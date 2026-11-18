# Sotto: design

Status: accepted, with the builder-check answers folded in. Date: 2026-11-06.

Decisions that are expensive to reverse are in `docs/adr/`; smaller calls and lessons learned are in `docs/decisions/LOG.md`. Where this document and an ADR disagree, the ADR wins.

## Intent

Sotto is a macOS meeting copilot. It listens to both sides of a call, transcribes them live, notices when the other person asks a question and starts answering before they finish, and shows the answer in a small overlay that stays out of screen shares. It also answers on demand, with an optional screenshot of the screen as context.

I am building it to learn how this class of app works end to end: native audio capture, on-device streaming speech recognition, low-latency LLM orchestration, a hardened desktop shell and a small backend. Sotto is a Cluely-style meeting copilot written from scratch; it copies no code, assets, names or visual design.

The feature this project lives or dies by is automatic question detection with a near-instant answer: answering only when the user presses a shortcut, from transcription that is segment-based in the cloud, cannot react while someone is still speaking.

### Success signals

- Automatic answers start showing within 1,000 ms (p50) and 2,000 ms (p95), measured from the end of the last "them" speech segment of the question to the first rendered answer token, for speculations started before the turn ended (ADR 0006). Sotto measures this itself and shows it in a debug panel.
- The question detector reaches precision of at least 0.9 and recall of at least 0.8 on 60 labelled snippets with revision sequences (pt-BR and English). Provisional targets, recalibrated with real meetings.
- No transcript segment is ever dropped silently. Overflow is counted and visible.
- Every item in the security requirements below has a check (test, lint rule or manual checklist entry) and passes.

## Goals

- Two-sided live transcription (me / them), on device by default, with cloud streaming as fallback.
- Automatic question detection on partial transcripts, speculative answer generation and cancellation.
- Manual ask (shortcut or typed), with optional screenshot.
- AI modes: a named system prompt plus attached files used as context (full text when small, local RAG when large).
- BYOK for Anthropic, OpenAI and Ollama. Keys never leave the device.
- A hosted mode where the backend mints short-lived provider tokens, so users without keys can use cloud transcription (and, later, paid AI).
- Overlay that can be resized, dragged, docked at the notch and hidden from screen capture.
- Session history on the device, with retention controls.
- Backend with accounts, device login, ephemeral tokens and preference sync.
- Google Calendar meeting list and "meeting starting" notification.

## Non-goals (v1)

- Billing and plans. The data model and backend leave room for them; nothing is built.
- Windows. Kept possible (Tauri and Rust are cross-platform; the Swift sidecar is the macOS-only piece), not built or tested.
- History sync across devices. The local schema is sync-ready (stable IDs, timestamps, tombstones); end-to-end encrypted sync is a later phase.
- Migration from any other app.
- Auto-update and distribution (notarization, signing for others). v1 runs locally from a dev-signed build.
- Mobile.

## Anti-goals

- Storing meeting content (audio, transcripts, screenshots, answers) on the backend.
- Any of the defects the security requirements rule out.
- Answering so eagerly that it burns the user's API budget: speculative generation must be capped and visible.
- A UI that looks like Cluely.

## Constraints

- Machine: Apple M4 Pro, 24 GB, macOS 27, Swift 6.4, Xcode 27.1 installed (active developer dir is Command Line Tools, SDK 27.0, which already includes `SpeechTranscriber`), Docker, Postgres 18, pnpm, bun. Rust is not installed yet.
- Everything must be testable for free on this machine. Paid APIs only through the user's own keys.
- Docs, code, comments, commits and UI copy are in English.
- Time budget: one or two 5-hour sessions for slices 1 and 2.

## Approaches considered

App shell:

- **Tauri 2 + Rust core + React webview + Swift sidecar (chosen).** Small footprint, a capability-based IPC that is deny-by-default, Rust for the latency-critical core, React for the rich chat UI, Swift only where macOS APIs require it. The user wants to learn Rust.
- Electron + Swift helper. Easiest UI, but the whole class of Electron hardening work (fuses, preload, CSP, Chromium cookie storage) comes with it and memory use is high.
- Pure Swift/SwiftUI. Best native integration, but macOS only and the chat rendering (markdown, code, math) is expensive to build.
- Rust with GPUI (Discord's choice). Pre-1.0, sparse docs, everything in the UI built from scratch. Rejected for this project.

Backend:

- **Rust (Axum + Postgres) in a container (chosen).** One language with the client, shared contract crate, portable to AWS, Fly or any VPS, free locally.
- Cloudflare Workers. Durable Objects per chat fit well, but the Agents SDK is TypeScript-only and `workers-rs` locks the backend to Cloudflare.
- AWS serverless. Many moving parts and weak local emulation for this scale.

Transcription:

- **On-device `SpeechTranscriber` by default, cloud streaming as fallback (chosen).** pt-BR is supported and installed on this machine (verified, see `docs/research/speech-transcriber.md`). Audio stays local.
- Cloud first (Deepgram or OpenAI Realtime). Lower and more predictable latency, but third-party audio leaves the machine.

Build order:

- **Vertical slice first (chosen).** The riskiest part (latency and detection) is proven before accounts and calendar.
- Foundation first. Cleaner start, risk discovered last.
- Parallel layers. Only pays off with several people.

## Decisions made by the user

- Tauri 2 app, Rust core, Swift sidecar, React UI. Windows later, maybe.
- Rust backend with Axum and Postgres, in a container.
- Backend built now, not deferred.
- BYOK and ephemeral tokens from day one; billing later.
- Local transcription by default, cloud streaming fallback; an onboarding test picks the engine and stores the choice in settings.
- History on the device in v1, sync-ready data model, E2E sync later.
- Automatic question detection with speculative "start thinking before the question ends" behavior is a must-have.
- Modes with files need retrieval.
- Resizable UI, notch docking, BYOK.
- None of the common defects of this class of app (see `docs/security-checklist.md`).
- Name: Sotto (for now). Design system built around the name.
- Docs and app in English.
- Use cheaper models in parallel where it saves cost without hurting quality.

## Architecture

```
┌──────────────────────── Sotto.app (macOS) ────────────────────────┐
│  sotto-audio (Swift sidecar)          sotto-core (Rust, Tauri)     │
│  ├ mic: AVAudioEngine + voice proc.   ├ session manager            │
│  ├ system audio: process tap          ├ transcript store (SQLite)  │
│  ├ STT: 2x SpeechAnalyzer             ├ question detector          │
│  ├ screenshot: ScreenCaptureKit       ├ answer orchestrator        │
│  └ JSON lines / PCM frames ──stdout──►├ providers: Anthropic,      │
│                         ◄──stdin──────┤   OpenAI, Ollama, cloud STT│
│                                       ├ keychain, settings, RAG    │
│                                       └ overlay windows (objc2)    │
│                                              │ Tauri commands/events│
│                                       React webview (overlay, chat,│
│                                       settings, onboarding)        │
└───────────────────────────────────────────────┬────────────────────┘
                                                │ HTTPS (no meeting content)
                                   sotto-api (Axum) ── Postgres
                                   accounts, device login, ephemeral
                                   tokens, preference sync, (billing later)
```

### Repository layout

```
sotto/
  apps/desktop/          Tauri app: src-tauri (Rust core), src (React)
  native/sotto-audio/    SwiftPM package for the sidecar
  services/api/          Axum backend
  crates/sotto-protocol/ shared types (API contract, sidecar messages); ts-rs exports TS types
  crates/sotto-detect/   question detector and turn state machine (pure, heavily tested)
  docs/                  design.md, ARCHITECTURE.md, adr/, research/, DESIGN_SYSTEM.md
  fixtures/              audio clips, labelled transcripts
  justfile, docker-compose.yml, CLAUDE.md
```

### Swift sidecar (`sotto-audio`)

- Process model and permissions in ADR 0007 (a spike confirms TCC attribution first; fallback is a linked static library).
- Spawned by the core with an absolute path inside the bundle. Never resolved through `PATH`.
- Both streams are stamped on one host-time clock so "me" and "them" ranges are comparable.
- Protocol: newline-delimited JSON on stdout for events and on stdin for commands. Raw PCM for the cloud fallback goes over a separate length-prefixed binary stream on fd 3, so it never mixes with JSON. Message types live in `sotto-protocol` and the Swift side mirrors them (a schema test keeps them in sync).
- Mic: `AVAudioEngine` with voice processing enabled, which gives echo cancellation. Both sides stay audible when people talk over each other.
- System audio: Core Audio process tap with an aggregate device, excluding Sotto's own process.
- STT: one `SpeechAnalyzer` per stream with `SpeechTranscriber`, progressive preset with volatile and fast results, `prepareToAnalyze` at session start, input converted to the analyzer's preferred format. Volatile results can be revised or revoked; the protocol carries a stable segment ID, a revision number and the time range.
- Screenshot: `ScreenCaptureKit` capture of the display that contains the overlay, with Sotto's own windows excluded, downscaled to a 1920 px long side, PNG.
- Engine modes: `local` (emits transcript events) and `raw` (emits PCM for the cloud fallback).

### Rust core (`sotto-core`)

- Owns all state. The webview receives snapshots and sends intents. Every intent is a typed command validated by serde with `deny_unknown_fields` before it touches state.
- Question detector and answer orchestrator: specified in ADR 0006. In short, a lexical heuristic on "them" revisions (no reliance on punctuation, which `SpeechTranscriber` does not produce) starts a hidden speculative generation at once; a classifier runs in parallel and can cancel it; at end of turn the final question is compared with the speculated one and the answer is revealed or restarted. Guards: one live speculation, cooldown, per-minute cap, per-session spend cap, off switch.
- Providers behind one trait: streaming text, cancellation, image input, prompt caching where supported. Defaults (user-editable): automatic answers and classifier on `claude-haiku-5-5` at low effort, manual asks on `claude-sonnet-5-5`, Ollama as the free option. Prices live in a dated config file.
- Keys: macOS Keychain via `keyring`. Never written to disk, never logged, never sent to the backend.
- Storage (ADR 0004): SQLite through `rusqlite` with SQLCipher; the database key lives in the Keychain. Sessions, transcript segments, messages, modes, files and embeddings. Retention deletes sessions older than N days (default 30).
- RAG: files are parsed to text (txt, md, pdf). If a mode's total text is under 30k tokens it goes into the cached prompt prefix. Above that, chunks are embedded (Ollama multilingual embedding model, or the provider's embedding API), stored as BLOBs in the encrypted database and searched by brute-force cosine similarity in Rust.
- Overlay windows: created by Tauri, converted to non-activating `NSPanel` through `objc2`. Always on top, visible on all Spaces, `sharingType = .none` when invisible mode is on, hidden from Mission Control. Notch docking uses `NSScreen.safeAreaInsets` and the auxiliary top areas; on screens without a notch it docks to the top center. Size and position persist per display.
- Global shortcuts via the official plugin; defaults are documented in settings.
- Login item: off by default, offered in onboarding.

