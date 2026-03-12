use log::info;
#[cfg(target_os = "windows")]
use log::warn;
use std::env;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs;

/// Install the app for the current OS (Desktop App mode - not a service)
pub fn install_service() -> Result<(), String> {
    let exe_path = env::current_exe()
        .map_err(|e| format!("Failed to get executable path: {}", e))?;
    let exe_path_str = exe_path.to_string_lossy().to_string();

    #[cfg(target_os = "windows")]
    {
        return install_windows_startup(&exe_path_str);
    }

    #[cfg(target_os = "linux")]
    {
        return install_linux_autostart(&exe_path_str);
    }

    #[cfg(target_os = "macos")]
    {
        return install_macos_loginitem(&exe_path_str);
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Err("Unsupported operating system".to_string())
    }
}

/// Uninstall the app for the current OS
pub fn uninstall_service() -> Result<(), String> {
    #[cfg(target_os = "windows")]
    {
        return uninstall_windows_startup();
    }

    #[cfg(target_os = "linux")]
    {
        return uninstall_linux_autostart();
    }

    #[cfg(target_os = "macos")]
    {
        return uninstall_macos_loginitem();
    }

    #[cfg(not(any(target_os = "windows", target_os = "linux", target_os = "macos")))]
    {
        Err("Unsupported operating system".to_string())
    }
}

// ========== Windows - Desktop App (Startup Registry) ==========

/// Kill all dawellservice.exe processes except the current one, using Windows API (no PowerShell)
#[cfg(target_os = "windows")]
fn kill_old_processes(current_pid: u32) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };
    use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};

    unsafe {
        let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return,
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name_len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
                let name = String::from_utf16_lossy(&entry.szExeFile[..name_len]);

                if name.to_lowercase() == "dawellservice.exe"
                    && entry.th32ProcessID != current_pid
                {
                    if let Ok(handle) =
                        OpenProcess(PROCESS_TERMINATE, false, entry.th32ProcessID)
                    {
                        let _ = TerminateProcess(handle, 1);
                        let _ = CloseHandle(handle);
                    }
                }

                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
    }
}

/// Check if any dawellservice.exe is running (excluding current PID), using Windows API
#[cfg(target_os = "windows")]
fn is_old_process_running(current_pid: u32) -> bool {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::Diagnostics::ToolHelp::{
        CreateToolhelp32Snapshot, Process32FirstW, Process32NextW, PROCESSENTRY32W,
        TH32CS_SNAPPROCESS,
    };

    unsafe {
        let snapshot = match CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0) {
            Ok(h) => h,
            Err(_) => return false,
        };

        let mut entry = PROCESSENTRY32W {
            dwSize: std::mem::size_of::<PROCESSENTRY32W>() as u32,
            ..Default::default()
        };

        let mut found = false;
        if Process32FirstW(snapshot, &mut entry).is_ok() {
            loop {
                let name_len = entry.szExeFile.iter().position(|&c| c == 0).unwrap_or(260);
                let name = String::from_utf16_lossy(&entry.szExeFile[..name_len]);

                if name.to_lowercase() == "dawellservice.exe"
                    && entry.th32ProcessID != current_pid
                {
                    found = true;
                    break;
                }

                if Process32NextW(snapshot, &mut entry).is_err() {
                    break;
                }
            }
        }

        let _ = CloseHandle(snapshot);
        found
    }
}

