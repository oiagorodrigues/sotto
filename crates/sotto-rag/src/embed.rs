//! Embeddings through a local Ollama server with a pinned model. The installed model's digest
//! must match the one recorded in `config/embedding.toml` before the core uses it, so a tag that
//! was repointed or a swapped model file is refused instead of silently changing every vector
//! (ADR 0011).

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;

use crate::index::unit;

const BUNDLED: &str = include_str!("../../../config/embedding.toml");

pub const OLLAMA_URL: &str = "http://127.0.0.1:11434";

const TAGS_TIMEOUT: Duration = Duration::from_secs(3);
const EMBED_TIMEOUT: Duration = Duration::from_secs(60);
/// Keeps the model resident between questions so a live answer never pays the load time.
const KEEP_ALIVE: &str = "30m";

/// The embedding model the app is built and tested against.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedModel {
    pub name: String,
    /// Full SHA-256 of the model manifest, as `/api/tags` reports it.
    pub digest: String,
    pub dimensions: usize,
    /// Task prefixes the model was trained with; empty when it uses none.
    #[serde(default)]
    pub query_prefix: String,
    #[serde(default)]
    pub document_prefix: String,
    #[serde(default)]
    pub as_of: String,
}

impl PinnedModel {
    pub fn bundled() -> Result<Self, EmbedError> {
        Self::parse(BUNDLED)
    }

    pub fn parse(text: &str) -> Result<Self, EmbedError> {
        let model: Self = toml::from_str(text).map_err(|e| EmbedError::Config(e.to_string()))?;
        let digest_ok =
            model.digest.len() == 64 && model.digest.chars().all(|c| c.is_ascii_hexdigit());
        if !digest_ok || model.name.is_empty() || model.dimensions == 0 {
            return Err(EmbedError::Config(
                "name, a 64-character hex digest and dimensions are required".into(),
            ));
        }
        Ok(model)
    }

