//! Local retrieval for mode files: text extraction, normalization, chunking, embeddings through
//! a pinned local model and brute-force cosine search (ADR 0004, ADR 0011). Pure: no database,
//! no app state.

pub mod chunk;
pub mod embed;
pub mod extract;
pub mod index;
pub mod normalize;
pub mod tokens;

pub use chunk::{Chunk, ChunkConfig, chunk};
pub use embed::{EmbedError, EmbedKind, Embedder, OllamaEmbedder, PinnedModel};
pub use extract::{ExtractError, FileKind, MAX_FILE_BYTES, extract};
pub use index::{Hit, IndexedChunk, VectorIndex, cosine};
pub use normalize::normalize;
pub use tokens::{estimate_tokens, truncate_to_tokens};
