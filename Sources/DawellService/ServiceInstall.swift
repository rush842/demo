import Foundation
import CoreGraphics
import ApplicationServices

// MARK: - Install (no sudo required — installs in user space)

func installService(exePath: String) throws {
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    let installDir = "\(home)/Library/Application Support/DawellService"
    try FileManager.default.createDirectory(atPath: installDir, withIntermediateDirectories: true)

    let installPath = "\(installDir)/dawellservice"

    // Kill existing processes
    let currentPid = ProcessInfo.processInfo.processIdentifier
    shell("pgrep -f dawellservice | grep -v \(currentPid) | xargs kill 2>/dev/null")

    // Unload existing LaunchAgent
    let plistPath = getPlistPath()
    shell("launchctl bootout gui/\(getuid()) '\(plistPath)' 2>/dev/null")
    Thread.sleep(forTimeInterval: 1)

    // Copy binary
    if FileManager.default.fileExists(atPath: installPath) {
        try FileManager.default.removeItem(atPath: installPath)
    }
    try FileManager.default.copyItem(atPath: exePath, toPath: installPath)
    shell("chmod +x '\(installPath)'")
    shell("xattr -cr '\(installPath)'")
    shell("codesign --force --deep --sign - '\(installPath)' 2>/dev/null")

    // Write LaunchAgent plist
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
        <key>RunAtLoad</key>
        <true/>
        <key>KeepAlive</key>
        <true/>
        <key>StandardErrorPath</key>
        <string>\(home)/Library/Application Support/DawellService/service.log</string>
        <key>StandardOutPath</key>
        <string>\(home)/Library/Application Support/DawellService/service.log</string>
    </dict>
    </plist>
    """
    if let parent = URL(fileURLWithPath: plistPath).deletingLastPathComponent().path as String? {
        try FileManager.default.createDirectory(atPath: parent, withIntermediateDirectories: true)
    }
    try plistContent.write(toFile: plistPath, atomically: true, encoding: .utf8)

    // Request permissions (in current user session — no sudo needed)
    requestMacOSPermissions(binaryPath: installPath)

    // Bootstrap LaunchAgent in user session
    let uid = getuid()
    let result = shell("launchctl bootstrap gui/\(uid) '\(plistPath)'")
    if result != 0 {
        // Already loaded or error 36 — try kickstart
        shell("launchctl kickstart -k gui/\(uid)/com.dawell.agent 2>/dev/null")
    }

    print("[DawellService] macOS service installed successfully")
}

func uninstallService() {
    let plistPath = getPlistPath()
    let uid = getuid()
    shell("launchctl bootout gui/\(uid) '\(plistPath)' 2>/dev/null")
    try? FileManager.default.removeItem(atPath: plistPath)
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    try? FileManager.default.removeItem(atPath: "\(home)/Library/Application Support/DawellService/dawellservice")
}

// MARK: - Permission Requests

func requestMacOSPermissions(binaryPath: String) {
    print()
    print("============================================================")
    print("  macOS Permissions — Please click Allow in each dialog")
    print("============================================================")
    print()

    // 1. Screen Recording — CGRequestScreenCaptureAccess is the correct API
    print("[1/3] Requesting Screen Recording...")
    CGRequestScreenCaptureAccess()
    Thread.sleep(forTimeInterval: 3)

    // 2. Accessibility — AXIsProcessTrustedWithOptions with prompt=true
    print("[2/3] Requesting Accessibility...")
    let opts = [kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String: true] as CFDictionary
    AXIsProcessTrustedWithOptions(opts)
    Thread.sleep(forTimeInterval: 3)

    // 3. Input Monitoring — open System Settings pane
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
    let home = FileManager.default.homeDirectoryForCurrentUser.path
    return "\(home)/Library/LaunchAgents/com.dawell.agent.plist"
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
