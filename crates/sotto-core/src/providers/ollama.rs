//! Ollama `/api/chat` NDJSON streaming, model listing and warm-model check.

use async_trait::async_trait;
use base64::Engine as _;
use reqwest::Client;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use super::wire::{self, LineParser, Parsed};
use super::{
    CompletionRequest, EventStream, LlmProvider, ProviderError, Result, StreamEvent, Thinking,
    Usage,
};

pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:11434";

#[derive(Debug)]
pub struct OllamaProvider {
    client: Client,
    base_url: String,
}

impl OllamaProvider {
    pub fn new() -> Result<Self> {
        Ok(Self {
            client: wire::build_client()?,
            base_url: DEFAULT_BASE_URL.to_string(),
        })
    }

    #[must_use]
    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_string();
        self
    }

    /// Names of the locally installed models (`/api/tags`).
    pub async fn list_models(&self) -> Result<Vec<String>> {
        self.model_names("/api/tags").await
    }

    /// Whether the model is currently loaded in memory (`/api/ps`). ADR 0006 uses a local
    /// classifier only when this is true, so the first call never pays a cold load.
    pub async fn is_model_loaded(&self, name: &str) -> Result<bool> {
        let loaded = self.model_names("/api/ps").await?;
        let with_tag = if name.contains(':') {
            name.to_string()
        } else {
            format!("{name}:latest")
        };
        Ok(loaded.iter().any(|m| m == name || *m == with_tag))
    }

    async fn model_names(&self, endpoint: &str) -> Result<Vec<String>> {
        let request = self.client.get(format!("{}{endpoint}", self.base_url));
        let resp = wire::send(request, &CancellationToken::new()).await?;
        let body: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Protocol(e.without_url().to_string()))?;
        let models = body
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(|| ProviderError::Protocol("missing models array".into()))?;
        Ok(models
            .iter()
            .filter_map(|m| {
                m.get("name")
                    .or_else(|| m.get("model"))
                    .and_then(Value::as_str)
            })
            .map(str::to_owned)
            .collect())
    }
}

pub(crate) fn build_body(req: &CompletionRequest) -> Value {
    let context = req.context_text();
    let system = [req.system_prefix.as_str(), context.as_str()]
        .iter()
        .filter(|s| !s.is_empty())
        .copied()
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut messages = Vec::new();
    if !system.is_empty() {
        messages.push(json!({"role": "system", "content": system}));
    }
    for t in &req.turns {
        messages.push(json!({"role": t.role.as_str(), "content": t.text}));
    }
    let mut user = json!({"role": "user", "content": req.user_text});
    if let Some(png) = &req.image_png {
        user["images"] = json!([base64::engine::general_purpose::STANDARD.encode(png)]);
    }
    messages.push(user);
    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "options": {"num_predict": req.max_tokens},
    });
    // qwen3 and other thinking models reason by default, which eats a short budget.
    if req.thinking == Thinking::Disabled {
        body["think"] = json!(false);
    }
    body
}

#[async_trait]
impl LlmProvider for OllamaProvider {
    async fn stream(
        &self,
        req: CompletionRequest,
        cancel: CancellationToken,
    ) -> Result<EventStream> {
        let request = self
            .client
            .post(format!("{}/api/chat", self.base_url))
            .json(&build_body(&req));
        let resp = wire::send(request, &cancel).await?;
        tracing::debug!(provider = "ollama", model = %req.model, "stream opened");
        Ok(wire::line_stream(resp, cancel, NdjsonParser::default()))
    }
}

#[derive(Default)]
struct NdjsonParser {
    saw_text: bool,
}

