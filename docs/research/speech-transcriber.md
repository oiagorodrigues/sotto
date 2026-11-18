# SpeechAnalyzer and SpeechTranscriber on macOS 26/27: practical reference

Scope: real-time on-device transcription for the Swift sidecar of the meeting copilot (microphone plus system audio, pt-BR and English, partial and final results as JSON lines).

Evidence tags used throughout:

- **[SDK]** read from `Speech.swiftinterface` / `.swiftdoc` in the SDK 27.0 on this machine (Swift 6.4). This is the ground truth.
- **[Measured here]** output of a script that only queries metadata (locales, asset status, formats). No audio was recorded.
- **[Apple]** WWDC25 session 277.
- **[3P]** third-party source, with date and caveats.
- **unverified** no source above confirmed it. Each item says how to verify.

## 1. Decisions in brief

1. pt-BR is supported by both `SpeechTranscriber` and `DictationTranscriber` [SDK + measured here]: `pt_BR` is in both `supportedLocales` lists and was already installed on this machine.
2. For fast partials use `SpeechTranscriber` with `reportingOptions: [.volatileResults, .fastResults]`, which is what the `.progressiveTranscription` preset does [SDK].
3. Use one `SpeechAnalyzer` and one `SpeechTranscriber` per stream (mic, system). Modules cannot be reused across analyzers [SDK].
4. The preferred input format here is 16 kHz, mono, Int16 [measured here]. Convert the 48 kHz float audio with `AVAudioConverter`, or on macOS 27 with `AnalyzerInputConverter` [SDK].
5. Permissions: microphone is required. Whether `NSSpeechRecognitionUsageDescription` / `SFSpeechRecognizer.requestAuthorization` is required for the new API is unverified (section 4).

## 2. Mental model

Pieces [SDK]:

- `SpeechAnalyzer` is an `actor`. It takes a list of modules (`[any SpeechModule]`) and an async sequence of `AnalyzerInput`.
- Modules: `SpeechTranscriber`, `DictationTranscriber`, `SpeechDetector` (VAD). Each module has its own `results` async sequence. You iterate `transcriber.results` in a separate `Task`, not results from the analyzer.
- `AnalyzerInput` wraps an `AVAudioPCMBuffer` with an optional `bufferStartTime: CMTime?`. On macOS 27 it also accepts a `CMReadySampleBuffer` and exposes `bufferDuration` and `bufferFormat`; the `buffer` property is deprecated in 27.
- `SpeechTranscriber.Result` has `text: AttributedString`, `alternatives: [AttributedString]`, `range: CMTimeRange`, `resultsFinalizationTime: CMTime`, and `isFinal` (from an extension on `SpeechModuleResult`).

Volatile versus final [SDK]:

- `isFinal` is `resultsFinalizationTime >= range.end`. If false the result is volatile: a later result covering the same range may replace it, and there is no guarantee it is ever reissued as final.
- An empty text in a volatile result means earlier results for that range are revoked.
- `SpeechAnalyzer.volatileRange` combines the volatile ranges of all modules. Results outside it will not be replaced. `volatileRangeChangedHandler` (init argument or `setVolatileRangeChangedHandler`) fires when the start (`changedStart`, something was finalized) or the end (`changedEnd`, new analysis started) moves.
- A module need not re-emit as final a range that did not change on finalization. The docs suggest two transcribers (one volatile+final, one final-only) if you need that. For us, treat "range finalized" as the truth (`isFinal` or `resultsFinalizationTime`).
- The WWDC sample appends to `finalizedTranscript` when `isFinal` and overwrites `volatileTranscript` otherwise [Apple].

Time: `range` is a `CMTimeRange` on the input audio timeline (Apple says timecodes are sample-accurate [Apple]). With `attributeOptions: [.audioTimeRange]` each run of the `AttributedString` carries an `audioTimeRange`; `rangeOfAudioTimeRangeAttributes(intersecting:)` finds the text for an interval [SDK].

## 3. Setup: locale and assets

API [SDK]:

