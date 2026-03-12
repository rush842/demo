import Foundation
import CoreGraphics
import ApplicationServices

// MARK: - Real User Detection (handles sudo case)

private struct RealUser {
    let name: String
    let home: String
    let uid: uid_t
}

private func realUser() -> RealUser {
    // When run via `sudo`, SUDO_USER = original username, HOME = /var/root
    // We must use the original user's home + uid for LaunchAgent install
    let sudoUser = ProcessInfo.processInfo.environment["SUDO_USER"] ?? ""
    if !sudoUser.isEmpty {
        let home = "/Users/\(sudoUser)"
        var uid: uid_t = getuid()
        if let pw = getpwnam(sudoUser) {
            uid = pw.pointee.pw_uid
        }
        return RealUser(name: sudoUser, home: home, uid: uid)
    }
    // Not sudo — use current user
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    return RealUser(name: NSUserName(), home: home, uid: getuid())
}

// MARK: - Install (works with or without sudo)

func installService(exePath: String) throws {
    let user = realUser()
    let installDir = "\(user.home)/Library/Application Support/DawellService"
    try FileManager.default.createDirectory(atPath: installDir, withIntermediateDirectories: true)

    let installPath = "\(installDir)/dawellservice"

    // Kill existing processes
    let currentPid = ProcessInfo.processInfo.processIdentifier
    shell("pgrep -f dawellservice | grep -v \(currentPid) | xargs kill 2>/dev/null")

    // Unload existing LaunchAgent from user's GUI session
    let plistPath = "\(user.home)/Library/LaunchAgents/com.dawell.agent.plist"
    shell("launchctl bootout gui/\(user.uid) '\(plistPath)' 2>/dev/null")
    Thread.sleep(forTimeInterval: 1)

    // Copy binary to user's directory
    if FileManager.default.fileExists(atPath: installPath) {
        try FileManager.default.removeItem(atPath: installPath)
    }
    try FileManager.default.copyItem(atPath: exePath, toPath: installPath)
    shell("chmod +x '\(installPath)'")
    shell("xattr -cr '\(installPath)'")
    shell("codesign --force --deep --sign - '\(installPath)' 2>/dev/null")

    // Write LaunchAgent plist with environment variables embedded
    let plistContent = """
    <?xml version="1.0" encoding="UTF-8"?>
    <!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
    <plist version="1.0">
    <dict>
        <key>Label</key>
        <string>com.dawell.agent</string>
        <key>ProgramArguments</key>
        <array>
            <string>\(installPath)</string>
            <string>--run</string>
        </array>
        <key>EnvironmentVariables</key>
        <dict>
            <key>DAWELLSERVICE_API_BASE_URL</key>
            <string>\(kApiBaseURL)</string>
            <key>DAWELLSERVICE_WS_URL</key>
            <string>\(kWsURL)</string>
            <key>HOME</key>
            <string>\(user.home)</string>
        </dict>
        <key>RunAtLoad</key>
        <true/>
        <key>KeepAlive</key>
        <true/>
        <key>StandardErrorPath</key>
        <string>\(user.home)/Library/Application Support/DawellService/service.log</string>
        <key>StandardOutPath</key>
        <string>\(user.home)/Library/Application Support/DawellService/service.log</string>
    </dict>
    </plist>
    """
    let plistDir = "\(user.home)/Library/LaunchAgents"
    try FileManager.default.createDirectory(atPath: plistDir, withIntermediateDirectories: true)
    try plistContent.write(toFile: plistPath, atomically: true, encoding: .utf8)

    // Request permissions as real user
    let sudoUser = ProcessInfo.processInfo.environment["SUDO_USER"] ?? ""
    if !sudoUser.isEmpty {
        // Running via sudo — spawn permissions in user's GUI session
        print("  Requesting permissions as user '\(user.name)' (uid=\(user.uid))...")
        shell("launchctl asuser \(user.uid) '\(installPath)' --permissions")
        Thread.sleep(forTimeInterval: 18)
    } else {
        // Running as real user — request inline
        requestMacOSPermissions(binaryPath: installPath)
    }

    // Bootstrap LaunchAgent in user's GUI session
    let result = shell("launchctl bootstrap gui/\(user.uid) '\(plistPath)'")
    if result != 0 {
        let r2 = shell("launchctl kickstart -k gui/\(user.uid)/com.dawell.agent 2>/dev/null")
        if r2 != 0 {
            print("  Note: Service will start on next login (bootstrap deferred)")
        }
    }

    print("[DawellService] macOS service installed successfully")
}

func uninstallService() {
    let user = realUser()
    let plistPath = "\(user.home)/Library/LaunchAgents/com.dawell.agent.plist"
    shell("launchctl bootout gui/\(user.uid) '\(plistPath)' 2>/dev/null")
    try? FileManager.default.removeItem(atPath: plistPath)
    try? FileManager.default.removeItem(atPath: "\(user.home)/Library/Application Support/DawellService/dawellservice")
}

// MARK: - Permission Requests

func requestMacOSPermissions(binaryPath: String) {
    print()
    print("============================================================")
    print("  macOS Permissions — Please click Allow in each dialog")
    print("============================================================")
    print()

    print("[1/3] Requesting Screen Recording...")
    CGRequestScreenCaptureAccess()
    Thread.sleep(forTimeInterval: 3)

    print("[2/3] Requesting Accessibility...")
    let opts = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
    AXIsProcessTrustedWithOptions(opts)
    Thread.sleep(forTimeInterval: 3)

    print("[3/3] Input Monitoring — opening System Settings...")
    print("  → Add '\(binaryPath)' and toggle ON")
    shell("open 'x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent'")
    Thread.sleep(forTimeInterval: 6)

    print()
    print("  After granting all permissions, the service will start automatically.")
    print("============================================================")
    print()
}

// MARK: - Helpers

func getPlistPath() -> String {
    let user = realUser()
    return "\(user.home)/Library/LaunchAgents/com.dawell.agent.plist"
}

@discardableResult
func shell(_ command: String) -> Int32 {
    let proc = Process()
    proc.launchPath = "/bin/bash"
    proc.arguments = ["-c", command]
    proc.launch()
    proc.waitUntilExit()
    return proc.terminationStatus
}
