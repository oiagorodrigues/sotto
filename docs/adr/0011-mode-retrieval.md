# 0011: Mode files in the prompt or through local retrieval with a pinned embedding model

Status: accepted (2026-11-06)

## Context

A mode is a name, a system prompt and attached files. Small file sets belong in the prompt as full text, where the provider can cache them. Large ones do not fit, so each answer needs the passages that match the question, found fast enough for the automatic-answer latency target (ADR 0006) and without sending the files anywhere (ADR 0004). Files come from the user's disk, so the webview must never handle paths or contents, and a model that is replaced under a tag must not silently change the vectors the app compares.

## Decision

- Files: `.txt`, `.md` and `.pdf`, at most 10 MB each, 25 files and about 4 million estimated tokens per mode. A native file panel opens from the Rust shell and hands paths to the core through an API; the webview only sends `PickModeFiles { mode_id }`. The core reads, checks and extracts text off the event loop and stores it in `mode_files`. Text decoding is UTF-8, UTF-16 with a byte order mark, or Windows-1252.
- PDF text comes from `pdf-extract` 0.12.1 (MIT, last push 2026-09-16), which pins `lopdf` 0.42, the first release that fixes the nested-object stack overflow (GHSA-6fj4-rcr3-r7rr). Parsing runs on its own thread with a 32 MiB stack and a 30 s timeout, so a panic or a hang fails one file. The GitHub advisory database lists no other open advisory for either crate (checked 2026-11-06). A stack overflow elsewhere in the parser would abort the process and cannot be caught; no such case is known, and it is unverified for the parser code outside `lopdf`.
- Threshold: a mode whose files total at most 30,000 estimated tokens (bytes / 4) goes into the prompt as full text, in the mode block before the transcript, so the bytes are identical for every request and the provider caches them. Above that, files are chunked and embedded.
- Chunks: windows of 320 estimated tokens (about 1,300 bytes) with 48 tokens of overlap, cut at paragraph, sentence, word and character boundaries in that order. 320 stays under the 512-token context of the chosen model with room for Portuguese, which tokenizes worse than the estimate assumes.
- Embedding model: `nomic-embed-text-v2-moe` on the local Ollama, 768 dimensions, MIT license (as `ollama show` reports it), 958 MB on disk. `config/embedding.toml` records its name, the SHA-256 of its manifest, the dimensions and its task prefixes (`search_query: `, `search_document: `). The core calls `/api/tags` and refuses the model unless the installed digest equals the recorded one; on a mismatch, a missing model or a stopped server it shows a notice once and answers with the start of each file (8,000 estimated tokens shared by all files, at least 400 per file).
- Storage: vectors are unit-length `f32` BLOBs in `chunks`, in the encrypted database (ADR 0004). The model identity that made them is stored in the `rag_embedding` setting; if it differs from the pinned one at start, the vectors are dropped and files are indexed again, so vectors from two models never mix.
- At answer time: the question is embedded once (800 ms limit, cached per session), searched against the mode's vectors held in memory since the session started (brute force, top 6, minimum cosine 0.3, at most 3,000 tokens), and the passages go in front of the question in the request, after the transcript history. The mode block (instructions and, for small modes, full text) stays byte-identical, so the cache breakpoints keep working. The model is loaded into Ollama's memory at session start and kept there for 30 minutes.
- Deleting a mode or a file leaves a tombstone with id, name and timestamps and erases the prompt, the file text and the vectors.

## Consequences

- Retrieval adds one local embedding call (10 to 50 ms warm, measured) before the first token of an answer, and none for modes that fit in the prompt.
- A user needs Ollama and `ollama pull nomic-embed-text-v2-moe` for modes above 30k tokens. Without it they still get answers from the start of each file.
- When the pinned model is updated upstream, the digest check refuses the new version until `config/embedding.toml` is updated and the vectors are rebuilt. That is the intent.
- The minimum score of 0.3 comes from a 12-passage, 12-question pt-BR and English sample; revisit it with real documents.
- Embeddings of document text sit in the encrypted database next to the text, so a mode takes about five times the size of its text on disk (the text, the chunk copy and 3 KB of vector per chunk).

## Alternatives

- `embeddinggemma` (622 MB, 10/12 top-1 on the sample, same speed): smaller, but the Gemma terms of use restrict uses and carry flow-down obligations.
- `bge-m3` (1.2 GB, 11/12, 8k context): twice as slow to index (21 against 42 chunks per second) and 1,024 dimensions.
- A provider embedding API: sends the user's files to a third party.
- `sqlite-vec`: rejected in ADR 0004.
- Putting retrieved passages in the cached prefix: simpler, but every new question would invalidate the cached transcript history that follows it.