impl LineParser for NdjsonParser {
    fn line(&mut self, line: &str) -> Parsed {
        let mut out = Parsed::default();
        if line.trim().is_empty() {
            return out;
        }
        let v: Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(e) => {
                out.items
                    .push(Err(ProviderError::Protocol(format!("bad NDJSON: {e}"))));
                out.finished = true;
                return out;
            }
        };
        if let Some(err) = v.get("error").and_then(Value::as_str) {
            out.items.push(Err(ProviderError::Stream {
                kind: "ollama".into(),
                message: err.chars().take(300).collect(),
            }));
            out.finished = true;
            return out;
        }
        if let Some(t) = v.pointer("/message/content").and_then(Value::as_str)
            && !t.is_empty()
        {
            self.saw_text = true;
            out.items.push(Ok(StreamEvent::TextDelta(t.to_string())));
        }
        if v.get("done").and_then(Value::as_bool) == Some(true) {
            let get = |k: &str| v.get(k).and_then(Value::as_u64).unwrap_or(0);
            out.items.push(Ok(StreamEvent::from_usage(Usage {
                input_tokens: get("prompt_eval_count"),
                output_tokens: get("eval_count"),
                ..Usage::default()
            })));
            let reason = v
                .get("done_reason")
                .and_then(Value::as_str)
                .unwrap_or("stop");
            out.items.push(if reason == "length" && !self.saw_text {
                Err(ProviderError::NoTextAtTokenLimit)
            } else {
                Ok(StreamEvent::Done {
                    stop_reason: reason.to_string(),
                })
            });
            out.finished = true;
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use futures::StreamExt;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;

    const BODY: &str = "\
{\"model\":\"llama3.2\",\"created_at\":\"2026-10-06T10:00:00Z\",\"message\":{\"role\":\"assistant\",\"content\":\"Hel\"},\"done\":false}
{\"model\":\"llama3.2\",\"created_at\":\"2026-10-06T10:00:01Z\",\"message\":{\"role\":\"assistant\",\"content\":\"lo\"},\"done\":false}
{\"model\":\"llama3.2\",\"created_at\":\"2026-10-06T10:00:02Z\",\"message\":{\"role\":\"assistant\",\"content\":\"\"},\"done\":true,\"done_reason\":\"stop\",\"prompt_eval_count\":26,\"eval_count\":2}
";

    fn request() -> CompletionRequest {
        CompletionRequest {
            model: "llama3.2".into(),
            system_prefix: "prefix".into(),
            mode_context: String::new(),
            history: String::new(),
            recent: String::new(),
            turns: vec![],
            user_text: "hi".into(),
            image_png: Some(vec![1, 2, 3]),
            max_tokens: 64,
            effort: None,
            thinking: Thinking::Adaptive,
        }
    }

    #[test]
    fn classifier_turns_thinking_off() {
        let mut r = request();
        assert!(build_body(&r).get("think").is_none());
        r.thinking = Thinking::Disabled;
        assert_eq!(build_body(&r)["think"], false);
    }

    #[test]
    fn length_stop_with_no_text_is_an_error() {
        let mut p = NdjsonParser::default();
        let out = p.line(
            "{\"message\":{\"content\":\"\"},\"done\":true,\"done_reason\":\"length\",\"eval_count\":256}",
        );
        assert_eq!(
            out.items.last(),
            Some(&Err(ProviderError::NoTextAtTokenLimit))
        );
        let mut p = NdjsonParser::default();
        p.line("{\"message\":{\"content\":\"yes\"},\"done\":false}");
        let out = p.line("{\"done\":true,\"done_reason\":\"length\"}");
        assert!(matches!(
            out.items.last(),
            Some(Ok(StreamEvent::Done { .. }))
        ));
    }

    fn provider(server: &MockServer) -> OllamaProvider {
        OllamaProvider::new().unwrap().with_base_url(server.uri())
    }

    #[test]
    fn body_carries_images_and_num_predict() {
        let b = build_body(&request());
        assert_eq!(b["options"]["num_predict"], 64);
        assert_eq!(b["messages"][0]["content"], "prefix");
        assert_eq!(b["messages"][1]["images"][0], "AQID");
    }

    #[tokio::test]
    async fn streams_ndjson() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/chat"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_raw(BODY.as_bytes().to_vec(), "application/x-ndjson"),
            )
            .mount(&server)
            .await;
        let stream = provider(&server)
            .stream(request(), CancellationToken::new())
            .await
            .unwrap();
        let items: Vec<_> = stream
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<_>>()
            .unwrap();
        assert_eq!(
            items,
            vec![
                StreamEvent::TextDelta("Hel".into()),
                StreamEvent::TextDelta("lo".into()),
                StreamEvent::Usage {
                    input_tokens: 26,
                    output_tokens: 2,
                    cache_read_input_tokens: 0,
                    cache_creation_input_tokens: 0,
                },
                StreamEvent::Done {
                    stop_reason: "stop".into()
                },
            ]
        );
    }

    #[tokio::test]
    async fn unknown_model_is_an_http_error() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(404).set_body_string("{\"error\":\"model 'x' not found\"}"),
            )
            .mount(&server)
            .await;
        let err = provider(&server)
            .stream(request(), CancellationToken::new())
            .await
            .err()
            .unwrap();
        assert_eq!(
            err,
            ProviderError::Http {
                status: 404,
                message: "model 'x' not found".into()
            }
        );
    }

    #[tokio::test]
    async fn lists_models_and_checks_loaded() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/tags"))
            .respond_with(ResponseTemplate::new(200).set_body_string(
                "{\"models\":[{\"name\":\"llama3.2:latest\",\"model\":\"llama3.2:latest\"},{\"name\":\"qwen3:8b\"}]}",
            ))
            .mount(&server)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/ps"))
            .respond_with(
                ResponseTemplate::new(200)
                    .set_body_string("{\"models\":[{\"name\":\"llama3.2:latest\"}]}"),
            )
            .mount(&server)
            .await;
        let p = provider(&server);
        assert_eq!(
            p.list_models().await.unwrap(),
            vec!["llama3.2:latest", "qwen3:8b"]
        );
        assert!(p.is_model_loaded("llama3.2").await.unwrap());
        assert!(p.is_model_loaded("llama3.2:latest").await.unwrap());
        assert!(!p.is_model_loaded("qwen3:8b").await.unwrap());
    }

    #[tokio::test]
    async fn unreachable_server_is_a_network_error() {
        let p = OllamaProvider::new()
            .unwrap()
            .with_base_url("http://127.0.0.1:1");
        assert!(matches!(
            p.list_models().await,
            Err(ProviderError::Network(_))
        ));
    }
}
