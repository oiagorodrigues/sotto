import CoreGraphics
import Foundation
import ImageIO
import ScreenCaptureKit
import SottoAudioCore
import UniformTypeIdentifiers

struct ScreenshotResult {
    let png: Data
    let width: Int
    let height: Int
}

enum ScreenshotError: Error, CustomStringConvertible {
    case permission
    case noDisplay
    case encode

    var description: String {
        switch self {
        case .permission: "screen recording permission is not granted"
        case .noDisplay: "no display found"
        case .encode: "could not encode the PNG"
        }
    }
}

enum Screenshotter {
    /// Captures the display that holds the parent's windows at any layer, preferring the overlay (or the main display),
    /// without any window owned by `parentPID`.
    static func capture(parentPID: pid_t, maxLongSide: UInt32) async throws -> ScreenshotResult {
        guard CGPreflightScreenCaptureAccess() else { throw ScreenshotError.permission }
        let content: SCShareableContent
        do {
            content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        } catch let error as NSError where error.domain == SCStreamErrorDomain && error.code == SCStreamError.userDeclined.rawValue {
            throw ScreenshotError.permission
        }

        let anchor = parentWindowCenter(ownerPID: parentPID)
        let display = anchor.flatMap { point in content.displays.first { $0.frame.contains(point) } }
            ?? content.displays.first { $0.displayID == CGMainDisplayID() }
            ?? content.displays.first
        guard let display else { throw ScreenshotError.noDisplay }

        let excluded = content.windows.filter { $0.owningApplication?.processID == parentPID }
        let filter = SCContentFilter(display: display, excludingWindows: excluded)

        let info = SCShareableContent.info(for: filter)
        let scale = CGFloat(info.pointPixelScale)
        var width = info.contentRect.width * scale
        var height = info.contentRect.height * scale
        let longSide = max(width, height)
        if maxLongSide > 0, longSide > CGFloat(maxLongSide) {
            let factor = CGFloat(maxLongSide) / longSide
            width *= factor
            height *= factor
        }

        let config = SCStreamConfiguration()
        config.width = max(1, Int(width.rounded()))
        config.height = max(1, Int(height.rounded()))
        config.showsCursor = false
        let image = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)

        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil) else {
            throw ScreenshotError.encode
        }
        CGImageDestinationAddImage(destination, image, nil)
        guard CGImageDestinationFinalize(destination) else { throw ScreenshotError.encode }
        return ScreenshotResult(png: data as Data, width: image.width, height: image.height)
    }

    /// `CGWindowListCopyWindowInfo` returns windows front to back, which `SCShareableContent` does not promise.
    private static func parentWindowCenter(ownerPID: pid_t) -> CGPoint? {
        guard let list = CGWindowListCopyWindowInfo([.optionOnScreenOnly], kCGNullWindowID) as? [[String: Any]] else {
            return nil
        }
        let windows = list.compactMap { window -> WindowSnapshot? in
            guard let pid = window[kCGWindowOwnerPID as String] as? Int32,
                  let layer = window[kCGWindowLayer as String] as? Int,
                  let bounds = window[kCGWindowBounds as String] as? NSDictionary,
                  let rect = CGRect(dictionaryRepresentation: bounds)
            else { return nil }
            return WindowSnapshot(ownerPID: pid, layer: layer, bounds: rect)
        }
        return DisplayAnchor.point(for: windows, ownerPID: ownerPID)
    }
}
