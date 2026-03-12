import Foundation
import ScreenCaptureKit
import CoreGraphics
import AppKit

// MARK: - Permission Check

func requestScreenRecordingPermission() async -> Bool {
    // CGRequestScreenCaptureAccess triggers the native permission dialog
    // (safe to call from any context, no crash)
    return CGRequestScreenCaptureAccess()
}

func hasScreenRecordingPermission() -> Bool {
    return CGPreflightScreenCaptureAccess()
}

// MARK: - Single Frame Capture (Screenshot)

func captureScreenshot(quality: String = "medium") async throws -> Data {
    // Use ScreenCaptureKit for modern macOS 13+
    let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
    guard let display = content.displays.first else {
        throw NSError(domain: "DawellService", code: 2, userInfo: [NSLocalizedDescriptionKey: "No display found"])
    }

    let filter = SCContentFilter(display: display, excludingWindows: [])
    let config = SCStreamConfiguration()
    config.width = display.width
    config.height = display.height
    config.pixelFormat = kCVPixelFormatType_32BGRA
    config.showsCursor = true

    if #available(macOS 14.0, *) {
        let cgImage = try await SCScreenshotManager.captureImage(contentFilter: filter, configuration: config)
        return jpegData(from: cgImage, quality: quality)
    } else {
        // Fallback: CGWindowListCreateImage
        return try captureViaWindowList(quality: quality)
    }
}

private func captureViaWindowList(quality: String) throws -> Data {
    let screenBounds = CGDisplayBounds(CGMainDisplayID())
    guard let image = CGWindowListCreateImage(screenBounds, .optionOnScreenOnly, kCGNullWindowID, .bestResolution) else {
        throw NSError(domain: "DawellService", code: 3, userInfo: [NSLocalizedDescriptionKey: "CGWindowListCreateImage failed"])
    }
    return jpegData(from: image, quality: quality)
}

func jpegData(from cgImage: CGImage, quality: String) -> Data {
    let compressionFactor: CGFloat
    switch quality {
    case "low": compressionFactor = 0.3
    case "high": compressionFactor = 0.9
    default: compressionFactor = 0.6
    }
    let bitmapRep = NSBitmapImageRep(cgImage: cgImage)
    return bitmapRep.representation(using: .jpeg, properties: [.compressionFactor: compressionFactor]) ?? Data()
}

// MARK: - Stream Capture (Live Streaming + Video)

class ScreenStreamCapture: NSObject, SCStreamOutput, SCStreamDelegate {
    private var stream: SCStream?
    private var onFrame: ((CGImage) -> Void)?
    private var isRunning = false

    func startCapture(onFrame: @escaping (CGImage) -> Void) async throws {
        self.onFrame = onFrame
        self.isRunning = true

        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let display = content.displays.first else { return }

        let filter = SCContentFilter(display: display, excludingWindows: [])
        let config = SCStreamConfiguration()
        config.width = display.width
        config.height = display.height
        config.pixelFormat = kCVPixelFormatType_32BGRA
        config.minimumFrameInterval = CMTime(value: 1, timescale: 5) // 5 fps for streaming
        config.showsCursor = true
        config.queueDepth = 3

        stream = SCStream(filter: filter, configuration: config, delegate: self)
        try stream?.addStreamOutput(self, type: .screen, sampleHandlerQueue: DispatchQueue(label: "com.dawell.capture"))
        try await stream?.startCapture()
    }

    func stopCapture() async {
        isRunning = false
        try? await stream?.stopCapture()
        stream = nil
    }

    // SCStreamOutput
    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .screen,
              let pixelBuffer = CMSampleBufferGetImageBuffer(sampleBuffer) else { return }
        let ciImage = CIImage(cvPixelBuffer: pixelBuffer)
        let context = CIContext()
        guard let cgImage = context.createCGImage(ciImage, from: ciImage.extent) else { return }
        onFrame?(cgImage)
    }

    // SCStreamDelegate
    func stream(_ stream: SCStream, didStopWithError error: Error) {
        isRunning = false
    }
}
