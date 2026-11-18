//! Background indexing of mode files: split into chunks, embed in small batches with progress,
//! store the vectors in the encrypted database. One job runs at a time so a large import cannot
//! starve the embedding calls that live answers need.

use std::sync::Arc;

use sotto_rag::{ChunkConfig, EmbedKind, Embedder, chunk};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::modes::ModesMsg;
use super::settings::BgMsg;
use super::store::StoreAccess;
use crate::storage::NewChunk;

const BATCH: usize = 16;
pub(crate) const IDENTITY_KEY: &str = "rag_embedding";

/// What the stored vectors were made with; vectors from another model are not comparable.
pub(crate) fn identity(embedder: &dyn Embedder) -> serde_json::Value {
    let p = embedder.pinned();
    serde_json::json!({ "name": p.name, "digest": p.digest })
}

pub(crate) struct IndexJob {
    pub files: Vec<Uuid>,
    /// Chunks from another embedding model are dropped before the first new one is stored.
    pub purge_stale: bool,
    pub embedder: Arc<dyn Embedder>,
    pub access: StoreAccess,
    pub out: mpsc::UnboundedSender<BgMsg>,
    pub cancel: CancellationToken,
    pub now_ms: i64,
}

impl IndexJob {
    fn send(&self, msg: ModesMsg) {
        let _ = self.out.send(BgMsg::Modes(msg));
    }

    fn fail_all(&self, message: &str) {
        for file_id in &self.files {
            self.send(ModesMsg::IndexFailed {
                file_id: *file_id,
                message: message.to_string(),
            });
        }
    }

    pub async fn run(self) {
        self.index_files().await;
        self.send(ModesMsg::JobDone);
    }

    async fn index_files(&self) {
        if let Err(e) = self.embedder.verify().await {
            tracing::warn!(error = %e, "embedding model unavailable for indexing");
            self.fail_all(&e.to_string());
            return;
        }
        let ident = identity(self.embedder.as_ref());
        let purge = self.purge_stale;
        let now = self.now_ms;
        let prepared = self
            .access
            .query(move |db| {
                if purge {
                    db.clear_chunks()?;
                }
                db.set_setting(IDENTITY_KEY, &ident, now)
            })
            .await;
        if !matches!(prepared, Some(Ok(()))) {
            self.fail_all("The local database could not be updated");
            return;
        }
        for file_id in &self.files {
            if self.cancel.is_cancelled() {
                return;
            }
            self.index_one(*file_id).await;
        }
    }

    async fn index_one(&self, file_id: Uuid) {
        let text = match self
            .access
            .query(move |db| db.mode_file_text(file_id))
            .await
        {
            Some(Ok(Some((_, text)))) => text,
            // Deleted while the job was queued.
            Some(Ok(None)) => return,
            _ => {
                self.send(ModesMsg::IndexFailed {
                    file_id,
                    message: "The file could not be read from the local database".into(),
                });
                return;
            }
        };
        let Ok(chunks) =
            tokio::task::spawn_blocking(move || chunk(&text, &ChunkConfig::default())).await
        else {
            self.send(ModesMsg::IndexFailed {
                file_id,
                message: "The file could not be split into passages".into(),
            });
            return;
        };
        let total = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
        self.send(ModesMsg::Progress {
            file_id,
            done: 0,
            total,
        });
        let mut stored = Vec::with_capacity(chunks.len());
        for batch in chunks.chunks(BATCH) {
            if self.cancel.is_cancelled() {
                return;
            }
            let texts: Vec<String> = batch.iter().map(|c| c.text.clone()).collect();
            let vectors = match self.embedder.embed(EmbedKind::Document, &texts).await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "embedding batch failed");
                    self.send(ModesMsg::IndexFailed {
                        file_id,
                        message: e.to_string(),
                    });
                    return;
                }
            };
            for (c, embedding) in batch.iter().zip(vectors) {
                stored.push(NewChunk {
                    ordinal: c.ordinal,
                    text: c.text.clone(),
                    embedding,
                });
            }
            self.send(ModesMsg::Progress {
                file_id,
                done: u32::try_from(stored.len()).unwrap_or(u32::MAX),
                total,
            });
        }
        let now = self.now_ms;
        let count = stored.len();
        match self
            .access
            .query(move |db| db.replace_chunks_if_live(file_id, &stored, now))
            .await
        {
            Some(Ok(true)) => {
                tracing::info!(chunks = count, "file indexed");
                self.send(ModesMsg::Indexed {
                    file_id,
                    chunks: u32::try_from(count).unwrap_or(u32::MAX),
                });
            }
            Some(Ok(false)) => {}
            _ => self.send(ModesMsg::IndexFailed {
                file_id,
                message: "The passages could not be saved".into(),
            }),
        }
    }
}