- `SpeechTranscriber.supportedLocales` (async), `.installedLocales` (async), `.supportedLocale(equivalentTo:)` (async; use it instead of `contains`, since equivalent locales may not be equal), `.isAvailable` (sync Bool).
- `AssetInventory.status(forModules:)` returns `Status`: `unsupported < downloading < supported < installed`. With mixed module states it returns the lowest.
- `AssetInventory.assetInstallationRequest(supporting:) async throws -> AssetInstallationRequest?` (nil when nothing needs installing). `AssetInstallationRequest` is `ProgressReporting` (`progress`) and has `downloadAndInstall()`.
- Reservations: `AssetInventory.reserve(locale:)`, `release(reservedLocale:)`, `reservedLocales`, `maximumReservedLocales`. The class reserves automatically when needed. Here `maximumReservedLocales` = 5 and `reservedLocales` was empty [measured here].
- The WWDC25 session mentions `AssetInventory.deallocate(locale:)` [Apple]. The SDK 27 name is `release(reservedLocale:)`. Follow the SDK.

Measured here (macOS 27, M4 Pro):

```
SpeechTranscriber.supportedLocales: 45 (includes pt_BR, pt_PT, en_US, en_GB, en_AU, en_CA, en_IN, en_IE, en_NZ, en_SG, en_ZA)
SpeechTranscriber.installedLocales: pt_BR, pt_PT, en_US, en_GB and 7 more en_* (11 total)
DictationTranscriber.supportedLocales: 54 (includes pt_BR and the same en_*)
DictationTranscriber.installedLocales: en_GB, pt_BR, en_US
SpeechTranscriber.isAvailable: true
status(forModules: [SpeechTranscriber(pt-BR, .progressiveTranscription)]) = .supported
```

`status` returned `.supported`, not `.installed`, even though `pt_BR` is in `installedLocales`. I did not investigate (possibly the per-app reservation is still missing). Do not gate on `.installed`; always call `assetInstallationRequest`, which returns nil when nothing is missing.

Setup code:

```swift
import Speech
import AVFoundation

enum SetupError: Error { case unsupportedLocale(String), noFormat }

struct LocalEngine: Sendable {
    let transcriber: SpeechTranscriber
    let format: AVAudioFormat
}

func makeTranscriber(localeID: String) async throws -> LocalEngine {
    guard SpeechTranscriber.isAvailable else { throw SetupError.unsupportedLocale("unavailable") }
    guard let locale = await SpeechTranscriber.supportedLocale(equivalentTo: Locale(identifier: localeID)) else {
        throw SetupError.unsupportedLocale(localeID)
    }
    let transcriber = SpeechTranscriber(locale: locale, preset: .timeIndexedProgressiveTranscription)

    if let request = try await AssetInventory.assetInstallationRequest(supporting: [transcriber]) {
        // Report request.progress (a Foundation Progress) to the Tauri UI via KVO or polling.
        try await request.downloadAndInstall()
    }
    guard let format = await SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith: [transcriber]) else {
        throw SetupError.noFormat
    }
    return LocalEngine(transcriber: transcriber, format: format)
}
```

Offline [SDK]: assets are models downloaded from Apple's servers and managed by the system, shared across apps. If a download fails on connectivity, the system retries later; `downloadAndInstall()` returns after the first attempt, and you track it with `status(forModules:)` or another request. Once installed, transcription needs no network. With no asset, `availableCompatibleAudioFormats` is empty and `SFSpeechError.Code.noModel` means no model is available or downloadable for the locale/options. The exact failure when offline with no asset is unverified (test on a clean machine with no network); the cloud fallback covers it.

## 4. Permissions

Confirmed:

- `SFSpeechRecognizer.requestAuthorization` requires `NSSpeechRecognitionUsageDescription` in Info.plist and the app crashes without it [SDK, `SFSpeechRecognizer.h`]. That is the legacy API.
- Microphone: the WWDC25 sample requests microphone permission before recording [Apple]. The mic path needs `NSMicrophoneUsageDescription` (standard macOS, not part of Speech).
- System-audio capture through a Core Audio process tap has its own consent. The exact Info.plist key for that is unverified.

Unconfirmed (unverified):

