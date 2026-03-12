import Foundation
import ScreenCaptureKit
import CoreGraphics
import AppKit

actor StreamingClient {
    let wsURL: String
    let macAddress: String
    let userId: Int
    let organizationId: Int
    private var wsClient: WebSocketClient?
    private var capture: ScreenStreamCapture?
    private var isStreaming = false
    private var streamTarget: String?

    init(wsURL: String, macAddress: String, userId: Int, organizationId: Int) {
        self.wsURL = wsURL
        self.macAddress = macAddress
        self.userId = userId
        self.organizationId = organizationId
    }

    func start() async {
        let client = WebSocketClient()
        self.wsClient = client

        await client.connect(url: wsURL) { [weak self] message in
            Task { await self?.handleMessage(message) }
        }

        // Register as desktop
        let payload: [String: Any] = [
            "type": "register_desktop",
            "macaddress": macAddress,
            "userId": userId,
            "organizationId": organizationId
        ]
        if let data = try? JSONSerialization.data(withJSONObject: payload),
           let json = String(data: data, encoding: .utf8) {
            try? await client.send(json)
        }

        NSLog("[DawellService] Streaming client connected")
    }

    func stop() async {
        await stopStream()
        await wsClient?.disconnect()
        wsClient = nil
    }

    private func handleMessage(_ message: String) async {
        guard let data = message.data(using: .utf8),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let type = json["type"] as? String else { return }

        switch type {
        case "start_stream":
            let viewerId = json["viewerId"] as? String ?? ""
            NSLog("[DawellService] Start stream request from viewer: \(viewerId)")
            await startStream(viewerId: viewerId)

        case "stop_stream":
            NSLog("[DawellService] Stop stream request")
            await stopStream()

        case "remote_input":
            if let event = json["event"] as? [String: Any] {
                await handleRemoteInput(event)
            }

        default:
            break
        }
    }

    private func startStream(viewerId: String) async {
        guard !isStreaming else { return }
        isStreaming = true
        streamTarget = viewerId

        let cap = ScreenStreamCapture()
        self.capture = cap

        do {
            try await cap.startCapture { [weak self] cgImage in
                Task { await self?.sendFrame(cgImage) }
            }
        } catch {
            NSLog("[DawellService] Failed to start stream: \(error)")
            isStreaming = false
        }
    }

    private func stopStream() async {
        guard isStreaming else { return }
        isStreaming = false
        streamTarget = nil
        await capture?.stopCapture()
        capture = nil
    }

    private func sendFrame(_ cgImage: CGImage) async {
        guard isStreaming, let client = wsClient else { return }
        let jpeg = jpegData(from: cgImage, quality: "medium")
        let base64 = jpeg.base64EncodedString()
        let payload: [String: Any] = [
            "type": "stream_frame",
            "macaddress": macAddress,
            "frame": base64,
            "width": cgImage.width,
            "height": cgImage.height
        ]
        if let data = try? JSONSerialization.data(withJSONObject: payload),
           let json = String(data: data, encoding: .utf8) {
            try? await client.send(json)
        }
    }

    private func handleRemoteInput(_ event: [String: Any]) async {
        guard let eventType = event["type"] as? String else { return }

        switch eventType {
        case "mousemove":
            if let xPct = event["x"] as? Double, let yPct = event["y"] as? Double {
                let screen = NSScreen.main?.frame ?? .zero
                let x = xPct * screen.width
                let y = (1.0 - yPct) * screen.height
                let pos = CGPoint(x: x, y: y)
                CGDisplayMoveCursorToPoint(CGMainDisplayID(), pos)
            }

        case "mousedown":
            CGEvent(mouseEventSource: nil, mouseType: .leftMouseDown,
                    mouseCursorPosition: NSEvent.mouseLocation.cgPoint,
                    mouseButton: .left)?.post(tap: .cghidEventTap)

        case "mouseup":
            CGEvent(mouseEventSource: nil, mouseType: .leftMouseUp,
                    mouseCursorPosition: NSEvent.mouseLocation.cgPoint,
                    mouseButton: .left)?.post(tap: .cghidEventTap)

        case "keydown":
            if let keyCode = event["keyCode"] as? Int {
                let src = CGEventSource(stateID: .combinedSessionState)
                CGEvent(keyboardEventSource: src, virtualKey: CGKeyCode(keyCode), keyDown: true)?.post(tap: .cghidEventTap)
            }

        case "keyup":
            if let keyCode = event["keyCode"] as? Int {
                let src = CGEventSource(stateID: .combinedSessionState)
                CGEvent(keyboardEventSource: src, virtualKey: CGKeyCode(keyCode), keyDown: false)?.post(tap: .cghidEventTap)
            }

        default:
            break
        }
    }
}

extension NSPoint {
    var cgPoint: CGPoint { CGPoint(x: x, y: y) }
}