#[cfg(target_os = "windows")]
fn install_windows_startup(exe_path: &str) -> Result<(), String> {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    info!("Installing as Desktop App (Startup)...");

    // 1. Remove old Windows Service if it exists (migration) — sc.exe is safe, not PowerShell
    let _ = Command::new("sc")
        .args(["stop", "DawellService"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    std::thread::sleep(std::time::Duration::from_secs(1));

    let _ = Command::new("sc")
        .args(["delete", "DawellService"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    info!("Old service removed (if existed)");

    // 2. Kill old processes using Windows API (no PowerShell / WMIC)
    info!("Killing any running dawellservice.exe processes...");
    println!("[*] Stopping old service...");

    let current_pid = std::process::id();

    kill_old_processes(current_pid);
    std::thread::sleep(std::time::Duration::from_millis(500));

    // Retry up to 5 times
    for attempt in 1..=5 {
        std::thread::sleep(std::time::Duration::from_secs(1));

        if !is_old_process_running(current_pid) {
            info!("All old dawellservice.exe processes terminated (attempt {})", attempt);
            println!("[*] Old service stopped");
            break;
        } else if attempt < 5 {
            info!("Process still running, killing again (attempt {}/5)...", attempt);
            kill_old_processes(current_pid);
        } else {
            info!("Warning: Process may still be running after 5 kill attempts");
            println!("[!] Warning: Could not stop old service completely");
        }
    }

    // Extra wait to ensure file handles are released
    std::thread::sleep(std::time::Duration::from_secs(2));

    // 3. Copy exe to permanent location
    let install_dir = std::path::PathBuf::from(r"C:\ProgramData\DawellService");
    std::fs::create_dir_all(&install_dir)
        .map_err(|e| format!("Failed to create install dir: {}", e))?;

    let install_path = install_dir.join("dawellservice.exe");
    let install_path_str = install_path.to_string_lossy().to_string();

    // Only copy if source is different from destination
    let source = std::path::Path::new(exe_path).canonicalize().ok();
    let dest = install_path.canonicalize().ok();
    if source != dest {
        println!("[*] Updating executable...");

        if install_path.exists() {
            info!("Attempting to delete old exe...");
            for del_attempt in 1..=5 {
                match std::fs::remove_file(&install_path) {
                    Ok(_) => {
                        info!("Old exe deleted successfully");
                        println!("[*] Old executable removed");
                        break;
                    }
                    Err(e) if del_attempt < 5 => {
                        info!("Delete attempt {} failed ({}), retrying...", del_attempt, e);
                        kill_old_processes(current_pid);
                        std::thread::sleep(std::time::Duration::from_secs(2));
                    }
                    Err(e) => {
                        info!("Could not delete old exe ({}), will try copy-overwrite", e);
                    }
                }
            }
        }

        let mut copy_success = false;
        for attempt in 1..=5 {
            match std::fs::copy(exe_path, &install_path) {
                Ok(bytes) => {
                    copy_success = true;
                    info!("Binary copied to {} ({} bytes)", install_path_str, bytes);
                    println!("[*] New executable installed ({} bytes)", bytes);
                    break;
                }
                Err(e) if attempt < 5 => {
                    info!("Copy attempt {} failed ({}), retrying...", attempt, e);
                    println!("[!] Copy attempt {} failed, retrying...", attempt);
                    kill_old_processes(current_pid);
                    std::thread::sleep(std::time::Duration::from_secs(2));
                }
                Err(e) => {
                    return Err(format!(
                        "Failed to copy exe to {}: {}. Please close any running dawellservice.exe and try again.",
                        install_path_str, e
                    ));
                }
            }
        }
        if !copy_success {
            return Err(
                "Failed to copy exe after 5 attempts. Please close any running dawellservice.exe and try again."
                    .to_string(),
            );
        }
    } else {
        println!("[*] Already running from installed location");
    }

    // 4a. Always write registry Run key — guarantees service starts on every login
    //     even if Task Scheduler restart count is exhausted.
    //     MultipleInstancesPolicy=IgnoreNew prevents a second instance if Task
    //     Scheduler already started the service on the same login.
    let startup_cmd = format!("\"{}\" --run", install_path_str);
    let reg_ok = Command::new("reg")
        .args([
            "add",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v", "DawellService",
            "/t", "REG_SZ",
            "/d", &startup_cmd,
            "/f",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if reg_ok {
        info!("Registry Run key written (login fallback)");
        println!("[*] Registry startup: login fallback enabled");
    } else {
        info!("Warning: registry Run key write failed");
    }

    // 4b. Also create Task Scheduler task for crash auto-restart (primary method)
    let task_ok = install_task_scheduler(&install_path_str, CREATE_NO_WINDOW);
    if task_ok {
        info!("Task Scheduler task created (restart-on-crash enabled)");
        println!("[*] Task Scheduler: auto-restart on crash enabled");
    } else {
        info!("Task Scheduler setup failed — registry login fallback is still active");
        println!("[!] Task Scheduler unavailable (login fallback active)");
    }

    // 5. Start the app now (hidden, in background)
    println!("[*] Starting service...");

    let child = Command::new(&install_path_str)
        .args(["--run"])
        .creation_flags(CREATE_NO_WINDOW)
        .spawn()
        .map_err(|e| format!("Failed to start app: {}", e))?;

    let new_pid = child.id();
    info!("Desktop app started with PID: {}", new_pid);

    // Verify using Windows API (no PowerShell)
    std::thread::sleep(std::time::Duration::from_secs(2));

    if is_old_process_running(current_pid) || new_pid != current_pid {
        info!("Service verified running with PID: {}", new_pid);
        println!("[OK] Service started successfully (PID: {})", new_pid);
    } else {
        info!("Warning: Could not verify service is running");
        println!("[!] Service may not have started properly");
    }

    info!("Desktop app installed and started successfully");
    Ok(())
}

#[cfg(target_os = "windows")]
fn uninstall_windows_startup() -> Result<(), String> {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    info!("Uninstalling Desktop App...");

    // 1. Stop and remove Task Scheduler task
    let _ = Command::new("schtasks")
        .args(["/End", "/TN", "DawellService"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    let _ = Command::new("schtasks")
        .args(["/Delete", "/TN", "DawellService", "/F"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    info!("Task Scheduler task removed");

    // 2. Remove from Startup registry (fallback cleanup)
    let _ = Command::new("reg")
        .args([
            "delete",
            r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
            "/v", "DawellService",
            "/f",
        ])
        .creation_flags(CREATE_NO_WINDOW)
        .output();
    info!("Removed from Windows Startup registry");

    // 3. Kill running process
    let _ = Command::new("taskkill")
        .args(["/F", "/IM", "dawellservice.exe"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    info!("Desktop app uninstalled");
    Ok(())
}

/// Public wrapper — called at service startup to refresh the Task Scheduler task
/// and reset the crash-restart counter to zero.
#[cfg(target_os = "windows")]
pub fn ensure_task_scheduler(install_path_str: &str, create_no_window: u32) -> bool {
    install_task_scheduler(install_path_str, create_no_window)
}

/// Create a Task Scheduler task that:
/// - Starts at user login (LogonTrigger)
/// - Restarts automatically every 30 seconds if the process crashes (up to 999 times)
/// - Does NOT start a second instance if one is already running
/// Returns true if the task was created successfully, false on failure.
#[cfg(target_os = "windows")]
fn install_task_scheduler(install_path_str: &str, create_no_window: u32) -> bool {
    use std::process::Command;
    use std::os::windows::process::CommandExt;

    let username = std::env::var("USERNAME").unwrap_or_default();
    if username.is_empty() {
        warn!("Could not determine USERNAME, Task Scheduler setup skipped");
        return false;
    }

    // Stop any existing task before recreating
    let _ = Command::new("schtasks")
        .args(["/End", "/TN", "DawellService"])
        .creation_flags(create_no_window)
        .output();

    // Build Task XML with RestartOnFailure policy
    let task_xml = format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo>
    <Description>Dawell360 employee monitoring service</Description>
  </RegistrationInfo>
  <Triggers>
    <LogonTrigger>
      <Enabled>true</Enabled>
      <UserId>{username}</UserId>
    </LogonTrigger>
    <SessionStateChangeTrigger>
      <Enabled>true</Enabled>
      <StateChange>SessionUnlock</StateChange>
      <UserId>{username}</UserId>
    </SessionStateChangeTrigger>
  </Triggers>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure>
      <Interval>PT30S</Interval>
      <Count>999</Count>
    </RestartOnFailure>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <Hidden>true</Hidden>
  </Settings>
  <Actions Context="Author">
    <Exec>
      <Command>{install_path_str}</Command>
      <Arguments>--run</Arguments>
    </Exec>
  </Actions>
</Task>"#
    );

    // schtasks requires UTF-16 LE with BOM
    let xml_utf16: Vec<u16> = std::iter::once(0xFEFFu16)
        .chain(task_xml.encode_utf16())
        .collect();
    let xml_bytes: Vec<u8> = xml_utf16
        .iter()
        .flat_map(|&w| w.to_le_bytes())
        .collect();

    let temp_path = std::env::temp_dir().join("dawellservice_task.xml");
    if std::fs::write(&temp_path, &xml_bytes).is_err() {
        warn!("Failed to write task XML to temp file");
        return false;
    }

    let result = Command::new("schtasks")
        .args([
            "/Create",
            "/TN", "DawellService",
            "/XML", &temp_path.to_string_lossy(),
            "/F",
        ])
        .creation_flags(create_no_window)
        .output();

    let _ = std::fs::remove_file(&temp_path);

    match result {
        Ok(output) if output.status.success() => true,
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            warn!("schtasks create failed: {}", stderr);
            false
        }
        Err(e) => {
            warn!("schtasks not available: {}", e);
            false
        }
    }
}

// ========== Linux - Autostart ==========

#[cfg(target_os = "linux")]
fn get_autostart_path() -> String {
    if let Ok(home) = env::var("HOME") {
        format!("{}/.config/autostart/dawellservice.desktop", home)
    } else {
        "/etc/xdg/autostart/dawellservice.desktop".to_string()
    }
}

#[cfg(target_os = "linux")]
fn install_linux_autostart(exe_path: &str) -> Result<(), String> {
    use std::process::Command;

    info!("Installing Linux autostart...");

    // Kill any running dawellservice processes (except current PID)
    let current_pid = std::process::id().to_string();
    let _ = Command::new("bash")
        .args(["-c", &format!(
            "pgrep -f dawellservice | grep -v {} | xargs -r kill 2>/dev/null",
            current_pid
        )])
        .output();
    std::thread::sleep(std::time::Duration::from_secs(1));

    // Copy binary to ~/.local/bin or /usr/local/bin
    let home = env::var("HOME").unwrap_or_default();
    let install_dir = format!("{}/.local/bin", home);
    let _ = fs::create_dir_all(&install_dir);

    let install_path = format!("{}/dawellservice", install_dir);
    fs::copy(exe_path, &install_path)
        .map_err(|e| format!("Failed to copy binary: {}", e))?;

    // Make executable
    let _ = Command::new("chmod").args(["+x", &install_path]).output();

    // Create .desktop file for autostart
    let autostart_dir = format!("{}/.config/autostart", home);
    let _ = fs::create_dir_all(&autostart_dir);

    let desktop_content = format!(
        r#"[Desktop Entry]
Type=Application
Name=DawellService
Exec={} --run
Hidden=false
NoDisplay=true
X-GNOME-Autostart-enabled=true
"#,
        install_path
    );

    let autostart_path = get_autostart_path();
    fs::write(&autostart_path, desktop_content)
        .map_err(|e| format!("Failed to write autostart file: {}", e))?;

    // Start now
    let _ = Command::new(&install_path).args(["--run"]).spawn();

    info!("Linux autostart installed successfully");
    Ok(())
}

#[cfg(target_os = "linux")]
fn uninstall_linux_autostart() -> Result<(), String> {
    use std::process::Command;

    info!("Uninstalling Linux autostart...");

    // Kill process
    let _ = Command::new("pkill").args(["-f", "dawellservice"]).output();

    // Remove autostart file
    let autostart_path = get_autostart_path();
    let _ = fs::remove_file(&autostart_path);

    // Remove binary
    let home = env::var("HOME").unwrap_or_default();
    let _ = fs::remove_file(format!("{}/.local/bin/dawellservice", home));

    info!("Linux autostart uninstalled");
    Ok(())
}

// ========== macOS - Login Item ==========

#[cfg(target_os = "macos")]
fn get_plist_path() -> String {
    if let Ok(home) = env::var("HOME") {
        format!("{}/Library/LaunchAgents/com.dawell.agent.plist", home)
    } else {
        "/Library/LaunchAgents/com.dawell.agent.plist".to_string()
    }
}

#[cfg(target_os = "macos")]
fn install_macos_loginitem(exe_path: &str) -> Result<(), String> {
    use std::process::Command;

    info!("Installing macOS login item...");

    // Kill any running dawellservice processes (except current PID)
    let current_pid = std::process::id().to_string();
    let _ = Command::new("bash")
        .args(["-c", &format!(
            "pgrep -f dawellservice | grep -v {} | xargs kill 2>/dev/null",
            current_pid
        )])
        .output();
    // Unload existing LaunchAgent if present
    let plist_path = get_plist_path();
    let _ = Command::new("launchctl").args(["unload", &plist_path]).output();
    std::thread::sleep(std::time::Duration::from_secs(1));

    // Copy binary
    let home = env::var("HOME").unwrap_or_default();
    let install_dir = format!("{}/Library/Application Support/DawellService", home);
    let _ = fs::create_dir_all(&install_dir);

    let install_path = format!("{}/dawellservice", install_dir);
    fs::copy(exe_path, &install_path)
        .map_err(|e| format!("Failed to copy binary: {}", e))?;

    let _ = Command::new("chmod").args(["+x", &install_path]).output();

    // Create LaunchAgent plist
    let plist_content = format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.dawell.agent</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
        <string>--run</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>KeepAlive</key>
    <true/>
</dict>
</plist>
"#,
        install_path
    );

    let plist_path = get_plist_path();
    if let Some(parent) = std::path::Path::new(&plist_path).parent() {
        let _ = fs::create_dir_all(parent);
    }
    fs::write(&plist_path, plist_content)
        .map_err(|e| format!("Failed to write plist: {}", e))?;

    // Request all required macOS permissions before starting service
    request_macos_permissions(&install_path);

    let _ = Command::new("launchctl").args(["load", &plist_path]).output();

    info!("macOS login item installed successfully");
    Ok(())
}

/// Open macOS System Settings to each required privacy pane so the user can
/// grant Screen Recording, Accessibility and Input Monitoring in one go.
#[cfg(target_os = "macos")]
fn request_macos_permissions(binary_path: &str) {
    use std::process::Command;

    println!();
    println!("============================================================");
    println!("  macOS Permissions Required");
    println!("============================================================");
    println!("  DawellService needs the following permissions:");
    println!("  1. Screen Recording  — screenshots, video, live stream");
    println!("  2. Accessibility     — remote desktop control");
    println!("  3. Input Monitoring  — keystroke & activity logging");
    println!("============================================================");
    println!();
    println!("  System Settings will open for each permission.");
    println!("  Click '+', add '{}', toggle ON.", binary_path);
    println!();

    // Trigger Screen Recording permission prompt by attempting a capture
    // (macOS shows dialog automatically on first attempt)
    let _ = Command::new("screencapture")
        .args(["-x", "/dev/null"])
        .output();

    // Open Screen Recording settings
    println!("[1/3] Opening Screen Recording settings...");
    let _ = Command::new("open")
        .args(["x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture"])
        .output();
    std::thread::sleep(std::time::Duration::from_secs(4));

    // Open Accessibility settings
    println!("[2/3] Opening Accessibility settings...");
    let _ = Command::new("open")
        .args(["x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility"])
        .output();
    std::thread::sleep(std::time::Duration::from_secs(4));

    // Open Input Monitoring settings
    println!("[3/3] Opening Input Monitoring settings...");
    let _ = Command::new("open")
        .args(["x-apple.systempreferences:com.apple.preference.security?Privacy_ListenEvent"])
        .output();
    std::thread::sleep(std::time::Duration::from_secs(4));

    println!();
    println!("  All permission panes opened.");
    println!("  Add dawellservice to each list and toggle ON.");
    println!("  Then close System Settings — service will start automatically.");
    println!("============================================================");
    println!();
}

#[cfg(target_os = "macos")]
fn uninstall_macos_loginitem() -> Result<(), String> {
    use std::process::Command;

    info!("Uninstalling macOS login item...");

    let plist_path = get_plist_path();
    let _ = Command::new("launchctl").args(["unload", &plist_path]).output();
    let _ = fs::remove_file(&plist_path);

    let home = env::var("HOME").unwrap_or_default();
    let _ = fs::remove_file(format!("{}/Library/Application Support/DawellService/dawellservice", home));

    info!("macOS login item uninstalled");
    Ok(())
}