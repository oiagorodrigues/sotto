# Architecture decision records

One file per decision that is expensive to reverse. Small or local calls go in `docs/decisions/LOG.md` instead.

Format: `NNNN-short-title.md` with Status, Context, Decision, Consequences and Alternatives. A superseded ADR stays in place with `Status: superseded by NNNN`.

| ADR | Decision |
|---|---|
| [0001](0001-app-shell.md) | Tauri 2 with a Rust core, React webview and a Swift sidecar |
| [0002](0002-backend.md) | Rust backend (Axum + Postgres) that never stores meeting content |
| [0003](0003-transcription.md) | On-device SpeechTranscriber by default, cloud streaming only with opt-in |
| [0004](0004-local-data.md) | Local history in SQLCipher, brute-force vector search, sync-ready schema |
| [0005](0005-keys-and-hosted-mode.md) | BYOK keys in the Keychain, hosted mode through ephemeral tokens |
| [0006](0006-auto-answer.md) | Hidden speculative answers started from partial transcripts |
| [0007](0007-sidecar-process-model.md) | Sidecar process, gated by a permissions spike |
| [0008](0008-device-login.md) | Device login with state and PKCE instead of a token in a deep link |
| [0010](0010-cloud-transcription.md) | Cloud transcription through OpenAI Realtime with client-side turns |
| [0011](0011-mode-retrieval.md) | Mode files in the prompt or through local retrieval with a pinned embedding model |
