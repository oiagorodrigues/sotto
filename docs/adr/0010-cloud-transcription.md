# 0010: Cloud transcription through OpenAI Realtime with client-side turns

Status: accepted (2026-11-06)

## Context

ADR 0003 makes the on-device engine the default and allows a cloud engine only after an explicit opt-in. This ADR picks the provider, the protocol and where the work happens. The question detector (ADR 0006) needs text while the other person is still talking, so a provider that returns text only after a finished utterance does not work.

## Decision

- Provider: OpenAI Realtime transcription over WebSocket, `wss://api.openai.com/v1/realtime?intent=transcription`, `Authorization: Bearer <key or ephemeral token>`, model `gpt-live-transcribe`, PCM16 mono at 24 kHz. ADR 0005 already provides ephemeral tokens for hosted mode; the same URL and header apply.
- The sidecar runs in its raw mode and writes 16 kHz PCM frames to fd 3. The Rust core resamples to 24 kHz, cuts turns with an energy gate, and sends one connection per speaker. The core, not the sidecar and not the provider, owns turn boundaries (`gpt-live-transcribe` has no server VAD and finalizes on `input_audio_buffer.commit`).
- Only audio inside a turn is sent, so silence is not billed (about $0.017 per minute of audio per connection).
- Provider events become the same transcript revisions as the local engine (one segment per turn), so detection, storage and the UI are shared.
- Credentials come from a `CloudCredentials` trait: the user's key now, a backend-minted short-lived token in hosted mode.
- A dropped socket is retried once; after that the engine is reported unavailable. There is no automatic switch to the local engine, or from it.

## Consequences

- Third parties' audio leaves the device only after the user picks Cloud and confirms in a dialog; the notch shows the engine for the whole session.
- The gate thresholds decide how much speech is cut or split, and they are tuned without real call audio so far.
- The protocol is tied to OpenAI's current live transcription API. The wire code is isolated in `cloud_stt/wire.rs`, the connection in `cloud_stt/engine.rs`; another provider (Deepgram) would add a sibling engine behind the same `CloudEvent` stream.

## Alternatives

- `gpt-4o-mini-transcribe` or `gpt-4o-transcribe` with server VAD: cheaper per minute for the mini model ($0.003), but text arrives only after each turn, which is too late for speculative answers.
- Sending the continuous stream with the sidecar's own cut points: the Swift side has no VAD today, and silence would be billed.
- Keeping the audio path in Swift: splits the provider logic across two languages and keys would have to reach the sidecar.
