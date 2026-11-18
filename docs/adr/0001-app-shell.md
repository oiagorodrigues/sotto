# 0001: Tauri 2 with a Rust core, React webview and a Swift sidecar

Status: accepted (2026-11-06)

## Context

Sotto needs a rich chat UI (markdown, code, math), an always-on-top overlay hidden from screen capture, native audio capture with echo cancellation, Core Audio process taps, on-device speech recognition and a low-latency orchestration core. Apps in this category usually ship on Electron, where most of the hardening work is Electron-specific: Chromium cookie encryption, the preload bridge, CSP, and IPC exposed to any page of the renderer origin. The user wants to learn Rust and does not need Windows in v1.

## Decision

- Tauri 2 (`>= 2.11.1`, which fixes GHSA-7gmj-67g7-phm9) with the system WebView.
- All state and all network calls live in the Rust core. The webview renders snapshots and sends typed intents.
- Swift sidecar (`sotto-audio`) for the APIs that only Swift reaches well: AVAudioEngine voice processing, Core Audio process taps, SpeechAnalyzer and ScreenCaptureKit.
- Overlay windows become non-activating `NSPanel`s through `objc2`, written in-house rather than through `tauri-nspanel` (single maintainer, no releases).
- Types shared with TypeScript through `ts-rs` (stable v12), not `specta` (still a release candidate).

## Consequences

- Capabilities are deny-by-default per window; the webview cannot reach the network or the filesystem.
- Two native languages (Rust, Swift) plus TypeScript. The Swift part stays small and has a JSON contract with tests.
- WKWebView instead of Chromium: no WebGPU or ONNX-in-the-browser assumptions; all ML runs natively.
- Windows support later means replacing the Swift sidecar with a Windows equivalent; the rest carries over.

## Alternatives

- Electron + Swift helper: the usual choice, but it brings that whole class of hardening work and higher memory use.
- Pure Swift/SwiftUI: best native fit, macOS-only and expensive chat rendering.
- Rust + GPUI (Discord's 2026 rewrite): pre-1.0, sparse docs, UI from scratch.
- `tauri-plugin-shell` for the sidecar: had a critical advisory (GHSA-c9pr-q8gx-3mgp, fixed in 2.2.1) and exposes process spawning to JS. The core spawns the sidecar with `std::process` instead.
