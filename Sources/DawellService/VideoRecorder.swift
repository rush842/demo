import Foundation
import AVFoundation
import CoreGraphics
import AppKit

actor VideoRecorder {
    let api: ApiClient
    let macAddress: String
    let organizationId: Int

    init(api: ApiClient, macAddress: String, organizationId: Int) {
        self.api = api
        self.macAddress = macAddress
        self.organizationId = organizationId
    }

    func run() async {
        while !Task.isCancelled {
            let settings = await api.fetchCaptureSettings(organizationId: organizationId)
            guard settings.videoEnabled else {
                try? await Task.sleep(nanoseconds: 30_000_000_000)
                continue
            }

            let duration = max(60, settings.videoDuration)
            NSLog("[DawellService] Starting video recording (\(duration)s)...")

            do {
                let videoData = try await recordVideo(durationSeconds: duration)
                NSLog("[DawellService] Video recorded (\(videoData.count) bytes), uploading...")
                try await api.uploadVideo(
                    videoData: videoData,
                    macAddress: macAddress,
                    organizationId: organizationId,
                    mimeType: "video/mp4",
                    filename: "recording.mp4"
                )
                NSLog("[DawellService] Video uploaded")
            } catch {
                NSLog("[DawellService] Video recording failed: \(error)")
                try? await Task.sleep(nanoseconds: 60_000_000_000)
            }
        }
    }

    private func recordVideo(durationSeconds: Int) async throws -> Data {
        let tmpURL = FileManager.default.temporaryDirectory
            .appendingPathComponent("dawell_video_\(Int(Date().timeIntervalSince1970)).mp4")
        defer { try? FileManager.default.removeItem(at: tmpURL) }

        let writer = try AVAssetWriter(outputURL: tmpURL, fileType: .mp4)
        let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
        guard let display = content.displays.first else {
            throw NSError(domain: "DawellService", code: 4, userInfo: [NSLocalizedDescriptionKey: "No display"])
        }

        // Scale down for video (halve resolution)
        let width = (display.width / 2) & ~1
        let height = (display.height / 2) & ~1

        let videoSettings: [String: Any] = [
            AVVideoCodecKey: AVVideoCodecType.h264,
            AVVideoWidthKey: width,
            AVVideoHeightKey: height,
            AVVideoCompressionPropertiesKey: [
                AVVideoAverageBitRateKey: 1_500_000,
                AVVideoProfileLevelKey: AVVideoProfileLevelH264HighAutoLevel
            ]
        ]

        let input = AVAssetWriterInput(mediaType: .video, outputSettings: videoSettings)
        input.expectsMediaDataInRealTime = true
        let adaptor = AVAssetWriterInputPixelBufferAdaptor(
            assetWriterInput: input,
            sourcePixelBufferAttributes: [
                kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                kCVPixelBufferWidthKey as String: width,
                kCVPixelBufferHeightKey as String: height
            ]
        )

        writer.add(input)
        writer.startWriting()
        writer.startSession(atSourceTime: .zero)

        let fps = 5
        let totalFrames = durationSeconds * fps
        let frameDuration = CMTime(value: 1, timescale: CMTimeScale(fps))

        let capture = ScreenStreamCapture()
        var frameCount = 0
        var startTime: CMTime = .zero

        var frameQueue: [(CGImage, CMTime)] = []
        let frameLock = NSLock()

        try await capture.startCapture { cgImage in
            let pts = CMTime(value: CMTimeValue(frameCount), timescale: CMTimeScale(fps))
            frameLock.lock()
            frameQueue.append((cgImage, pts))
            frameLock.unlock()
        }

        let deadline = Date().addingTimeInterval(TimeInterval(durationSeconds))
        while Date() < deadline && frameCount < totalFrames {
            frameLock.lock()
            let frames = frameQueue
            frameQueue.removeAll()
            frameLock.unlock()

            for (cgImage, pts) in frames {
                guard input.isReadyForMoreMediaData else { break }
                if let buffer = pixelBuffer(from: cgImage, width: width, height: height, pool: adaptor.pixelBufferPool) {
                    adaptor.append(buffer, withPresentationTime: pts)
                }
                frameCount += 1
                if frameCount >= totalFrames { break }
            }
            try? await Task.sleep(nanoseconds: 100_000_000)
        }

        await capture.stopCapture()
        input.markAsFinished()
        await writer.finishWriting()

        return try Data(contentsOf: tmpURL)
    }
}

private func pixelBuffer(from cgImage: CGImage, width: Int, height: Int, pool: CVPixelBufferPool?) -> CVPixelBuffer? {
    var pixelBuffer: CVPixelBuffer?
    if let pool = pool {
        CVPixelBufferPoolCreatePixelBuffer(nil, pool, &pixelBuffer)
    } else {
        CVPixelBufferCreate(nil, width, height, kCVPixelFormatType_32BGRA, nil, &pixelBuffer)
    }
    guard let pb = pixelBuffer else { return nil }
    CVPixelBufferLockBaseAddress(pb, [])
    defer { CVPixelBufferUnlockBaseAddress(pb, []) }

    guard let context = CGContext(
        data: CVPixelBufferGetBaseAddress(pb),
        width: width, height: height,
        bitsPerComponent: 8,
        bytesPerRow: CVPixelBufferGetBytesPerRow(pb),
        space: CGColorSpaceCreateDeviceRGB(),
        bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue
    ) else { return nil }

    context.draw(cgImage, in: CGRect(x: 0, y: 0, width: width, height: height))
    return pb
}

// Import ScreenCaptureKit for VideoRecorder
import ScreenCaptureKit
