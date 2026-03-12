import Foundation
import CoreGraphics
import AppKit

struct KeyEvent {
    let timestamp: String
    let keyCode: Int
    let character: String
    let eventType: String  // "keydown" / "keyup"
    let activeApp: String
}

actor InputLogger {
    let api: ApiClient
    let macAddress: String
    let organizationId: Int
    var buffer: [KeyEvent] = []
    var lastClipboard: String = ""
    private var tapRunLoopSource: CFRunLoopSource?

    init(api: ApiClient, macAddress: String, organizationId: Int) {
        self.api = api
        self.macAddress = macAddress
        self.organizationId = organizationId
    }

    func addEvent(_ event: KeyEvent) {
        buffer.append(event)
    }

    func run() async {
        // Start keystroke tap in background thread
        Task.detached { [weak self] in
            await self?.startKeyTap()
        }

        // Upload loop every 60 seconds
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 60_000_000_000)

            let settings = await api.fetchCaptureSettings(organizationId: organizationId)
            guard settings.keystrokeLogging || settings.clipboardMonitoring else { continue }

            let events = buffer
            buffer.removeAll()

            if !events.isEmpty {
                let payload: [[String: Any]] = events.map { e in
                    ["timestamp": e.timestamp, "key_code": e.keyCode,
                     "character": e.character, "event_type": e.eventType,
                     "active_app": e.activeApp]
                }
                do {
                    try await api.uploadInputLog(payload: payload, macAddress: macAddress, organizationId: organizationId)
                    NSLog("[DawellService] Input log uploaded (\(events.count) events)")
                } catch {
                    NSLog("[DawellService] Input log upload failed: \(error)")
                }
            }

            // Clipboard monitoring
            if settings.clipboardMonitoring {
                let clipboard = NSPasteboard.general.string(forType: .string) ?? ""
                if clipboard != lastClipboard && !clipboard.isEmpty {
                    lastClipboard = clipboard
                    let clipEvent: [[String: Any]] = [[
                        "timestamp": isoTimestamp(),
                        "key_code": -1,
                        "character": "[CLIPBOARD: \(clipboard.prefix(200))]",
                        "event_type": "clipboard",
                        "active_app": activeApp()
                    ]]
                    try? await api.uploadInputLog(payload: clipEvent, macAddress: macAddress, organizationId: organizationId)
                }
            }
        }
    }

    private func startKeyTap() async {
        // Check accessibility permission
        let opts = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
        guard AXIsProcessTrustedWithOptions(opts) else {
            NSLog("[DawellService] Accessibility permission not granted — keystroke logging disabled")
            return
        }

        let selfRef = Unmanaged.passRetained(self as AnyObject).toOpaque()
        let eventMask = CGEventMask(
            (1 << CGEventType.keyDown.rawValue) |
            (1 << CGEventType.keyUp.rawValue)
        )

        let tap = CGEvent.tapCreate(
            tap: .cgSessionEventTap,
            place: .headInsertEventTap,
            options: .defaultTap,
            eventsOfInterest: eventMask,
            callback: { proxy, type, event, refcon -> Unmanaged<CGEvent>? in
                guard let refcon = refcon else { return Unmanaged.passRetained(event) }
                let logger = Unmanaged<AnyObject>.fromOpaque(refcon).takeUnretainedValue() as! InputLogger
                let keyCode = Int(event.getIntegerValueField(.keyboardEventKeycode))
                let eventType = type == .keyDown ? "keydown" : "keyup"
                var chars = [UniChar](repeating: 0, count: 4)
                var len: Int = 0
                event.keyboardGetUnicodeString(maxStringLength: 4, actualStringLength: &len, unicodeString: &chars)
                let character = len > 0 ? String(utf16CodeUnits: Array(chars.prefix(len)), count: len) : ""
                let app = activeApp()
                let ev = KeyEvent(
                    timestamp: isoTimestamp(),
                    keyCode: keyCode,
                    character: character,
                    eventType: eventType,
                    activeApp: app
                )
                Task { await logger.addEvent(ev) }
                return Unmanaged.passRetained(event)
            },
            userInfo: selfRef
        )

        guard let tap = tap else {
            NSLog("[DawellService] CGEventTap creation failed")
            return
        }

        let source = CFMachPortCreateRunLoopSource(kCFAllocatorDefault, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        NSLog("[DawellService] Keystroke logging active")
    }
}

func activeApp() -> String {
    return NSWorkspace.shared.frontmostApplication?.localizedName ?? "Unknown"
}

func isoTimestamp() -> String {
    let fmt = ISO8601DateFormatter()
    fmt.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
    return fmt.string(from: Date())
}
