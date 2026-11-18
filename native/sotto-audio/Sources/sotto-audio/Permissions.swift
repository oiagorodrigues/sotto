import AVFoundation
import CoreGraphics
import Foundation
import Speech
import SottoProtocol

/// System audio has no query API: the status stays `not_determined` until a tap has been created
/// once, because macOS only asks for consent at that moment.
final class PermissionCenter: @unchecked Sendable {
    private let lock = NSLock()
    private var systemAudioGranted = false

    func markSystemAudioGranted() {
        lock.lock()
        systemAudioGranted = true
        lock.unlock()
    }

    func states() -> [PermissionState] {
        lock.lock()
        let system = systemAudioGranted
        lock.unlock()
        return [
            PermissionState(permission: .microphone, status: Self.microphone()),
            PermissionState(permission: .systemAudio, status: system ? .granted : .notDetermined),
            PermissionState(permission: .speechRecognition, status: Self.speech()),
            PermissionState(permission: .screenRecording, status: CGPreflightScreenCaptureAccess() ? .granted : .notDetermined),
        ]
    }

    func request(_ kind: PermissionKind) async {
        switch kind {
        case .microphone:
            _ = await AVCaptureDevice.requestAccess(for: .audio)
        case .speechRecognition:
            await withCheckedContinuation { (c: CheckedContinuation<Void, Never>) in
                SFSpeechRecognizer.requestAuthorization { _ in c.resume() }
            }
        case .screenRecording:
            _ = CGRequestScreenCaptureAccess()
        case .systemAudio:
            if SystemAudioCapture.probe() { markSystemAudioGranted() }
        }
    }

    static func microphone() -> PermissionStatus {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: .granted
        case .denied: .denied
        case .restricted: .restricted
        case .notDetermined: .notDetermined
        @unknown default: .notDetermined
        }
    }

    static func speech() -> PermissionStatus {
        switch SFSpeechRecognizer.authorizationStatus() {
        case .authorized: .granted
        case .denied: .denied
        case .restricted: .restricted
        case .notDetermined: .notDetermined
        @unknown default: .notDetermined
        }
    }
}