### Webview (React + TypeScript)

- Vite, strict TypeScript, types generated from `sotto-protocol` with `ts-rs`. Chat rendering with markdown, code highlighting and math.
- No remote content: everything is bundled. CSP allows only `'self'` and the Tauri IPC origin. The webview never talks to providers or the backend; the core does.
- Windows: overlay (transcript strip, answer card, input), settings, onboarding, history.

### Onboarding

1. Recording consent: an explanation that other participants are recorded and that consent rules vary by jurisdiction, plus a per-session "listening" indicator.
2. Permissions, each with its reason: microphone, system audio capture, speech recognition (shipped key in case the framework asks), screen recording (only if the user enables screenshots), accessibility (only if a chosen shortcut needs it).
3. Engine benchmark: plays bundled pt-BR and English clips through the local engine (and the cloud engine if available), measures time to first partial, end-of-speech to final, and word error rate against reference text, then stores the chosen engine and results in settings. It can be re-run from settings and is offered again after a macOS update.
4. Models: BYOK keys or hosted mode sign-in, and a test call.

### Backend (`sotto-api`)

- Axum, `sqlx` (>= 0.8.1) with Postgres, migrations in the repo, Docker Compose with Postgres and Mailpit. Container runs as non-root.
- Accounts: email and password (argon2id) and magic link (sent to Mailpit locally).
- Device login, instead of a bare token in a deep link: the app generates `state` and a PKCE verifier and opens the browser to the backend's login page. After login the backend redirects to `sotto://auth/callback?code=…&state=…`. The app accepts the callback only if `state` matches a pending request created by this process in the last 5 minutes, then exchanges the code with the verifier. Codes are single use and expire in 60 seconds.
- Tokens: opaque access tokens (15 min) and rotating refresh tokens stored hashed. The refresh token lives in the Keychain.
- Ephemeral tokens (hosted mode): `POST /v1/realtime-tokens` mints a short-lived token from a server-held provider key (OpenAI Realtime transcription client secrets; Deepgram temporary keys if available, unverified). Rate limited per account, with audit log entries that hold no content. Providers without ephemeral tokens (Anthropic) are not offered in hosted mode in v1; a stateless proxy is a later decision.
- Preference sync: settings, shortcuts, mode names and prompts (not files). No meeting content endpoints exist.
- Strict CORS (no browser clients besides the login pages), structured logs without request bodies, secrets from environment variables, `cargo deny` and `cargo audit` in checks.

### Calendar (my call)

