# Sotto

A Cluely-style meeting copilot for macOS, written from scratch. Sotto listens to both sides of a call, transcribes on the device, detects when someone asks you a question and prepares an answer before they finish speaking. The overlay docks to the notch and is hidden from screen sharing and recordings.

The name comes from *sotto voce*, "in a low voice".

## What it does

- Captures the microphone and system audio (Core Audio process taps) through a Swift helper, with voice-processing echo cancellation.
- Transcribes on the device with Apple's `SpeechTranscriber` (macOS 26+). Cloud transcription through OpenAI Realtime is opt-in, per device, and never a silent fallback.
- Detects questions from partial transcripts and starts a hidden speculative answer, revealed when the turn ends.
- Answers with your own keys (Anthropic, OpenAI) or a local model through Ollama. Keys live in the Keychain.
- Modes: a system prompt plus files (txt, md, pdf), with local retrieval through a pinned Ollama embedding model for large files.
- Google Calendar: a prompt to start a session when a meeting begins, titled after the event.
- Onboarding with consent, permissions and an on-device transcription check.
- Optional backend for accounts, device sign-in (state plus PKCE), preference sync and short-lived provider tokens. It stores no meeting content; history stays local in an encrypted SQLite database.

## Stack

- `apps/desktop`: Tauri 2 shell and a React 19 + Vite webview.
- `crates/sotto-core`: the Rust core (sessions, answers, providers, storage, sidecar, account, calendar, cloud transcription).
- `crates/sotto-protocol`: the contract between core, webview and sidecar; TypeScript types are generated with ts-rs.
- `crates/sotto-detect`, `crates/sotto-rag`: question detection and retrieval.
- `native/sotto-audio`: the Swift audio and speech sidecar.
- `services/api`: Axum + sqlx + Postgres backend, in its own Cargo workspace.

## Running it

Requires macOS 26+, Xcode 27.1, rustup and `just` from Homebrew, pnpm, and Docker for the backend.

```sh
just check     # fmt, clippy, all tests (Rust, Swift, API on Postgres), web typecheck and lint
just dev       # desktop app with a fresh sidecar build
just api-dev   # backend on http://localhost:8080; login mails show in Mailpit at http://localhost:8025
```

For local answers, install Ollama and pull `llama3.1:8b`; mode retrieval needs `nomic-embed-text-v2-moe`.

## Status

A learning project, built in vertical slices. Slice 1 (capture, transcription, answers, notch overlay) and slice 2 (backend, onboarding, modes, calendar, cloud transcription) are merged on `main`. `slice2/integrate` is unfinished work that has not passed the checks. Several parts have only run against fakes so far; `docs/decisions/LOG.md` lists what is verified and what is not.

## Docs

- `docs/design.md`: goals, requirements and the slice plan.
- `docs/adr/`: decisions that are expensive to reverse.
- `docs/decisions/LOG.md`: smaller decisions and the lessons every contributor reads first.
- `docs/security-checklist.md`: security requirements and how each is checked.
- `docs/DESIGN_SYSTEM.md`, `docs/design/ux-rules.md`: the visual system and UX rules.