- Whether `SpeechAnalyzer` / `SpeechTranscriber` require `SFSpeechRecognizer.requestAuthorization`. Third-party guides disagree: the `apple_speech` plugin README says analyzer-based file transcription needs neither speech nor microphone permission, while two community guides advise requesting both. None is Apple documentation. To verify: in a test binary with `NSMicrophoneUsageDescription` and without `NSSpeechRecognitionUsageDescription`, transcribe an audio file (records nothing), then the mic, and watch for a prompt, a crash or an error. For the product, the safe rule is to ship both keys and call `requestAuthorization` once during onboarding.
- TCC attribution for a CLI sidecar spawned by a parent app (responsible process) is unverified. The general macOS behavior is that permissions are attributed to the responsible process, so a child spawned directly by the Tauri app normally inherits its context, but I have no cited source for it. To verify: launch the sidecar from the packaged, signed app and check which name appears in System Settings > Privacy; also run it from Terminal, where Terminal becomes the responsible process.
- Info.plist for a CLI: bare executables have no bundle. The usual technique is embedding a plist in the `__TEXT,__info_plist` section with the linker flag `-Xlinker -sectcreate -Xlinker __TEXT -Xlinker __info_plist -Xlinker Info.plist`. Unverified for this project. A safer alternative is shipping the sidecar inside the Tauri `.app` bundle (`Contents/MacOS` or `Contents/Helpers`), declaring the keys in the app's Info.plist and signing everything with the same Team ID and hardened runtime, including the `com.apple.security.device.audio-input` entitlement.

## 5. Audio input

Formats [SDK + measured here]: each module reports accepted formats in `availableCompatibleAudioFormats`. For `SpeechTranscriber` pt-BR: 8000 Hz mono Int16 and 16000 Hz mono Int16. `SpeechAnalyzer.bestAvailableAudioFormat(compatibleWith:)` returned 16 kHz mono Int16. There is also `bestAvailableAudioFormat(compatibleWith:considering:)`, which takes the source's natural format. A wrong format raises `SFSpeechError.Code.unexpectedAudioFormat` or `incompatibleAudioFormats`.

macOS 27 conversion [SDK]: `AnalyzerInputConverter` (macOS 27+) converts an `AVAudioBuffer` and returns `[AnalyzerInput]` (zero or more per call), with `flush()` at the end. It has `init(analyzerFormat:configurationHandler:)` to tweak the inner `AVAudioConverter`, and an async `converter(compatibleWith:)`.

Alternative that also works on macOS 26, with `AVAudioConverter`:

```swift
final class BufferConverter: @unchecked Sendable {   // used from one serial queue per stream
    private let converter: AVAudioConverter
    private let outFormat: AVAudioFormat

    init?(from input: AVAudioFormat, to output: AVAudioFormat) {
        guard let c = AVAudioConverter(from: input, to: output) else { return nil }
        converter = c; outFormat = output
    }

    func convert(_ buffer: AVAudioPCMBuffer) throws -> AVAudioPCMBuffer? {
        let ratio = outFormat.sampleRate / buffer.format.sampleRate
        let cap = AVAudioFrameCount((Double(buffer.frameLength) * ratio).rounded(.up)) + 16
        guard let out = AVAudioPCMBuffer(pcmFormat: outFormat, frameCapacity: cap) else { return nil }
        var consumed = false
        var error: NSError?
        let status = converter.convert(to: out, error: &error) { _, inputStatus in
            if consumed { inputStatus.pointee = .noDataNow; return nil }
            consumed = true
            inputStatus.pointee = .haveData
            return buffer
        }
        if status == .error { throw error ?? SetupError.noFormat }
        return out.frameLength > 0 ? out : nil
    }
}
```

Keep one converter per stream and reuse it across buffers, because the resampler keeps state and a new converter per buffer introduces discontinuities (engineering judgment, not from a source). If the tap delivers stereo, the converter downmixes to mono.

Buffer size: the WWDC sample uses `installTap(bufferSize: 4096)` [Apple]. For latency, 1024 to 2048 frames at 48 kHz (21 to 43 ms) is a reasonable range; the best size is undocumented, so unverified, measure it. Buffers must not overlap or precede earlier input; violations give `SFSpeechError.Code.audioDisordered` [SDK]. To skip audio, omit buffers and pass the correct `bufferStartTime` when resuming [SDK].

Voice processing on the mic: the input sequence does not care about the source, but enabling voice processing on the `AVAudioEngine` input node changes the node's format. That is AVAudioEngine behavior, not Speech, and is unverified here. Read the real tap format instead of assuming 48 kHz mono.

Two analyzers at once: the docs say there is a limit on active engine instances and models, and that on macOS there is "currently no limit" [SDK]. On iOS and visionOS the limit is about two. Exceeding it throws `insufficientResources`. On macOS 27, `SpeechAnalyzer.Options(priority:modelRetention:ignoresResourceLimits:)` lifts the conservative limit, at the cost of unpredictable failures under load [SDK]. Similarly configured transcribers share engine and model [SDK], so use the same locale and preset on both streams.

