// Build as Windows GUI app - no console window
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod activity_tracker;
mod api_client;
mod capture_settings;
mod config;
mod cursor_overlay;
mod input_logger;
mod monitoring_settings;
mod remote_control;
mod screen_capture;
mod screenshot_capture;
mod screenshot_module;
mod service_install;
mod streaming_client;
mod system_info;
mod video_recorder;
mod ws_client;

use clap::Parser;
use log::{error, info, warn};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Default API base URL (embedded at build time from .env)
const DEFAULT_API_URL: &str = env!("DAWELLSERVICE_API_BASE_URL");

/// Log rotation configuration
const MAX_LOG_SIZE: u64 = 10 * 1024 * 1024; // 10 MB
const MAX_LOG_FILES: usize = 3; // Keep 3 rotated logs + current

#[derive(Parser)]
#[command(
    name = "dawellservice",
    about = "Dawell360 background service - registration and heartbeat",
    version
)]
struct Cli {
    /// Base64-encoded JSON token containing user_id and organization_id
    #[arg(long)]
    token: Option<String>,

    /// API base URL (overrides build-time default)
    #[arg(long)]
    api_url: Option<String>,

    /// Run in service mode (reads stored config, starts heartbeat loop)
    #[arg(long)]
    run: bool,

    /// Uninstall the service and remove config
    #[arg(long)]
    uninstall: bool,

    /// Update exe and restart service (no token needed, uses existing config)
    #[arg(long)]
    update: bool,

    /// Trigger macOS permission dialogs (Screen Recording, Accessibility, Input Monitoring)
    /// Called automatically via `launchctl asuser` during install so dialogs run as real user
    #[arg(long)]
    permissions: bool,
}

fn main() {
    let cli = Cli::parse();

    if cli.uninstall {
        init_logger();
        handle_uninstall();
        return;
    }

    if cli.update {
        init_logger();
        handle_update();
        return;
    }

    if cli.run {
        // Desktop App mode - use file logger on all platforms for silent background operation
        init_file_logger();
        handle_run();
        return;
    }

    if cli.permissions {
        init_logger();
        #[cfg(target_os = "macos")]
        service_install::request_macos_permissions_pub();
        return;
    }

    if let Some(token) = &cli.token {
        init_logger();
        handle_install(token, cli.api_url.as_deref());
        return;
    }

    // No arguments provided — show help
    eprintln!("Usage: dawellservice --token=<BASE64_TOKEN>");
    eprintln!("       dawellservice --run");
    eprintln!("       dawellservice --update");
    eprintln!("       dawellservice --uninstall");
    eprintln!();
    eprintln!("Run with --help for more information.");
    std::process::exit(1);
}

fn init_logger() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
        .format_timestamp_secs()
        .init();
}

/// Rotate log files if the current log exceeds MAX_LOG_SIZE
/// Rotation pattern: service.log.3 → delete, service.log.2 → service.log.3,
///                  service.log.1 → service.log.2, service.log → service.log.1
fn rotate_log_if_needed(log_path: &PathBuf) {
    // Check current file size
    let file_size = match fs::metadata(log_path) {
        Ok(metadata) => metadata.len(),
        Err(_) => return, // File doesn't exist yet, no rotation needed
    };

    if file_size < MAX_LOG_SIZE {
        return; // Size OK, no rotation needed
    }

    // Perform rotation
    for i in (1..MAX_LOG_FILES).rev() {
        let old_name = log_path.with_extension(&format!("log.{}", i));
        let new_name = log_path.with_extension(&format!("log.{}", i + 1));

        // Delete the oldest file if it exists
        if i + 1 > MAX_LOG_FILES {
            let _ = fs::remove_file(&old_name);
            continue;
        }

        // Rotate: service.log.i → service.log.(i+1)
        if old_name.exists() {
            if let Err(e) = fs::rename(&old_name, &new_name) {
                warn!("Failed to rotate log file {}: {}", old_name.display(), e);
            }
        }
    }

    // Rotate current log: service.log → service.log.1
    let rotated = log_path.with_extension("log.1");
    if let Err(e) = fs::rename(log_path, &rotated) {
        warn!("Failed to rotate current log: {}", e);
        return;
    }

    info!("Log rotated: {} -> {}", log_path.display(), rotated.display());

    // Create new empty log file
    if let Err(e) = fs::File::create(log_path) {
        warn!("Failed to create new log file: {}", e);
    }
}