Google Calendar runs in the app, not the backend: OAuth for installed apps with PKCE and a loopback redirect, refresh token in the Keychain. The backend never holds calendar data, which keeps third-party data off the server. The "meeting starting" notification comes from the local event list, plus an optional signal when another app starts using the microphone. The user needs to create a Google Cloud OAuth client (desktop type).

## Security requirements

Each item has a check in the harness or the manual checklist (`docs/security-checklist.md`).

- Tauri `>= 2.11.1` (fixes GHSA-7gmj-67g7-phm9). Capabilities per window, deny by default; no `tauri-plugin-shell` exposed to JavaScript; no remote domain IPC access.
- Strict CSP, no remote scripts, no CDN fallbacks. Model and asset downloads are pinned by SHA-256.
- Webview intents validated before they change state.
- Login callback protected with state and PKCE.
- Secrets only in the Keychain; local database encrypted with a Keychain-held key.
- Sidecar launched by absolute path; no `PATH` changes.
- Bounded queues with explicit overflow counters; no silent drops.
- Native echo cancellation instead of muting the mic.
- Logs never contain transcript text, prompts, answers, keys or tokens.
- Minimal entitlements: audio input; no camera. Hardened runtime on dev builds.
- Login item off by default.
- Backend stores no meeting content; argon2id, hashed tokens, rate limits, non-root container, dependency audits.

## Engineering harness

- `docs/ARCHITECTURE.md` (components, data flow, trust boundaries), five ADRs (shell, backend, transcription, history, keys), `docs/DESIGN_SYSTEM.md`, project `CLAUDE.md` with the rules below.
- `just check` runs: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`, `cargo deny check`, Swift build with warnings as errors and `swift test`, `tsc --noEmit` (strict), Biome lint and format, a check that generated TS types are current, and the protocol schema test. A pre-commit hook runs the fast subset; nothing is committed red.
- Tests where bugs are expensive: detector and turn state machine against labelled fixtures, speculation cancel logic with a fake clock and fake provider, protocol round-trips, backend auth, device login and token minting. UI and live audio are tested by hand.
- Review: at the end of each slice an independent reviewer reads the diff against these docs before the next slice starts.
- Model use: Opus for architecture, contract, cross-component integration and final review; Sonnet for well-specified components (sidecar pieces, backend endpoints, UI screens) and research; Haiku only for mechanical work (fixtures, boilerplate, lint fixes, reference docs), never for audio, Keychain or auth code. Each delegated task gets a written brief with file boundaries so agents do not overlap.

## Slices

Slice 1, the vertical slice:

0. Spikes, throwaway, before the protocol work: the permissions spike (ADR 0007) and a latency spike that feeds a bundled pt-BR clip through `SpeechTranscriber` and measures partial and final timing, plus Haiku 5.5 time to first token. Results go into the ADRs and `LOG.md`.
1. Install Rust, scaffold the monorepo, harness and docs.
2. Sidecar: mic with voice processing, process tap, two analyzers, JSON protocol, screenshot.
3. Core: session manager, detector, orchestrator with speculation and caps, Anthropic, OpenAI and Ollama providers, Keychain, SQLite.
4. Overlay: panel, invisible mode, transcript strip, streaming answer card, manual ask with screenshot, debug panel with latency numbers.

Slice 2, the rest:

1. Backend with accounts, device login, ephemeral tokens, preference sync; cloud STT fallback in the core.
2. Onboarding with consent, permissions and benchmark.
3. Modes with files and RAG.
4. Calendar and meeting notification.
5. Notch docking, resizing polish, design system applied across screens.

If time runs short, cut in this order: RAG (full text only), notch docking (top center only), calendar.

## Accepted risks

- Live pt-BR latency of `SpeechTranscriber` is unknown until the slice 1 spike. If it is slow, the cloud engine (opt-in) is the fallback and early detection is weaker in local mode.
- `sharingType = .none` may not hide windows from every capture path on recent macOS versions (unverified). The manual check covers screenshot, QuickTime, Zoom and Google Meet.
- TCC attribution for the sidecar is unverified; the spike decides between the process and the linked library (ADR 0007).
- Cloud transcription sends third parties' audio to a provider; it is opt-in only (ADR 0003).
- Hosted mode cannot offer Anthropic without a proxy, so hosted users get OpenAI or Deepgram only in v1.
- Recording consent is the user's responsibility; Sotto informs and shows an indicator.

## Left to the builder

Exact crate versions within the constraints above, internal module structure, heuristic thresholds (tuned against fixtures), prompt wording, the stability window length, chunk size and top-k for RAG, and the UI details defined later in `DESIGN_SYSTEM.md`.