Threading: the analyzer is an actor and `results` sequences are Sendable. Have the audio callback only hand the buffer to a serial queue per stream, where conversion and `continuation.yield` happen. Do not convert inside the real-time audio callback.

## 6. Latency

Options [SDK]:

| Option | Effect |
|---|---|
| `.volatileResults` | Emits partial hypotheses that improve until finalized. Without it only finals arrive. |
| `.fastResults` (`SpeechTranscriber` only) | "Biases the transcriber towards responsiveness, yielding faster but also less accurate results", using a smaller context window. |
| `.frequentFinalization` (`DictationTranscriber` only) | Finalizes more often. |
| `SpeechAnalyzer.prepareToAnalyze(in:)` | Does the lazy setup before the first audio to "reduce or eliminate delays". Call it after the modules are set. |
| `Options.modelRetention` | `.whileInUse`, `.lingering`, `.processLifetime`: how long models stay loaded; `SpeechModels.endRetention()` releases a `.processLifetime` model. |
| `Options.priority` | `TaskPriority` of the analysis work. |
| `finalize(through:)` | Forces delivery up to a timecode (you agree to wait). `cancelAnalysis(before:)` drops pending work (you cannot wait). |
| `SpeechDetector` | VAD that gates silence to save power. Not a latency feature. |

`SpeechTranscriber` presets [SDK, documentation table]:

| Preset | volatile | fast | alternatives | audioTimeRange |
|---|---|---|---|---|
| `transcription` | no | no | no | no |
| `transcriptionWithAlternatives` | no | no | yes | no |
| `timeIndexedTranscriptionWithAlternatives` | no | no | yes | yes |
| `progressiveTranscription` | yes | yes | no | no |
| `timeIndexedProgressiveTranscription` | yes | yes | no | yes |

For the question detector use `timeIndexedProgressiveTranscription` (fast partials plus timestamps). Confidence needs `.transcriptionConfidence` in `attributeOptions`; build it with `SpeechTranscriber.Preset(transcriptionOptions:reportingOptions:attributeOptions:)` or the full initializer.

Latency numbers (all [3P], none measured here):

- IraVoice (dev.to; the author sells the IraVoice dictation app), one Apple Silicon Mac, clean English, 607 reference words: median post-speech latency 125 to 132 ms and p95 194 to 201 ms for SpeechAnalyzer in progressive mode; WER 1.98% versus 4.28% for whisper.cpp small.en. The author says latencies are close and does not claim a latency win. The two engines ran through different paths. No multilingual data, no pt-BR.
- MacStories (Finn Voorhees) via GIGAZINE, 2025-06-19: a 34-minute video transcribed in 45 s by SpeechAnalyzer (Yap CLI) versus 1 min 41 s for Whisper Large-v3. This is file throughput, not streaming latency.
- Time to first partial for live pt-BR mic streaming: unknown. Measure it (section 11).
- The WWDC25 session gives no numbers [Apple].

## 7. Vocabulary, punctuation, timestamps, confidence

- `AnalysisContext.contextualStrings[.general] = [...]`, applied with `analyzer.setContext(_:)` or in the init [SDK]. The `.swiftdoc` text describes this property in terms of `DictationTranscriber`, recommends short phrases (one or two words) and at most 100 phrases in total. Whether `SpeechTranscriber` honors `contextualStrings` is unverified; test by measuring WER with and without the list on your proper nouns.
- Custom language models (`SFCustomLanguageModelData` with `PhraseCount` and `CustomPronunciation` in X-SAMPA, `export(to:)`) attach through `DictationTranscriber.ContentHint.customizedLanguage(modelConfiguration:)` [SDK]. The interface shows no equivalent on `SpeechTranscriber`.
- Punctuation: `SpeechTranscriber.TranscriptionOption` has only `etiquetteReplacements`; there is no punctuation switch [SDK]. Whether and how well it punctuates pt-BR by default is unverified. `DictationTranscriber` has `.punctuation`, `.emoji`, `.etiquetteReplacements`.
- Timestamps: `.audioTimeRange` (attribute `audioTimeRange: CMTimeRange` per run) [SDK].
- Confidence: `.transcriptionConfidence` (attribute `transcriptionConfidence: Double`) [SDK].