    /// What the user runs to install the model.
    #[must_use]
    pub fn pull_command(&self) -> String {
        format!("ollama pull {}", self.name)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EmbedError {
    #[error("Ollama is not running")]
    Unreachable,
    #[error("the embedding model {name} is not installed")]
    ModelMissing { name: String },
    #[error(
        "the installed {name} does not match the pinned version ({found} instead of {expected})"
    )]
    DigestMismatch {
        name: String,
        expected: String,
        found: String,
    },
    #[error("Ollama answered with status {0}")]
    Status(u16),
    #[error("Ollama sent an unexpected response")]
    BadResponse,
    #[error("the embedding has {found} dimensions, expected {expected}")]
    Dimensions { expected: usize, found: usize },
    #[error("embedding configuration: {0}")]
    Config(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbedKind {
    Document,
    Query,
}

#[async_trait]
pub trait Embedder: Send + Sync + std::fmt::Debug {
    /// The model whose vectors this embedder produces.
    fn pinned(&self) -> &PinnedModel;

    /// Checks the server is up and the installed model is the pinned one.
    async fn verify(&self) -> Result<(), EmbedError>;

    /// One unit-length vector per input, in order.
    async fn embed(&self, kind: EmbedKind, inputs: &[String]) -> Result<Vec<Vec<f32>>, EmbedError>;
}

#[derive(Deserialize)]
struct Tags {
    #[serde(default)]
    models: Vec<TagModel>,
}

#[derive(Deserialize)]
struct TagModel {
    #[serde(default)]
    name: String,
    #[serde(default)]
    digest: String,
}

fn base_name(name: &str) -> &str {
    name.strip_suffix(":latest").unwrap_or(name)
}

fn short(digest: &str) -> String {
    digest.chars().take(12).collect()
}

/// Checks an `/api/tags` body against the pinned model.
pub fn check_installed(tags_body: &str, pinned: &PinnedModel) -> Result<(), EmbedError> {
    let tags: Tags = serde_json::from_str(tags_body).map_err(|_| EmbedError::BadResponse)?;
    let installed = tags
        .models
        .iter()
        .find(|m| base_name(&m.name) == base_name(&pinned.name))
        .ok_or_else(|| EmbedError::ModelMissing {
            name: pinned.name.clone(),
        })?;
    let found = installed
        .digest
        .strip_prefix("sha256:")
        .unwrap_or(&installed.digest);
    if found.eq_ignore_ascii_case(&pinned.digest) {
        Ok(())
    } else {
        Err(EmbedError::DigestMismatch {
            name: pinned.name.clone(),
            expected: short(&pinned.digest),
            found: short(found),
        })
    }
}

#[derive(Debug, Clone)]
pub struct OllamaEmbedder {
    client: reqwest::Client,
    base_url: String,
    pinned: PinnedModel,
}

impl OllamaEmbedder {
    pub fn new(pinned: PinnedModel) -> Result<Self, EmbedError> {
        let client = reqwest::Client::builder()
            .timeout(EMBED_TIMEOUT)
            .build()
            .map_err(|e| EmbedError::Config(e.to_string()))?;
        Ok(Self {
            client,
            base_url: OLLAMA_URL.to_string(),
            pinned,
        })
    }

    /// The bundled pinned model on the default local server.
    pub fn bundled() -> Result<Self, EmbedError> {
        Self::new(PinnedModel::bundled()?)
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    fn prefixed(&self, kind: EmbedKind, text: &str) -> String {
        let prefix = match kind {
            EmbedKind::Document => &self.pinned.document_prefix,
            EmbedKind::Query => &self.pinned.query_prefix,
        };
        // The server rejects an empty input.
        if text.is_empty() && prefix.is_empty() {
            " ".to_string()
        } else {
            format!("{prefix}{text}")
        }
    }
}

fn transport(e: &reqwest::Error) -> EmbedError {
    if e.is_connect() || e.is_timeout() {
        EmbedError::Unreachable
    } else {
        EmbedError::BadResponse
    }
}

#[derive(Deserialize)]
struct EmbedResponse {
    embeddings: Vec<Vec<f32>>,
}

#[async_trait]
impl Embedder for OllamaEmbedder {
    fn pinned(&self) -> &PinnedModel {
        &self.pinned
    }

    async fn verify(&self) -> Result<(), EmbedError> {
        let resp = self
            .client
            .get(format!("{}/api/tags", self.base_url))
            .timeout(TAGS_TIMEOUT)
            .send()
            .await
            .map_err(|e| transport(&e))?;
        if !resp.status().is_success() {
            return Err(EmbedError::Status(resp.status().as_u16()));
        }
        let body = resp.text().await.map_err(|e| transport(&e))?;
        check_installed(&body, &self.pinned)
    }

    async fn embed(&self, kind: EmbedKind, inputs: &[String]) -> Result<Vec<Vec<f32>>, EmbedError> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let input: Vec<String> = inputs.iter().map(|t| self.prefixed(kind, t)).collect();
        let resp = self
            .client
            .post(format!("{}/api/embed", self.base_url))
            .json(&serde_json::json!({
                "model": self.pinned.name,
                "input": input,
                "truncate": true,
                "keep_alive": KEEP_ALIVE,
            }))
            .send()
            .await
            .map_err(|e| transport(&e))?;
        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(EmbedError::ModelMissing {
                name: self.pinned.name.clone(),
            });
        }
        if !status.is_success() {
            return Err(EmbedError::Status(status.as_u16()));
        }
        let body: EmbedResponse = resp.json().await.map_err(|_| EmbedError::BadResponse)?;
        if body.embeddings.len() != inputs.len() {
            return Err(EmbedError::BadResponse);
        }
        body.embeddings
            .iter()
            .map(|v| {
                if v.len() != self.pinned.dimensions {
                    return Err(EmbedError::Dimensions {
                        expected: self.pinned.dimensions,
                        found: v.len(),
                    });
                }
                unit(v).ok_or(EmbedError::BadResponse)
            })
            .collect()
    }
}

#[cfg(any(test, feature = "test-support"))]
pub mod fake {
    //! A deterministic embedder for tests: hashed bag of words, so texts that share words are
    //! close. No network.

    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use async_trait::async_trait;

    use super::{EmbedError, EmbedKind, Embedder, PinnedModel};
    use crate::index::unit;

    pub const DIMENSIONS: usize = 128;

    #[derive(Debug)]
    pub struct FakeEmbedder {
        pinned: PinnedModel,
        verify_error: Mutex<Option<EmbedError>>,
        embed_error: Mutex<Option<EmbedError>>,
        delay: Mutex<Duration>,
        pub embed_calls: AtomicUsize,
        pub inputs_embedded: AtomicUsize,
    }

