//! What a session's mode adds to every prompt: the user's instructions plus the mode's files,
//! either as full text (small modes) or as the passages that match the question (large modes,
//! ADR 0011). Built once per session and per mode change; the first use waits for the load, and
//! retrieval at answer time is one embedding call plus a dot product per chunk.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use sotto_rag::{
    EmbedError, EmbedKind, Embedder, IndexedChunk, VectorIndex, estimate_tokens, truncate_to_tokens,
};
use tokio::sync::{OnceCell, broadcast};
use uuid::Uuid;

use super::store::StoreAccess;
use super::{Notice, NoticeKind};

/// A mode whose files fit in this many estimated tokens goes into the prompt as full text, in
/// the cacheable prefix; larger modes use retrieval.
pub(crate) const FULL_TEXT_MAX_TOKENS: u32 = 30_000;
const TOP_K: usize = 6;
/// With the pinned model, unrelated questions scored up to 0.24 against a small test corpus and
/// matching ones from 0.35 (ADR 0011); provisional until real documents are measured.
const MIN_SCORE: f32 = 0.3;
const RETRIEVED_MAX_TOKENS: u32 = 3_000;
/// Budget for the start-of-file fallback, shared by all files of the mode.
const FALLBACK_MAX_TOKENS: u32 = 8_000;
const FALLBACK_MIN_FILE_TOKENS: u32 = 400;
const QUERY_MAX_TOKENS: u32 = 400;
/// An automatic answer must not wait longer than this for the embedding of its question.
const QUERY_TIMEOUT: Duration = Duration::from_millis(800);
const WARMUP_TIMEOUT: Duration = Duration::from_secs(2);
const QUERY_CACHE: usize = 32;

/// Remembers the last notice about retrieval so the same problem is announced once, not on every
/// answer.
#[derive(Debug)]
pub(crate) struct NoticeOnce {
    notices: broadcast::Sender<Notice>,
    last: Mutex<Option<String>>,
}

impl NoticeOnce {
    pub fn new(notices: broadcast::Sender<Notice>) -> Self {
        Self {
            notices,
            last: Mutex::new(None),
        }
    }

    pub fn announce(&self, kind: NoticeKind, message: String) {
        let mut last = self.last.lock().unwrap_or_else(PoisonError::into_inner);
        if last.as_deref() == Some(message.as_str()) {
            return;
        }
        let _ = self.notices.send(Notice {
            kind,
            message: message.clone(),
        });
        *last = Some(message);
    }

    /// Retrieval works again, so the next problem is new.
    pub fn clear(&self) {
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = None;
    }
}

/// The part of a mode the context needs; plain data copied from the mode state.
#[derive(Debug, Clone)]
pub(crate) struct ModeSource {
    pub mode_id: Uuid,
    pub instructions: String,
    pub file_count: usize,
    pub total_tokens: u32,
    /// Every file has stored embeddings from the pinned model.
    pub fully_indexed: bool,
    /// Indexing a file failed; the user already has a notice about it.
    pub index_failed: bool,
}

enum Material {
    None,
    Files(String),
    Heads(String),
    Retrieval { index: VectorIndex, heads: String },
}

pub(crate) struct ModeContext {
    source: ModeSource,
    access: StoreAccess,
    embedder: Arc<dyn Embedder>,
    notes: Arc<NoticeOnce>,
    material: OnceCell<Material>,
    queries: Mutex<VecDeque<(String, Vec<f32>)>>,
}

impl std::fmt::Debug for ModeContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModeContext")
            .field("mode_id", &self.source.mode_id)
            .finish_non_exhaustive()
    }
}

/// What a stream task needs to fill in the mode part of its request.
#[derive(Debug, Clone)]
pub(crate) struct ModeQuery {
    pub context: Arc<ModeContext>,
    pub query: String,
}

fn embed_problem(e: &EmbedError, install: &str) -> String {
    match e {
        EmbedError::Unreachable => {
            "Ollama is not running, so the mode files are not searched. Answers use the start of each file instead".into()
        }
        EmbedError::ModelMissing { .. } => format!(
            "The embedding model is not installed. Run `{install}` to search the mode files; until then answers use the start of each file"
        ),
        EmbedError::DigestMismatch { name, expected, found } => format!(
            "The installed {name} ({found}) is not the version Sotto is pinned to ({expected}), so it is not used. Answers use the start of each file instead"
        ),
        other => format!("Searching the mode files failed ({other}). Answers use the start of each file instead"),
    }
}

fn file_block(name: &str, text: &str) -> String {
    format!("<file name=\"{name}\">\n{text}\n</file>\n")
}

/// The start of every file, sharing `FALLBACK_MAX_TOKENS`.
fn heads_text(files: &[(String, String)]) -> String {
    let share = (FALLBACK_MAX_TOKENS / u32::try_from(files.len().max(1)).unwrap_or(1))
        .max(FALLBACK_MIN_FILE_TOKENS);
    files
        .iter()
        .map(|(name, text)| file_block(name, truncate_to_tokens(text, share)))
        .collect()
}

