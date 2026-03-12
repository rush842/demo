import Foundation
import CoreGraphics
import AppKit

struct ActivityRecord {
    var date: String
    var activeSeconds: Int
    var idleSeconds: Int
    var mouseClicks: Int
    var keystrokes: Int
    var activeApp: String
    var windowTitle: String
}

actor ActivityTracker {
    let api: ApiClient
    let macAddress: String
    let organizationId: Int
    var lastActivity = Date()
    var mouseClicks = 0
    var keystrokes = 0
    var currentApp = ""
    var currentWindow = ""
    private let idleThreshold: TimeInterval = 60

    init(api: ApiClient, macAddress: String, organizationId: Int) {
        self.api = api
        self.macAddress = macAddress
        self.organizationId = organizationId
    }

    func recordClick() { mouseClicks += 1; lastActivity = Date() }
    func recordKey() { keystrokes += 1; lastActivity = Date() }

    func run() async {
        // Start event monitor in background
        Task.detached { [weak self] in
            await self?.startEventMonitor()
        }

        // Upload activity every 5 minutes
        while !Task.isCancelled {
            try? await Task.sleep(nanoseconds: 300_000_000_000)

            let settings = await api.fetchMonitoringSettings(organizationId: organizationId)
            guard settings.activityTracking else { continue }

            let now = Date()
            let idleSince = now.timeIntervalSince(lastActivity)
            let isIdle = idleSince > idleThreshold

            let frontApp = NSWorkspace.shared.frontmostApplication?.localizedName ?? "Unknown"
            let payload: [String: Any] = [
                "macaddress": macAddress,
                "organization_id": organizationId,
                "timestamp": isoTimestamp(),
                "active_app": frontApp,
                "window_title": frontApp,
                "mouse_clicks": mouseClicks,
                "keystrokes": keystrokes,
                "is_idle": isIdle,
                "idle_seconds": Int(max(0, idleSince))
            ]

            mouseClicks = 0
            keystrokes = 0

            do {
                try await api.uploadActivity(payload: payload)
            } catch {
                NSLog("[DawellService] Activity upload failed: \(error)")
            }
        }
    }

    private func startEventMonitor() async {
        // Monitor mouse clicks
        NSEvent.addGlobalMonitorForEvents(matching: .leftMouseDown) { [weak self] _ in
            Task { await self?.recordClick() }
        }
        NSEvent.addGlobalMonitorForEvents(matching: .rightMouseDown) { [weak self] _ in
            Task { await self?.recordClick() }
        }
        // Monitor keystrokes (count only, no content)
        NSEvent.addGlobalMonitorForEvents(matching: .keyDown) { [weak self] _ in
            Task { await self?.recordKey() }
        }
        NSLog("[DawellService] Activity monitoring active")
    }
}