    impl Default for FakeEmbedder {
        fn default() -> Self {
            Self {
                pinned: PinnedModel {
                    name: "fake-embed".into(),
                    digest: "0".repeat(64),
                    dimensions: DIMENSIONS,
                    query_prefix: String::new(),
                    document_prefix: String::new(),
                    as_of: String::new(),
                },
                verify_error: Mutex::new(None),
                embed_error: Mutex::new(None),
                delay: Mutex::new(Duration::ZERO),
                embed_calls: AtomicUsize::new(0),
                inputs_embedded: AtomicUsize::new(0),
            }
        }
    }

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    impl FakeEmbedder {
        pub fn fail_verify(&self, e: Option<EmbedError>) {
            *lock(&self.verify_error) = e;
        }

        pub fn fail_embed(&self, e: Option<EmbedError>) {
            *lock(&self.embed_error) = e;
        }

        pub fn set_delay(&self, d: Duration) {
            *lock(&self.delay) = d;
        }

        #[must_use]
        pub fn vector(text: &str) -> Vec<f32> {
            let mut v = vec![0.0_f32; DIMENSIONS];
            for word in text
                .to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.len() > 2)
            {
                let h = word.bytes().fold(14_695_981_039_346_656_037_u64, |h, b| {
                    (h ^ u64::from(b)).wrapping_mul(1_099_511_628_211)
                });
                let slot = usize::try_from(h % DIMENSIONS as u64).unwrap_or(0);
                v[slot] += 1.0;
            }
            unit(&v).unwrap_or(v)
        }
    }

    #[async_trait]
    impl Embedder for FakeEmbedder {
        fn pinned(&self) -> &PinnedModel {
            &self.pinned
        }

        async fn verify(&self) -> Result<(), EmbedError> {
            lock(&self.verify_error).clone().map_or(Ok(()), Err)
        }

        async fn embed(
            &self,
            _kind: EmbedKind,
            inputs: &[String],
        ) -> Result<Vec<Vec<f32>>, EmbedError> {
            self.embed_calls.fetch_add(1, Ordering::SeqCst);
            let delay = *lock(&self.delay);
            if !delay.is_zero() {
                tokio::time::sleep(delay).await;
            }
            if let Some(e) = lock(&self.embed_error).clone() {
                return Err(e);
            }
            self.inputs_embedded
                .fetch_add(inputs.len(), Ordering::SeqCst);
            Ok(inputs.iter().map(|t| Self::vector(t)).collect())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn pinned() -> PinnedModel {
        PinnedModel {
            name: "embeddinggemma".into(),
            digest: "a".repeat(64),
            dimensions: 3,
            query_prefix: "query: ".into(),
            document_prefix: "doc: ".into(),
            as_of: String::new(),
        }
    }

    fn tags(name: &str, digest: &str) -> String {
        serde_json::json!({"models": [{"name": name, "digest": digest}]}).to_string()
    }

    #[test]
    fn the_bundled_pin_is_valid() {
        let m = PinnedModel::bundled().unwrap();
        assert_eq!(m.digest.len(), 64);
        assert!(m.dimensions > 0);
    }

    #[test]
    fn bad_pins_are_rejected() {
        assert!(PinnedModel::parse("name = \"x\"\ndigest = \"abc\"\ndimensions = 3").is_err());
        let zero_dims = format!(
            "name = \"x\"\ndigest = \"{}\"\ndimensions = 0",
            "a".repeat(64)
        );
        assert!(PinnedModel::parse(&zero_dims).is_err());
        let unknown = format!(
            "name = \"x\"\ndigest = \"{}\"\ndimensions = 3\nextra = 1",
            "a".repeat(64)
        );
        assert!(PinnedModel::parse(&unknown).is_err());
    }

    #[test]
    fn matching_digest_passes_with_or_without_latest() {
        let p = pinned();
        assert_eq!(
            check_installed(&tags("embeddinggemma:latest", &"a".repeat(64)), &p),
            Ok(())
        );
        assert_eq!(
            check_installed(&tags("embeddinggemma", &"A".repeat(64)), &p),
            Ok(())
        );
        let prefixed = format!("sha256:{}", "a".repeat(64));
        assert_eq!(
            check_installed(&tags("embeddinggemma", &prefixed), &p),
            Ok(())
        );
    }

    #[test]
    fn a_different_digest_is_refused() {
        let err = check_installed(&tags("embeddinggemma:latest", &"b".repeat(64)), &pinned());
        assert_eq!(
            err,
            Err(EmbedError::DigestMismatch {
                name: "embeddinggemma".into(),
                expected: "a".repeat(12),
                found: "b".repeat(12),
            })
        );
    }

    #[test]
    fn a_missing_model_or_a_bad_body_is_refused() {
        assert_eq!(
            check_installed(&tags("llama3.1:8b", &"a".repeat(64)), &pinned()),
            Err(EmbedError::ModelMissing {
                name: "embeddinggemma".into()
            })
        );
        assert_eq!(
            check_installed("{\"models\":[]}", &pinned()),
            Err(EmbedError::ModelMissing {
                name: "embeddinggemma".into()
            })
        );
        assert_eq!(
            check_installed("nope", &pinned()),
            Err(EmbedError::BadResponse)
        );
    }

    #[test]
    fn a_name_that_only_starts_the_same_is_not_the_model() {
        assert!(matches!(
            check_installed(&tags("embeddinggemma-x:latest", &"a".repeat(64)), &pinned()),
            Err(EmbedError::ModelMissing { .. })
        ));
    }

    #[tokio::test]
    async fn verify_reads_the_server_tags() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string(tags("embeddinggemma:latest", &"a".repeat(64))),
            )
            .mount(&server)
            .await;
        let e = OllamaEmbedder::new(pinned())
            .unwrap()
            .with_base_url(server.uri());
        assert_eq!(e.verify().await, Ok(()));
    }