```swift
extension AttributedString {
    var plain: String { String(characters) }
}

func words(of r: SpeechTranscriber.Result) -> [(text: String, start: Double, end: Double, conf: Double?)] {
    r.text.runs.map { run in
        let t = String(r.text[run.range].characters)
        let tr = run.audioTimeRange
        return (t, tr?.start.seconds ?? .nan, tr?.end.seconds ?? .nan, run.transcriptionConfidence)
    }
}
```

This snippet was not compiled; the attribute key paths follow `AttributeScopes.SpeechAttributes` in the interface, but compile it before relying on it.

## 8. Languages, and SpeechTranscriber versus DictationTranscriber

Confirmed [SDK + measured here]: `pt_BR` is in `supportedLocales` for both, and English covers en_US, en_GB, en_AU, en_CA, en_IN, en_IE, en_NZ, en_SG, en_ZA. The counts (45 and 54) are for this machine and OS; check at runtime with `supportedLocale(equivalentTo:)` because the list depends on device and OS (WWDC25 already said more languages were coming [Apple]).

| | SpeechTranscriber | DictationTranscriber |
|---|---|---|
| Origin [Apple] | New model, suited to long-form and distant audio such as meetings | Matches the iOS 10 on-device recognizer; covers languages and hardware the new model does not |
| Locales here | 45 | 54 |
| Fast results | `.fastResults` | `.frequentFinalization` |
| Punctuation | no option | `.punctuation` |
| Content hints | none | `.shortForm`, `.farField`, `.atypicalSpeech`, `.customizedLanguage` |
| Custom model | none seen | yes |
| Presets | 5 (above) | `phrase`, `shortDictation`, `progressiveShortDictation`, `longDictation`, `progressiveLongDictation`, `timeIndexedLongDictation` |

Recommendation: `SpeechTranscriber` by default; `DictationTranscriber` (`progressiveLongDictation`) as a second local option for locales the first does not cover or to test custom vocabulary. Which has lower WER on real pt-BR meetings is unknown; measure.

New in macOS 27 in this interface [SDK] (all marked `anyAppleOS 27`): `AnalyzerInputConverter`, `AssetInputSequenceProvider`, `CaptureInputSequenceProvider`, `AnalyzerInput.init(buffer: CMReadySampleBuffer<...>)` with `bufferDuration` and `bufferFormat`, `SpeechAnalyzer.Options.ignoresResourceLimits`, `SFSpeechError.Code.cannotConfigureAudioSystem`. Everything else is `anyAppleOS 26`. Behavior changes between 26 and 27 beyond this symbol list are unverified.

## 9. Errors, cancellation, long sessions

Errors [SDK]: when the analyzer or a module's result stream throws, the session becomes finished, and the same error (or `CancellationError`) comes out of every waiting method and stream. Create a new analyzer to continue. Useful `SFSpeechError.Code` values: `audioDisordered`, `unexpectedAudioFormat`, `noModel`, `assetLocaleNotAllocated`, `tooManyAssetLocalesAllocated`, `incompatibleAudioFormats`, `moduleOutputFailed`, `cannotAllocateUnsupportedLocale`, `insufficientResources`, `cannotConfigureAudioSystem` (27).

Finishing [SDK]:

- `finalizeAndFinishThroughEndOfInput()`: at end of recording, turns pending volatile results into finals and closes the streams.
- `finalizeAndFinish(through:)` with the `CMTime` returned by `analyzeSequence(_:)`.
- `cancelAndFinishNow()`: stops immediately without finalizing.
- Finishing your input `AsyncStream` does not end the session (exception: `finalizeAndFinishThroughEndOfInput`). You must call a `finish*` method or release the analyzer.
- `start(inputSequence:)` replaces the previous sequence; unconsumed audio is ignored. Call `finalize(through:)` first if you do not want to lose it.
- Cancelling the task running `analyzeSequence` makes it return the last timecode without throwing `CancellationError`.

Hours-long sessions: the docs say nothing about duration limits, memory or thermal behavior, so those are unverified. Apple says the model runs outside the app's memory [Apple]. To verify: a 2 to 4 hour soak test fed from a looped audio file (not the mic), sampling the sidecar's RSS, `ProcessInfo.thermalState` and power with `powermetrics`. A reasonable mitigation without evidence behind it: restart the analyzer at a long silence (for example every 30 to 60 minutes), keeping a global timeline in the sidecar.

