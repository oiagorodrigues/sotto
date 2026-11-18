# 0003: On-device SpeechTranscriber by default, cloud streaming only with opt-in

Status: accepted (2026-11-06)

## Context

Automatic question detection needs partial text while the other person is still speaking. Transcribing closed segments in the cloud (a VAD closes a segment after a short silence, then a WAV is uploaded) cannot react mid-sentence. `SpeechTranscriber` (macOS 26+) streams volatile and final results on device; `pt_BR` and English are supported and installed on the development machine (verified, see `docs/research/speech-transcriber.md`).

## Decision

- Default engine: one `SpeechAnalyzer` + `SpeechTranscriber` per stream (me, them), progressive preset with volatile and fast results, in the Swift sidecar.
- Fallback engine: cloud streaming STT (OpenAI Realtime transcription, or Deepgram if its temporary keys are confirmed), reached with the user's key (BYOK) or an ephemeral token (hosted mode).
- The cloud engine is used only after explicit opt-in in settings, never as a silent fallback, because it sends third parties' audio off the device. Each session shows which engine is active.
- Onboarding benchmarks the engines on bundled clips (time to first partial, end of speech to final, WER) and stores the result and choice in settings. If the local engine does not support the session language and cloud is not opted in, the session asks before starting.
- Bounded queues with overflow counters. No segment is dropped without being counted and shown.

## Consequences

- Audio stays on the device by default.
- SpeechTranscriber has no punctuation option (only DictationTranscriber does), so the question detector cannot rely on question marks (see ADR 0006).
- Real pt-BR live latency is unmeasured until the slice 1 latency spike.

## Alternatives

- Whisper with a sliding window: multilingual but pseudo-streaming and GPU-heavy; `whisper-rs` is archived on GitHub (July 2025).
- Kyutai STT: true streaming, but English and French only.
- Cloud first: lower and more predictable latency, but third-party audio leaves the machine by default.