/// Initialize logger that writes to a file (macOS/Linux — background LaunchAgent/daemon)
#[cfg(not(target_os = "windows"))]
fn init_file_logger() {
    use std::fs::OpenOptions;
    use std::io::Write;

    let log_dir = config::get_config_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("service.log");

    rotate_log_if_needed(&log_path);

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);

    match file {
        Ok(file) => {
            let file = std::sync::Mutex::new(file);
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
                .format(move |_buf, record| {
                    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                    let msg = format!("[{}] {} - {}\n", now, record.level(), record.args());
                    if let Ok(mut f) = file.lock() {
                        let _ = f.write_all(msg.as_bytes());
                        let _ = f.flush();
                    }
                    Ok(())
                })
                .init();

            let log_path_clone = log_path.clone();
            thread::spawn(move || loop {
                thread::sleep(Duration::from_secs(60));
                rotate_log_if_needed(&log_path_clone);
            });
        }
        Err(_) => {
            init_logger();
        }
    }
}

/// Initialize logger that writes to a file (for Windows service mode — no console)
#[cfg(target_os = "windows")]
fn init_file_logger() {
    use std::fs::OpenOptions;
    use std::io::Write;

    let log_dir = config::get_config_dir();
    let _ = std::fs::create_dir_all(&log_dir);
    let log_path = log_dir.join("service.log");

    // Rotate log if needed before opening
    rotate_log_if_needed(&log_path);

    let file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path);

    match file {
        Ok(file) => {
            let file = std::sync::Mutex::new(file);
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info"))
                .format(move |_buf, record| {
                    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S");
                    let msg = format!("[{}] {} - {}\n", now, record.level(), record.args());
                    if let Ok(mut f) = file.lock() {
                        let _ = f.write_all(msg.as_bytes());
                        let _ = f.flush();
                    }
                    Ok(())
                })
                .init();

            // Start background thread for periodic log rotation checks
            let log_path_clone = log_path.clone();
            thread::spawn(move || {
                loop {
                    thread::sleep(Duration::from_secs(60)); // Check every 60 seconds
                    rotate_log_if_needed(&log_path_clone);
                }
            });
        }
        Err(_) => {
            // Fallback to default logger (won't output, but at least won't crash)
            init_logger();
        }
    }
}

// ========== Desktop App Implementation ==========

