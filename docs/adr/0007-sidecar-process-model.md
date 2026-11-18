# 0007: Sidecar process, gated by a permissions spike

Status: accepted, pending spike (2026-11-06)

## Context

macOS privacy permissions (TCC) for microphone, system audio capture, speech recognition and screen recording are attributed to a "responsible process". Whether a command-line Swift sidecar spawned by the Tauri app gets its prompts and grants attributed to Sotto.app is unverified, and so are the Info.plist keys a bare executable needs. A common pattern is to ship native helpers as separate executables inside the bundle, signed with the app's team.

## Decision

- Default: `sotto-audio` is a separate executable at `Sotto.app/Contents/MacOS/sotto-audio`, signed with the same identity, with an Info.plist embedded through `-sectcreate __TEXT __info_plist`, launched by absolute path with `std::process` and never through `PATH`.
- Before the protocol work, a spike (at most 1 hour) checks: the prompt shows Sotto as the requester; grants survive rebuilds with a stable signing identity; voice processing on the mic and a process tap excluding Sotto work at the same time; whether `SpeechAnalyzer` triggers a speech recognition prompt. Results go into this ADR.
- If attribution fails: build the Swift code as a static library with a C ABI (`@_cdecl`) linked into the Rust core, with the same message types over a callback.

## Consequences

- The JSON contract (stdout events, stdin commands, fd 3 for raw PCM) only exists in the process model; the fallback keeps the types and drops the transport.
- Info.plist keys shipped: `NSMicrophoneUsageDescription`, `NSAudioCaptureUsageDescription`, `NSSpeechRecognitionUsageDescription`, and screen recording handled by ScreenCaptureKit's own prompt. Entitlements: `com.apple.security.device.audio-input` only; no camera.

## Alternatives

- Linked library from the start: one process, but Swift and Rust build integration costs time before we know it is needed.