/// Head size to ask the database for, in characters (a token is at least one).
fn head_chars(file_count: usize) -> u32 {
    let share = (FALLBACK_MAX_TOKENS / u32::try_from(file_count.max(1)).unwrap_or(1))
        .max(FALLBACK_MIN_FILE_TOKENS);
    share.saturating_mul(4)
}

fn passages_text(hits: &[sotto_rag::Hit<'_>]) -> String {
    let mut chosen: Vec<&sotto_rag::Hit<'_>> = Vec::new();
    let mut tokens = 0_u32;
    for hit in hits {
        let cost = estimate_tokens(hit.text);
        if tokens + cost > RETRIEVED_MAX_TOKENS && !chosen.is_empty() {
            break;
        }
        tokens += cost;
        chosen.push(hit);
    }
    chosen.sort_by(|a, b| a.file.cmp(b.file).then(a.ordinal.cmp(&b.ordinal)));
    chosen.iter().map(|h| file_block(h.file, h.text)).collect()
}

impl ModeContext {
    pub fn new(
        source: ModeSource,
        access: StoreAccess,
        embedder: Arc<dyn Embedder>,
        notes: Arc<NoticeOnce>,
    ) -> Arc<Self> {
        Arc::new(Self {
            source,
            access,
            embedder,
            notes,
            material: OnceCell::new(),
            queries: Mutex::new(VecDeque::new()),
        })
    }

    /// Loads the material now, so the first answer does not wait for it.
    pub fn preload(self: &Arc<Self>) {
        let ctx = Arc::clone(self);
        tokio::spawn(async move {
            ctx.material().await;
        });
    }

    async fn material(&self) -> &Material {
        self.material.get_or_init(|| self.load()).await
    }

    async fn load(&self) -> Material {
        let src = &self.source;
        if src.file_count == 0 {
            return Material::None;
        }
        let id = src.mode_id;
        if src.total_tokens <= FULL_TEXT_MAX_TOKENS {
            return match self.access.query(move |db| db.list_mode_files(id)).await {
                Some(Ok(files)) => Material::Files(
                    files
                        .iter()
                        .map(|f| file_block(&f.file_name, &f.text))
                        .collect(),
                ),
                _ => {
                    self.notes.announce(
                        NoticeKind::Modes,
                        "Could not read the mode files; answers go without them".into(),
                    );
                    Material::None
                }
            };
        }
        let chars = head_chars(src.file_count);
        let heads = match self
            .access
            .query(move |db| db.mode_file_heads(id, chars))
            .await
        {
            Some(Ok(files)) => heads_text(&files),
            _ => String::new(),
        };
        if !src.fully_indexed {
            if !src.index_failed {
                self.notes.announce(
                    NoticeKind::Modes,
                    "The mode files are still being indexed; answers use the start of each file for now".into(),
                );
            }
            return Material::Heads(heads);
        }
        if let Err(e) = self.embedder.verify().await {
            self.notes.announce(
                NoticeKind::Modes,
                embed_problem(&e, &self.embedder.pinned().pull_command()),
            );
            return Material::Heads(heads);
        }
        let dims = self.embedder.pinned().dimensions;
        let Some(Ok(stored)) = self.access.query(move |db| db.mode_chunks(id)).await else {
            return Material::Heads(heads);
        };
        let built = tokio::task::spawn_blocking(move || {
            let mut index = VectorIndex::new(dims);
            for c in stored {
                index.push(IndexedChunk {
                    file: c.file_name,
                    ordinal: c.ordinal,
                    text: c.text,
                    vector: c.embedding,
                });
            }
            index
        })
        .await;
        let Ok(index) = built else {
            return Material::Heads(heads);
        };
        if index.is_empty() {
            return Material::Heads(heads);
        }
        // Loads the model into memory so the first question of the meeting is as fast as the rest.
        let _ = tokio::time::timeout(
            WARMUP_TIMEOUT,
            self.embedder
                .embed(EmbedKind::Query, &["warm up".to_string()]),
        )
        .await;
        self.notes.clear();
        Material::Retrieval { index, heads }
    }

    fn cached_query(&self, query: &str) -> Option<Vec<f32>> {
        let cache = self.queries.lock().unwrap_or_else(PoisonError::into_inner);
        cache
            .iter()
            .find(|(q, _)| q == query)
            .map(|(_, v)| v.clone())
    }

    async fn query_vector(&self, query: &str) -> Result<Vec<f32>, EmbedError> {
        if let Some(v) = self.cached_query(query) {
            return Ok(v);
        }
        let inputs = [query.to_string()];
        let embedded = tokio::time::timeout(
            QUERY_TIMEOUT,
            self.embedder.embed(EmbedKind::Query, &inputs),
        )
        .await
        .map_err(|_| EmbedError::Unreachable)??;
        let vector = embedded.into_iter().next().ok_or(EmbedError::BadResponse)?;
        let mut cache = self.queries.lock().unwrap_or_else(PoisonError::into_inner);
        if cache.len() >= QUERY_CACHE {
            cache.pop_front();
        }
        cache.push_back((query.to_string(), vector.clone()));
        Ok(vector)
    }

    /// The mode text for one request. The stable part (instructions, and the files' full text or
    /// their first part when retrieval is unavailable) goes in the cacheable prefix; the passages
    /// that match `query` change with every question, so they travel with the question and leave
    /// the cached transcript history intact.
    pub async fn render(&self, query: &str) -> Rendered {
        let mut passages = String::new();
        let body = match self.material().await {
            Material::None => String::new(),
            Material::Files(text) => format!("Reference files from the user:\n{text}"),
            Material::Heads(text) => heads_section(text),
            Material::Retrieval { index, heads } => {
                let query = truncate_to_tokens(query.trim(), QUERY_MAX_TOKENS);
                if query.is_empty() {
                    String::new()
                } else {
                    match self.query_vector(query).await {
                        Ok(v) => {
                            let hits = index.search(&v, TOP_K, MIN_SCORE);
                            if !hits.is_empty() {
                                passages = format!(
                                    "Passages from the user's reference files that match the current question:\n{}",
                                    passages_text(&hits)
                                );
                            }
                            String::new()
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "query embedding failed; using file heads");
                            let message = if e == EmbedError::Unreachable {
                                "Searching the mode files timed out or Ollama stopped. Answers use the start of each file instead".to_string()
                            } else {
                                embed_problem(&e, &self.embedder.pinned().pull_command())
                            };
                            self.notes.announce(NoticeKind::Modes, message);
                            heads_section(heads)
                        }
                    }
                }
            }
        };
        let instructions = self.source.instructions.trim();
        let stable = match (instructions.is_empty(), body.is_empty()) {
            (true, _) => body,
            (false, true) => {
                format!("Instructions for this meeting, from the user:\n{instructions}")
            }
            (false, false) => {
                format!("Instructions for this meeting, from the user:\n{instructions}\n\n{body}")
            }
        };
        Rendered { stable, passages }
    }
}

