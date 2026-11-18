# 0004: Local history in SQLCipher, brute-force vector search, sync-ready schema

Status: accepted (2026-11-06)

## Context

History (sessions, transcript segments, messages), modes, mode files and their embeddings stay on the device in v1. Data at rest should not be readable by any process of the same user. `sqlite-vec` together with SQLCipher is unverified.

## Decision

- One SQLite database through `rusqlite` with the bundled SQLCipher feature. The 256-bit key is generated on first run and stored in the Keychain.
- Embeddings are stored as `f32` BLOBs in the same encrypted database. Retrieval is brute-force cosine similarity in Rust. A mode's files are at most a few thousand chunks, so this takes milliseconds, and it removes `sqlite-vec` from the dependency list.
- Schema is sync-ready: UUIDv7 primary keys, `created_at` and `updated_at` in UTC, soft-delete tombstones, no auto-increment IDs exposed.
- Retention: sessions older than N days are deleted (default 30), configurable, including "keep forever".

## Consequences

- Data at rest is encrypted independently of FileVault.
- If brute-force search ever becomes slow (it should not at this scale), revisit with an ANN index; until then there is no vector extension to keep compatible with SQLCipher.

## Alternatives

- Plain SQLite and rely on FileVault: simpler, but leaves the data readable by any same-user process.
- `sqlite-vec`: unverified with SQLCipher and not needed at this size.
- LanceDB: another storage engine to secure and back up.