`SpeechDetector` [SDK]: works only together with a `SpeechTranscriber` or `DictationTranscriber`. `SpeechDetector()` uses `.medium` and reports nothing. `SpeechDetector(detectionOptions:reportResults:)` with `reportResults: true` exposes `results` with `speechDetected`, but the docs say these results "currently only support error handling from the VAD model". Do not use it for speech start/end markers; use it only to save power. The docs also warn that the VAD can drop real speech and recommend `.medium`.

## 10. Recommended sidecar architecture

```
mic (AVAudioEngine, VP) --tap--> serial queue A --convert--> AsyncStream<AnalyzerInput> A --> SpeechAnalyzer A [SpeechTranscriber]
system (process tap)  --IOProc-> serial queue B --convert--> AsyncStream<AnalyzerInput> B --> SpeechAnalyzer B [SpeechTranscriber]

Task A: for try await r in transcriberA.results --\
Task B: for try await r in transcriberB.results ---+--> actor Emitter --> stdout (JSON lines)
```

Rules:

- Two analyzers, two transcribers, same locale and preset (they share the engine [SDK]). One `source` per stream: `mic` or `system`.
- Common clock: build `bufferStartTime` from each buffer's host time (`AVAudioTime.hostTime` in the mic tap, `AudioTimeStamp` in the system tap IOProc), converted to seconds since a single sidecar `t0`, so the two streams' `range` values are comparable. Drift between devices is unverified; check by playing an audible click into both.
- One `Emitter` actor serializes writes to stdout (one line per event, `\n`, flush per line). Logs go to stderr.
- Restart a failing stream without killing the other and emit an `error` event.

JSON lines schema:

```json
{"v":1,"type":"partial","source":"mic","seq":812,"lang":"pt-BR","text":"voce pode me explicar como","start":12.340,"end":14.020,"t_emit":15.101}
{"v":1,"type":"final","source":"mic","seq":813,"lang":"pt-BR","text":"Voce pode me explicar como funciona o contrato?","start":12.340,"end":15.480,"conf":0.91,"words":[{"w":"Voce","s":12.34,"e":12.62,"c":0.95}],"t_emit":15.790}
{"v":1,"type":"status","source":"system","event":"started|finished|asset_downloading","progress":0.4}
{"v":1,"type":"error","source":"system","code":"insufficientResources","message":"..."}
```

`start`/`end` come from `result.range` in seconds on the sidecar clock; `t_emit` is the wall-clock time the event was written, so end-to-end latency can be computed. `seq` is monotonic per source. The question detector consumes `partial` events and replaces the text of the span with the same `start`; `final` closes the span.

Emission and lifecycle (not compiled; `Event` and `ErrorEvent` are your own Encodable types):

```swift
actor Emitter {
    private let enc = JSONEncoder()
    func emit<T: Encodable & Sendable>(_ e: T) {
        guard let d = try? enc.encode(e) else { return }
        FileHandle.standardOutput.write(d + Data([0x0A]))
    }
}

func consume(_ t: SpeechTranscriber, source: String, emitter: Emitter, clock: @Sendable () -> Double) async {
    var seq = 0
    do {
        for try await r in t.results {
            seq += 1
            await emitter.emit(Event(type: r.isFinal ? "final" : "partial", source: source, seq: seq,
                                     text: r.text.plain, start: r.range.start.seconds,
                                     end: r.range.end.seconds, tEmit: clock()))
        }
    } catch { await emitter.emit(ErrorEvent(source: source, message: "\(error)")) }
}

func run(source: String, engine: LocalEngine, emitter: Emitter) async throws {
    let analyzer = SpeechAnalyzer(modules: [engine.transcriber],
                                  options: .init(priority: .userInitiated, modelRetention: .whileInUse))
    try await analyzer.prepareToAnalyze(in: engine.format)
    let (stream, cont) = AsyncStream.makeStream(of: AnalyzerInput.self)
    // The capture side calls cont.yield(AnalyzerInput(buffer: converted, bufferStartTime: cmTime))
    async let reading: Void = consume(engine.transcriber, source: source, emitter: emitter) { now() }
    let last = try await analyzer.analyzeSequence(stream)   // returns when cont.finish() is called
    if let last { try await analyzer.finalizeAndFinish(through: last) } else { await analyzer.cancelAndFinishNow() }
    await reading
}
```

`Options(priority:modelRetention:)` exists on 26 and 27 [SDK]. `now()` is your monotonic clock. The default unbounded `AsyncStream` buffer avoids silently dropping audio; monitor its depth to detect real lag.

