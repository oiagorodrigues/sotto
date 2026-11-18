# Architecture

Sotto has four runtime components and one shared contract. The full rationale is in `docs/design.md` and the ADRs; this page is the map.

| Component | Path | Language | Owns |
|---|---|---|---|
| Audio sidecar | `native/sotto-audio` | Swift 6 | Mic (voice processing), system audio (process tap), on-device transcription (SpeechAnalyzer), screenshots (ScreenCaptureKit), permission checks |
| Core | `crates/sotto-core` | Rust | All app state, sessions, question detection (`crates/sotto-detect`), answer orchestration, LLM providers, Keychain, encrypted SQLite, sidecar process |
| Desktop shell | `apps/desktop/src-tauri` | Rust | Tauri windows and panels, capabilities, CSP, the `dispatch` command and `sotto://snapshot` event, glue to the core |
| Webview | `apps/desktop/src` | TypeScript, React | Rendering snapshots, sending intents. No network, no secrets |
| Backend (slice 2) | `services/api` | Rust | Accounts, device login, ephemeral tokens, preference sync. No meeting content |
| Contract | `crates/sotto-protocol` | Rust, exported to TS | Sidecar commands and events, UI snapshot and intents |

## Data flow during a session

1. The core spawns `sotto-audio` by absolute path and sends `start`.
2. The sidecar streams `transcript` revisions for `me` and `them` on one clock.
3. The core feeds "them" revisions to `sotto-detect`. A likely question starts a hidden speculative answer; a classifier confirms in parallel; at end of turn the answer is revealed or restarted (ADR 0006).
4. The core streams provider tokens into the snapshot; the webview renders them.
5. Manual asks optionally request a screenshot from the sidecar first.
6. Everything persists to the encrypted local database; nothing about the meeting goes to the backend.

## Trust boundaries

- Webview to core: untrusted input. Typed intents, `deny_unknown_fields`, `Intent::validate`, deny-by-default capabilities, strict CSP.
- Sidecar to core: same-team signed child process; its JSON is still parsed strictly and bounded in size.
- Core to providers: HTTPS with the user's key from the Keychain.
- Core to backend: HTTPS with an opaque access token; only account and preference data.
- Deep links: only `sotto://auth/callback` with a matching state and PKCE verifier (ADR 0008).