    #[tokio::test]
    async fn verify_reports_a_server_that_is_down() {
        let e = OllamaEmbedder::new(pinned())
            .unwrap()
            .with_base_url("http://127.0.0.1:1");
        assert_eq!(e.verify().await, Err(EmbedError::Unreachable));
    }

    #[tokio::test]
    async fn embed_sends_prefixed_inputs_and_returns_unit_vectors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .and(wiremock::matchers::body_partial_json(serde_json::json!({
                "model": "embeddinggemma",
                "input": ["doc: first", "doc: second"],
                "keep_alive": "30m",
            })))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "embeddings": [[3.0, 0.0, 4.0], [0.0, 2.0, 0.0]]
            })))
            .mount(&server)
            .await;
        let e = OllamaEmbedder::new(pinned())
            .unwrap()
            .with_base_url(server.uri());
        let out = e
            .embed(EmbedKind::Document, &["first".into(), "second".into()])
            .await
            .unwrap();
        assert_eq!(out, vec![vec![0.6, 0.0, 0.8], vec![0.0, 1.0, 0.0]]);
    }

    #[tokio::test]
    async fn queries_use_the_query_prefix() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .and(wiremock::matchers::body_partial_json(
                serde_json::json!({"input": ["query: hi"]}),
            ))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"embeddings": [[1.0, 0.0, 0.0]]})),
            )
            .mount(&server)
            .await;
        let e = OllamaEmbedder::new(pinned())
            .unwrap()
            .with_base_url(server.uri());
        assert_eq!(
            e.embed(EmbedKind::Query, &["hi".into()])
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn wrong_dimensions_count_or_status_are_errors() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"embeddings": [[1.0, 0.0]]})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        let e = OllamaEmbedder::new(pinned())
            .unwrap()
            .with_base_url(server.uri());
        assert_eq!(
            e.embed(EmbedKind::Query, &["a".into()]).await,
            Err(EmbedError::Dimensions {
                expected: 3,
                found: 2
            })
        );
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(serde_json::json!({"embeddings": []})),
            )
            .up_to_n_times(1)
            .mount(&server)
            .await;
        assert_eq!(
            e.embed(EmbedKind::Query, &["a".into()]).await,
            Err(EmbedError::BadResponse)
        );
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;
        assert_eq!(
            e.embed(EmbedKind::Query, &["a".into()]).await,
            Err(EmbedError::Status(500))
        );
    }

    #[tokio::test]
    async fn a_missing_model_is_reported_by_name() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(ResponseTemplate::new(404))
            .mount(&server)
            .await;
        let e = OllamaEmbedder::new(pinned())
            .unwrap()
            .with_base_url(server.uri());
        assert_eq!(
            e.embed(EmbedKind::Query, &["a".into()]).await,
            Err(EmbedError::ModelMissing {
                name: "embeddinggemma".into()
            })
        );
    }

    #[tokio::test]
    async fn the_zero_vector_is_a_bad_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/embed"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_json(serde_json::json!({"embeddings": [[0.0, 0.0, 0.0]]})),
            )
            .mount(&server)
            .await;
        let e = OllamaEmbedder::new(pinned())
            .unwrap()
            .with_base_url(server.uri());
        assert_eq!(
            e.embed(EmbedKind::Query, &["a".into()]).await,
            Err(EmbedError::BadResponse)
        );
    }

    #[test]
    fn the_fake_embeds_shared_words_closer() {
        use crate::index::cosine;
        use fake::FakeEmbedder;
        let a = FakeEmbedder::vector("refund policy for annual plans");
        let b = FakeEmbedder::vector("what is the refund policy");
        let c = FakeEmbedder::vector("office parking rules");
        assert!(cosine(&a, &b).unwrap() > cosine(&a, &c).unwrap());
    }
}