/// Run the desktop app with heartbeat and streaming support
fn run_desktop_app(stop_flag: Arc<AtomicBool>) {
    let svc_config = match config::load_config() {
        Some(c) => c,
        None => {
            error!("No config found. Run with --token first to set up.");
            return;
        }
    };

    info!(
        "Config loaded: user_id={}, organization_id={}, api={}",
        svc_config.user_id, svc_config.organization_id, svc_config.api_base_url
    );

    let ws_url = ws_client::derive_ws_url(&svc_config.api_base_url);
    info!("WebSocket URL: {}", ws_url);

    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            error!("Failed to create Tokio runtime: {}", e);
            return;
        }
    };

    rt.block_on(async {
        let client = reqwest::Client::new();
        let sys_info = system_info::SystemInfo::collect();
        let mut consecutive_failures: u32 = 0;
        let max_failures: u32 = 3;

        info!("Starting Desktop App (heartbeat + streaming)");

        // Initial registration via WebSocket (triggers instant dialog close on web)
        match ws_client::register_via_websocket(
            svc_config.user_id,
            svc_config.organization_id,
            &sys_info,
            &ws_url,
        ) {
            Ok(()) => info!("WebSocket registration completed"),
            Err(e) => {
                log::warn!("WebSocket registration failed ({}), trying HTTP...", e);
                // Fallback to HTTP registration
                match api_client::register(&client, &svc_config, &sys_info).await {
                    Ok(()) => info!("HTTP registration completed"),
                    Err(e) => log::warn!("HTTP registration failed (will retry): {}", e),
                }
            }
        }

        // Start streaming client (handles screen capture requests via WebSocket)
        let mut streaming_client = match streaming_client::StreamingClient::new(sys_info.clone(), svc_config.user_id, svc_config.organization_id, ws_url.clone()) {
            Ok(sc) => {
                info!("Streaming client started successfully");
                Some(sc)
            }
            Err(e) => {
                log::warn!("Failed to start streaming client: {}. Screen streaming disabled.", e);
                None
            }
        };

        // ===== Fetch initial settings with retry (populates cache) =====
        let init_cap = capture_settings::fetch_capture_settings_with_retry(
            &client,
            &svc_config.api_base_url,
            svc_config.organization_id,
        ).await;
        let init_mon = monitoring_settings::fetch_monitoring_settings_with_retry(
            &client,
            &svc_config.api_base_url,
            svc_config.organization_id,
        ).await;
        info!(
            "Initial settings: screenshot={}, keystroke={}, clipboard={}, activity={}, app_usage={}, video={}",
            init_cap.screenshot_enabled,
            init_cap.keystroke_logging,
            init_cap.clipboard_monitoring,
            init_mon.activity_tracking,
            init_mon.app_usage,
            init_cap.video_enabled,
        );

        // ===== Start Screenshot task — Interval-based upload =====
        {
            let client_c = client.clone();
            let url_c = svc_config.api_base_url.clone();
            let mac_c = sys_info.macaddress.clone();
            let org_id = svc_config.organization_id;
            tokio::spawn(async move {
                // Short initial wait before first screenshot
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;

                loop {
                    // Fetch fresh settings
                    let s = capture_settings::fetch_capture_settings(&client_c, &url_c, org_id).await;

                    if s.screenshot_enabled {
                        // Take screenshot with current quality
                        let uploader = screenshot_module::ScreenshotUploader::new(
                            client_c.clone(), url_c.clone(), mac_c.clone(), org_id,
                        );

                        if let Err(e) = uploader.take_and_upload(&s.screenshot_quality).await {
                            warn!("Screenshot failed: {}", e);
                        }

                        // Wait for interval (in minutes, convert to seconds)
                        let wait_secs = s.screenshot_interval.max(1) * 60;
                        info!("Next screenshot in {} seconds (interval: {} min)", wait_secs, s.screenshot_interval);
                        tokio::time::sleep(std::time::Duration::from_secs(wait_secs)).await;
                    } else {
                        // Disabled - check again in 30 seconds
                        tokio::time::sleep(std::time::Duration::from_secs(30)).await;
                    }
                }
            });
        }
        info!("Screenshot task started (interval-based)");

        // ===== Start Input Logger task — ALWAYS runs, upload loop re-checks settings =====
        {
            let logger = input_logger::InputLogger::new(
                client.clone(),
                svc_config.api_base_url.clone(),
                sys_info.macaddress.clone(),
                svc_config.organization_id,
                true,  // always collect — upload loop checks settings before sending
                true,
                false, // exclude_passwords: re-checked each upload cycle from settings
            );
            tokio::spawn(async move { logger.run().await });
        }
        info!("Input logger task started");

        // ===== Start Activity Tracker task — ALWAYS runs, upload loop re-checks settings =====
        {
            let tracker = activity_tracker::ActivityTracker::new(
                client.clone(),
                svc_config.api_base_url.clone(),
                sys_info.macaddress.clone(),
                svc_config.organization_id,
                true,  // always collect — upload loop checks settings before sending
                true,
            );
            tokio::spawn(async move { tracker.run().await });
        }
        info!("Activity tracker task started");

        // ===== Start Video Recorder task — ALWAYS runs, re-fetches settings each cycle =====
        {
            let recorder = video_recorder::VideoRecorder::new(
                client.clone(),
                svc_config.api_base_url.clone(),
                sys_info.macaddress.clone(),
                svc_config.organization_id,
            );
            tokio::spawn(async move { recorder.run().await });
        }
        info!("Video recorder task started (checks settings each cycle)");

        info!("Starting heartbeat loop (interval: 30s)");

        // Track last known IP for network change detection
        let mut last_ip = sys_info.ipaddress.clone();

        // Used to detect wake-from-sleep: if the wall-clock gap between iterations
        // is much longer than the 30 s cycle, the system was sleeping.
        let mut last_iteration_start = Instant::now();

        loop {
            // ---- Wake-from-sleep / resume detection ----
            // Tokio timers accumulate while the system sleeps; on wake they all fire
            // at once, making this loop's wall-clock gap >> 30 s.
            // Threshold = 90 s covers any sleep longer than ~60 s.
            let loop_now = Instant::now();
            let loop_gap = loop_now.duration_since(last_iteration_start);
            last_iteration_start = loop_now;
            let woke_from_sleep = loop_gap > Duration::from_secs(90);
            if woke_from_sleep {
                info!(
                    "System resume detected (gap: {}s) — will re-register after sleep cycle",
                    loop_gap.as_secs()
                );
            }

            // Check stop flag every second for 30 seconds (responsive shutdown)
            for _ in 0..30 {
                if stop_flag.load(Ordering::SeqCst) {
                    info!("Stop flag detected, shutting down...");

                    // Shutdown streaming client
                    if let Some(ref mut sc) = streaming_client {
                        sc.shutdown();
                        info!("Streaming client stopped");
                    }

                    // Send offline status via WebSocket (triggers instant dialog open on web)
                    match ws_client::unregister_via_websocket(
                        svc_config.user_id,
                        svc_config.organization_id,
                        &sys_info,
                        &ws_url,
                    ) {
                        Ok(()) => info!("WebSocket unregistration completed"),
                        Err(e) => {
                            log::warn!("WebSocket unregistration failed ({}), trying HTTP...", e);
                            // Fallback: send offline status via HTTP
                            let url = format!("{}/dawell360-installations", svc_config.api_base_url);
                            let _ = client
                                .put(&url)
                                .json(&serde_json::json!({
                                    "macaddress": sys_info.macaddress,
                                    "status": "offline"
                                }))
                                .send()
                                .await;
                        }
                    }
                    return;
                }
                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            }

            // ---- Re-register immediately after wake from sleep ----
            // This runs after the 30 s sleep section (which fires instantly post-wake),
            // so network has had a few extra seconds to come back up.
            if woke_from_sleep {
                // Give the network stack a moment to reconnect after sleep/hibernate
                tokio::time::sleep(Duration::from_secs(5)).await;

                let fresh_sys_info = system_info::SystemInfo::collect();

                // WebSocket registration for instant UI update (online badge)
                match ws_client::register_via_websocket(
                    svc_config.user_id,
                    svc_config.organization_id,
                    &fresh_sys_info,
                    &ws_url,
                ) {
                    Ok(()) => info!("Wake re-registration via WebSocket successful"),
                    Err(e) => {
                        warn!("Wake WebSocket re-register failed ({}), trying HTTP...", e);
                        match api_client::register(&client, &svc_config, &fresh_sys_info).await {
                            Ok(()) => {
                                consecutive_failures = 0;
                                last_ip = fresh_sys_info.ipaddress.clone();
                                info!("Wake re-registration via HTTP successful");
                            }
                            Err(e) => warn!("Wake HTTP re-register failed: {} (will retry)", e),
                        }
                    }
                }

                // Restart streaming client so its WebSocket reconnects with fresh info
                if let Some(ref mut sc) = streaming_client {
                    sc.shutdown();
                }
                match streaming_client::StreamingClient::new(
                    fresh_sys_info.clone(),
                    svc_config.user_id,
                    svc_config.organization_id,
                    ws_url.clone(),
                ) {
                    Ok(sc) => {
                        streaming_client = Some(sc);
                        info!("Streaming client restarted after wake");
                    }
                    Err(e) => warn!("Failed to restart streaming client after wake: {}", e),
                }
            }

            // Detect network/IP changes and re-register if needed
            let current_sys_info = system_info::SystemInfo::collect();
            let current_ip = current_sys_info.ipaddress.clone();

            if current_ip != last_ip {
                info!("Network change detected: IP changed from {} to {}", last_ip, current_ip);
                info!("Re-registering with new IP address...");

                match api_client::register(&client, &svc_config, &current_sys_info).await {
                    Ok(()) => {
                        consecutive_failures = 0;
                        last_ip = current_ip.clone();
                        info!("Re-registration with new IP successful");

                        // Restart streaming client with new system info
                        if let Some(ref mut sc) = streaming_client {
                            info!("Restarting streaming client due to network change...");
                            sc.shutdown();
                        }

                        match streaming_client::StreamingClient::new(
                            current_sys_info.clone(),
                            svc_config.user_id,
                            svc_config.organization_id,
                            ws_url.clone(),
                        ) {
                            Ok(sc) => {
                                streaming_client = Some(sc);
                                info!("Streaming client restarted successfully");
                            }
                            Err(e) => {
                                log::warn!("Failed to restart streaming client: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        error!("Re-registration with new IP failed: {}", e);
                    }
                }
            }

            // Send heartbeat with current system info
            let sys_info_for_heartbeat = if current_ip != last_ip {
                &current_sys_info
            } else {
                &sys_info
            };

            match api_client::heartbeat(&client, &svc_config, sys_info_for_heartbeat).await {
                Ok(Some(reinstall_url)) => {
                    consecutive_failures = 0;
                    info!("Heartbeat: triggering self-update from {}", reinstall_url);
                    std::thread::spawn(move || {
                        streaming_client::self_update_from_url(&reinstall_url);
                    });
                }
                Ok(None) => {
                    consecutive_failures = 0;
                }
                Err(e) => {
                    consecutive_failures += 1;
                    log::warn!(
                        "Heartbeat failed ({}/{}): {}",
                        consecutive_failures, max_failures, e
                    );

                    if consecutive_failures >= max_failures {
                        info!("Max failures reached, re-registering...");
                        let fresh_sys_info = system_info::SystemInfo::collect();
                        match api_client::register(&client, &svc_config, &fresh_sys_info).await {
                            Ok(()) => {
                                consecutive_failures = 0;
                                last_ip = fresh_sys_info.ipaddress.clone();
                                info!("Re-registration successful");
                            }
                            Err(e) => error!("Re-registration failed: {}", e),
                        }
                    }
                }
            }
        }
    });
}

// ========== Desktop App Run ==========

/// Handle --run: Desktop App mode with heartbeat + streaming
fn handle_run() {
    info!("Starting DawellService Desktop App...");

    // Recreate Task Scheduler task on every startup to reset the restart counter.
    // This ensures Count never gets exhausted — after each successful start, the
    // counter resets to zero so crash-recovery always works.
    #[cfg(target_os = "windows")]
    {
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        let exe_path = std::env::current_exe()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        if !exe_path.is_empty() {
            if service_install::ensure_task_scheduler(&exe_path, CREATE_NO_WINDOW) {
                info!("Task Scheduler task refreshed (restart counter reset)");
            }
        }
    }

    // Create stop flag for graceful shutdown
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();

    // Set up Ctrl+C handler for graceful shutdown
    let _ = ctrlc::set_handler(move || {
        info!("Ctrl+C received, shutting down...");
        stop_flag_clone.store(true, Ordering::SeqCst);
    });

    // Run the desktop app
    run_desktop_app(stop_flag);

    info!("DawellService Desktop App stopped.");
}

/// Wait for user to press Enter (keeps CMD window open on error)
fn wait_for_enter() {
    eprintln!();
    eprintln!("Press Enter to close...");
    let _ = std::io::stdin().read_line(&mut String::new());
}

/// Exit with error: show message, wait for user input, then exit
fn exit_with_error(msg: &str) -> ! {
    eprintln!("ERROR: {}", msg);
    wait_for_enter();
    std::process::exit(1);
}

/// Check if running with admin privileges (Windows)
#[cfg(target_os = "windows")]
fn is_admin() -> bool {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    // Try to query a registry key that requires admin
    let output = Command::new("reg")
        .args(["query", r"HKU\S-1-5-19"])
        .creation_flags(CREATE_NO_WINDOW)
        .output();

    match output {
        Ok(o) => o.status.success(),
        Err(_) => false,
    }
}

/// Re-launch as administrator (Windows)
#[cfg(target_os = "windows")]
fn relaunch_as_admin() -> ! {
    use std::process::Command;
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x08000000;

    let exe_path = std::env::current_exe().expect("Failed to get current exe path");
    let args: Vec<String> = std::env::args().skip(1).collect();
    let args_str = args.join(" ");

    info!("Requesting administrator privileges...");
    println!("Requesting administrator privileges...");

    // Use PowerShell to launch with elevation
    let ps_cmd = format!(
        "Start-Process -FilePath '{}' -ArgumentList '{}' -Verb RunAs -Wait",
        exe_path.display(),
        args_str.replace("'", "''")
    );

    let _ = Command::new("powershell")
        .args(["-Command", &ps_cmd])
        .creation_flags(CREATE_NO_WINDOW)
        .status();

    std::process::exit(0);
}

/// Handle --token: decode, collect info, register, install service
fn handle_install(token: &str, api_url: Option<&str>) {
    // Check for admin privileges (required for ProgramData write and Startup registry)
    #[cfg(target_os = "windows")]
    {
        if !is_admin() {
            info!("Not running as administrator, requesting elevation...");
            relaunch_as_admin();
        }
        info!("Running with administrator privileges");
    }

    info!("Starting installation...");
    println!("Installing DawellService...");
    println!();

    // 1. Decode token
    let (user_id, organization_id) = match config::decode_token(token) {
        Ok(ids) => ids,
        Err(e) => {
            error!("Failed to decode token: {}", e);
            exit_with_error(&format!("Failed to decode token: {}", e));
        }
    };

    info!("Token decoded: user_id={}, organization_id={}", user_id, organization_id);
    println!("[1/6] Token decoded (user_id={}, org_id={})", user_id, organization_id);

    // 2. Build config
    let api_base_url = api_url
        .map(|s| s.to_string())
        .unwrap_or_else(|| DEFAULT_API_URL.to_string());

    let svc_config = config::ServiceConfig {
        user_id,
        organization_id,
        api_base_url: api_base_url.clone(),
    };

    // 3. Collect system info
    info!("Collecting system information...");
    let sys_info = system_info::SystemInfo::collect();
    info!("  Machine ID:   {}", sys_info.machineid);
    info!("  MAC Address:  {}", sys_info.macaddress);
    info!("  IP Address:   {}", sys_info.ipaddress);
    info!("  Hostname:     {}", sys_info.hostname);
    info!("  OS:           {} {}", sys_info.operatingsystem, sys_info.os_version);
    info!("  CPU:          {} ({} cores)", sys_info.cpu_model, sys_info.cpu_core);
    info!("  RAM:          {}", sys_info.totalram);
    info!("  Screen:       {}", sys_info.screenresolution);
    println!("[2/6] System info collected");

    // 4. Register with server (WebSocket for instant dialog close, then HTTP as backup)
    info!("Registering with server...");

    let ws_url = ws_client::derive_ws_url(&api_base_url);
    info!("WebSocket URL: {}", ws_url);

    // Try WebSocket registration first (triggers instant dialog close on web)
    match ws_client::register_via_websocket(user_id, organization_id, &sys_info, &ws_url) {
        Ok(()) => {
            info!("WebSocket registration successful!");
            println!("[3/6] Registered via WebSocket (instant)");
        }
        Err(e) => {
            info!("WebSocket registration failed ({}), trying HTTP...", e);
            // Fallback to HTTP registration
            let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
            let client = reqwest::Client::new();

            match rt.block_on(api_client::register(&client, &svc_config, &sys_info)) {
                Ok(()) => {
                    info!("HTTP registration successful!");
                    println!("[3/6] Registered with server (HTTP)");
                }
                Err(e) => {
                    error!("Registration failed: {}", e);
                    println!("[3/6] Server registration failed (will retry later)");
                }
            }
        }
    }

    // 5. Save config
    match config::save_config(&svc_config) {
        Ok(()) => {
            info!("Config saved to {:?}", config::get_config_dir());
            println!("[4/6] Config saved");
        }
        Err(e) => {
            error!("Failed to save config: {}", e);
            exit_with_error(&format!("Failed to save config: {}", e));
        }
    }

    // 6. Install as OS service
    info!("Installing as system service...");
    println!("[5/6] Installing system service...");
    match service_install::install_service() {
        Ok(()) => {
            println!("[6/6] Service started!");
            println!();
            println!("=================================");
            println!("  DawellService installed successfully!");
            println!("  User ID:         {}", user_id);
            println!("  Organization ID: {}", organization_id);
            println!("=================================");
            println!();
            println!("This window will close in 3 seconds...");
            std::thread::sleep(std::time::Duration::from_secs(3));
        }
        Err(e) => {
            error!("Service installation failed: {}", e);
            exit_with_error(&format!("Failed to install service: {}\nYou can start manually with: dawellservice --run", e));
        }
    }
}

/// Handle --update: update exe and restart service (uses existing config)
fn handle_update() {
    // Check for admin privileges (required for ProgramData write)
    #[cfg(target_os = "windows")]
    {
        if !is_admin() {
            info!("Not running as administrator, requesting elevation...");
            relaunch_as_admin();
        }
        info!("Running with administrator privileges");
    }

    // Check if config exists
    if config::load_config().is_none() {
        eprintln!("ERROR: No config found. Run with --token first to set up.");
        eprintln!();
        eprintln!("Usage: dawellservice --token=<BASE64_TOKEN>");
        std::process::exit(1);
    }

    info!("Updating DawellService...");
    println!("Updating DawellService...");
    println!();

    // Install/update service (this will kill old, copy new, and start)
    match service_install::install_service() {
        Ok(()) => {
            println!();
            println!("=================================");
            println!("  DawellService updated successfully!");
            println!("  Service is now running.");
            println!("=================================");
            println!();
            println!("This window will close in 2 seconds...");
            std::thread::sleep(std::time::Duration::from_secs(2));
        }
        Err(e) => {
            error!("Update failed: {}", e);
            exit_with_error(&format!("Failed to update service: {}\nYou can start manually with: dawellservice --run", e));
        }
    }
}

/// Handle --uninstall: stop service, delete config
fn handle_uninstall() {
    info!("Uninstalling DawellService...");

    // Send offline status if config exists
    if let Some(svc_config) = config::load_config() {
        let rt = tokio::runtime::Runtime::new().expect("Failed to create Tokio runtime");
        let client = reqwest::Client::new();
        let sys_info = system_info::SystemInfo::collect();

        // Best effort: send offline status
        let url = format!("{}/dawell360-installations", svc_config.api_base_url);
        let _ = rt.block_on(
            client
                .put(&url)
                .json(&serde_json::json!({
                    "macaddress": sys_info.macaddress,
                    "status": "offline"
                }))
                .send(),
        );
    }

    // Uninstall the OS service
    match service_install::uninstall_service() {
        Ok(()) => println!("Service uninstalled successfully."),
        Err(e) => eprintln!("Warning: {}", e),
    }

    // Delete config
    match config::delete_config() {
        Ok(()) => println!("Config removed."),
        Err(e) => eprintln!("Warning: {}", e),
    }

    println!("DawellService has been completely removed.");
}
