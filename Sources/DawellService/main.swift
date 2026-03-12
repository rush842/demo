import Foundation
import AppKit

// ─────────────────────────────────────────────────────────────
// Entry Point
// ─────────────────────────────────────────────────────────────

let args = CommandLine.arguments

func printUsage() {
    print("Usage: dawellservice --token=<BASE64_TOKEN>")
    print("       dawellservice --run")
    print("       dawellservice --permissions")
    print("       dawellservice --uninstall")
}

// Parse arguments
var token: String?
var runMode = false
var uninstallMode = false
var permissionsMode = false

for arg in args.dropFirst() {
    if arg.hasPrefix("--token=") {
        token = String(arg.dropFirst("--token=".count))
    } else if arg == "--run" {
        runMode = true
    } else if arg == "--uninstall" {
        uninstallMode = true
    } else if arg == "--permissions" {
        permissionsMode = true
    }
}

if permissionsMode {
    let exe = CommandLine.arguments[0]
    requestMacOSPermissions(binaryPath: exe)
    exit(0)
}

if uninstallMode {
    print("Uninstalling DawellService...")
    guard let config = loadConfig() else {
        print("No config found.")
        uninstallService()
        exit(0)
    }
    let api = ApiClient(baseURL: config.apiBaseUrl)
    let sysInfo = SystemInfo.collect()
    Task {
        await unregisterViaWebSocket(config: config, sysInfo: sysInfo)
        await api.setOffline(macAddress: sysInfo.macAddress)
        uninstallService()
        deleteConfig()
        print("DawellService uninstalled.")
        exit(0)
    }
    RunLoop.main.run()
}

if runMode {
    // Background service mode
    guard let config = loadConfig() else {
        NSLog("[DawellService] No config found. Run with --token first.")
        exit(1)
    }
    NSLog("[DawellService] Starting DawellService (user_id=\(config.userId), org=\(config.organizationId))")
    runService(config: config)
    RunLoop.main.run()
}

if let token = token {
    // Install mode
    print("Installing DawellService...")
    print()

    do {
        let (userId, orgId) = try decodeToken(token)
        print("[1/5] Token decoded (user_id=\(userId), org_id=\(orgId))")

        let config = ServiceConfig(
            userId: userId,
            organizationId: orgId,
            apiBaseUrl: kApiBaseURL,
            wsUrl: kWsURL
        )

        let sysInfo = SystemInfo.collect()
        print("[2/5] System info collected")
        print("  Machine ID:  \(sysInfo.machineId)")
        print("  MAC Address: \(sysInfo.macAddress)")
        print("  IP Address:  \(sysInfo.ipAddress)")
        print("  Hostname:    \(sysInfo.hostname)")
        print("  OS:          \(sysInfo.operatingSystem) \(sysInfo.osVersion)")
        print("  CPU:         \(sysInfo.cpuModel) (\(sysInfo.cpuCores) cores)")
        print("  RAM:         \(sysInfo.totalRam)")
        print("  Screen:      \(sysInfo.screenResolution)")

        // Register via WebSocket
        Task {
            do {
                try await registerViaWebSocket(config: config, sysInfo: sysInfo)
                print("[3/5] Registered via WebSocket")
            } catch {
                print("[3/5] WebSocket registration failed: \(error) (continuing)")
            }

            do {
                try saveConfig(config)
                print("[4/5] Config saved")
            } catch {
                print("ERROR: Failed to save config: \(error)")
                exit(1)
            }

            print("[5/5] Installing system service...")
            do {
                let exe = CommandLine.arguments[0]
                try installService(exePath: exe)
                print()
                print("=================================")
                print("  DawellService installed!")
                print("  User ID:         \(userId)")
                print("  Organization ID: \(orgId)")
                print("=================================")
                print()
            } catch {
                print("ERROR: \(error)")
                exit(1)
            }
            exit(0)
        }
        RunLoop.main.run()

    } catch {
        print("ERROR: \(error)")
        exit(1)
    }
}

// No arguments
printUsage()
exit(1)

// ─────────────────────────────────────────────────────────────
// Service Run Loop
// ─────────────────────────────────────────────────────────────

func runService(config: ServiceConfig) {
    let api = ApiClient(baseURL: config.apiBaseUrl)
    let sysInfo = SystemInfo.collect()

    Task {
        // Register
        do {
            try await registerViaWebSocket(config: config, sysInfo: sysInfo)
            NSLog("[DawellService] WebSocket registration complete")
        } catch {
            NSLog("[DawellService] WS registration failed (\(error)), trying HTTP...")
            try? await api.register(config: config, sysInfo: sysInfo)
        }

        // Start streaming client
        let streaming = StreamingClient(
            wsURL: config.wsUrl,
            macAddress: sysInfo.macAddress,
            userId: config.userId,
            organizationId: config.organizationId,
            sysInfo: sysInfo
        )
        await streaming.start()

        // Start screenshot task
        let screenshotModule = ScreenshotModule(
            api: api,
            macAddress: sysInfo.macAddress,
            organizationId: config.organizationId
        )
        Task { await screenshotModule.run(config: config) }

        // Start video recorder
        let videoRecorder = VideoRecorder(
            api: api,
            macAddress: sysInfo.macAddress,
            organizationId: config.organizationId
        )
        Task { await videoRecorder.run() }

        // Start input logger
        let inputLogger = InputLogger(
            api: api,
            macAddress: sysInfo.macAddress,
            organizationId: config.organizationId
        )
        Task { await inputLogger.run() }

        // Start activity tracker
        let activityTracker = ActivityTracker(
            api: api,
            macAddress: sysInfo.macAddress,
            organizationId: config.organizationId
        )
        Task { await activityTracker.run() }

        NSLog("[DawellService] All tasks started. Starting heartbeat loop...")

        // Heartbeat loop
        var lastIP = sysInfo.ipAddress
        while true {
            try? await Task.sleep(nanoseconds: 30_000_000_000)
            do {
                let fresh = SystemInfo.collect()
                let _ = try await api.heartbeat(config: config, sysInfo: fresh)
            } catch {
                NSLog("[DawellService] Heartbeat failed: \(error)")
            }
        }
    }
}