Fallback: `SpeechTranscriber.isAvailable == false`, `status == .unsupported`, `noModel`, an asset error, or a benchmark over threshold switches to the cloud STT. The sidecar emits the same schema in both cases.

## 11. Onboarding benchmark procedure

Input: a bundled audio file (pt-BR and en-US) with reference text and, ideally, per-sentence start/end marks. It never touches the microphone. Feed `AnalyzerInput` buffers at real-time pace (a `Task.sleep` per buffer), not with `analyzeSequence(from:)` on a file, which runs faster than real time and hides streaming latency.

Metrics:

1. Time to first partial: wall-clock emission of the first `partial` minus the moment the first speech buffer was delivered. Run cold (no `prepareToAnalyze`) and warm.
2. End of speech to final: for each reference sentence, `t_emit` of the `final` covering the sentence end minus the delivery time of that sentence's last buffer. Report median and p95. Also measure with your own `finalize(through:)` call at sentence end (simulating a VAD you control), because automatic finalization timing is not guaranteed.
3. Partial lag: for each final word, the time between the word's end in the audio (`audioTimeRange.end`) and the first partial containing it.
4. WER: normalize (lowercase, strip punctuation, spell numbers like the reference), join only `final` results, compute word-level Levenshtein distance divided by reference word count. Compute pt-BR and English separately.
5. Real-time factor with accelerated feed (for example 4x): processing time over audio duration, to see headroom on the M4 Pro with two streams.
6. Resources: sidecar RSS and `thermalState` at start and end.

Suggested fallback thresholds (to calibrate, not from any source): first partial above 1.5 s, median end-of-speech-to-final above 1.0 s, or pt-BR WER above what you accept, switches to cloud. Run it for each installed locale and also for `DictationTranscriber` as an alternative candidate.

## 12. Limitations and gotchas

1. Partials with `fastResults` are less accurate and can be revoked (empty text revokes the range) [SDK]. The question detector must tolerate changes and must not trigger irreversible actions on a partial. Verified in the docs; actual revision rates for pt-BR are unverified.
2. `SpeechDetector` is not an end-of-speech marker: its results only report VAD errors today, and the VAD can drop speech [SDK].
3. WWDC25 names have changed: `deallocate(locale:)`, `.offlineTranscription`, `.progressiveLiveTranscription` and the sample's `convertBuffer(_:to:)` do not appear in the SDK 27 interface [Apple vs SDK]. Use `release(reservedLocale:)`, `.transcription`, `.progressiveTranscription`, and your own conversion or `AnalyzerInputConverter`.
4. Finishing the input `AsyncStream` does not close the session; without a `finish*` call the result streams stay open [SDK].
5. Modules cannot be reused across analyzers [SDK]; create new transcribers when restarting.
6. The analyzer needs 16 kHz (or 8 kHz) Int16 mono here; 48 kHz float input must be converted, and a new converter per buffer corrupts resampling.
7. Assets need the network the first time and are system-managed; reservations are limited (5 here) [measured here].
8. Unverified: speech authorization requirement, TCC attribution for the sidecar, multi-hour behavior, `contextualStrings` on `SpeechTranscriber`, and real pt-BR streaming latency.

## Sources

- Local SDK: `/Library/Developer/CommandLineTools/SDKs/MacOSX.sdk/System/Library/Frameworks/Speech.framework/Versions/A/Modules/Speech.swiftmodule/arm64e-apple-macos.swiftinterface` and `arm64e-apple-macos.swiftdoc` (embedded documentation), plus `Headers/SFSpeechRecognizer.h`
- WWDC25, Bring advanced speech-to-text to your app with SpeechAnalyzer: https://developer.apple.com/videos/play/wwdc2025/277/
- Apple Speech framework documentation (not read directly; equivalent text came from the local swiftdoc): https://developer.apple.com/documentation/speech
- IraVoice, Apple SpeechAnalyzer vs whisper.cpp (commercial interest disclosed): https://dev.to/iravoice/apple-speechanalyzer-vs-whispercpp-a-40-speaker-mac-benchmark-40i4
- GIGAZINE on the MacStories test, 2025-06-19: https://gigazine.net/gsc_news/en/20250619-apple-speech-analyzer
- Community guides on permissions (conflicting): https://levelup.gitconnected.com/bring-advanced-speech-to-text-to-your-app-with-speechanalyzer-6ed25a84586c and https://pub.dev/packages/apple_speech