/// The mode text of one request, split by how often it changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Rendered {
    pub stable: String,
    /// Empty unless retrieval found passages for this question.
    pub passages: String,
}

fn heads_section(text: &str) -> String {
    if text.is_empty() {
        String::new()
    } else {
        format!("Reference files from the user (only the start of each file is shown):\n{text}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hit<'a>(file: &'a str, ordinal: u32, text: &'a str) -> sotto_rag::Hit<'a> {
        sotto_rag::Hit {
            file,
            ordinal,
            text,
            score: 0.5,
        }
    }

    #[test]
    fn passages_are_ordered_by_file_and_position() {
        let hits = [
            hit("b.md", 0, "bb"),
            hit("a.md", 3, "a3"),
            hit("a.md", 1, "a1"),
        ];
        let text = passages_text(&hits);
        let a1 = text.find("a1").unwrap();
        let a3 = text.find("a3").unwrap();
        let bb = text.find("bb").unwrap();
        assert!(a1 < a3 && a3 < bb);
    }

    #[test]
    fn passages_stop_at_the_budget_but_keep_at_least_one() {
        let big = "x".repeat(usize::try_from(RETRIEVED_MAX_TOKENS).unwrap() * 4);
        let hits = [hit("a", 0, &big), hit("a", 1, "tail")];
        let text = passages_text(&hits);
        assert!(text.contains(&big) && !text.contains("tail"));
    }

    #[test]
    fn heads_share_the_budget() {
        let files: Vec<(String, String)> = (0..4)
            .map(|i| (format!("f{i}.txt"), "word ".repeat(10_000)))
            .collect();
        let text = heads_text(&files);
        assert!(estimate_tokens(&text) < FALLBACK_MAX_TOKENS + 200);
        for i in 0..4 {
            assert!(text.contains(&format!("f{i}.txt")));
        }
    }

    #[test]
    fn head_size_has_a_floor_for_many_files() {
        assert_eq!(head_chars(1), FALLBACK_MAX_TOKENS * 4);
        assert_eq!(head_chars(100), FALLBACK_MIN_FILE_TOKENS * 4);
    }

    #[test]
    fn notices_repeat_only_when_the_message_changes() {
        let (tx, mut rx) = broadcast::channel(8);
        let once = NoticeOnce::new(tx);
        once.announce(NoticeKind::Modes, "a".into());
        once.announce(NoticeKind::Modes, "a".into());
        once.announce(NoticeKind::Modes, "b".into());
        once.clear();
        once.announce(NoticeKind::Modes, "b".into());
        let got: Vec<String> =
            std::iter::from_fn(|| rx.try_recv().ok().map(|n| n.message)).collect();
        assert_eq!(got, ["a", "b", "b"]);
    }
}
