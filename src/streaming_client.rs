use log::{error, info, warn};
use serde::{Deserialize, Serialize};
use std::net::TcpStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{connect, Message, WebSocket};

use crate::remote_control::{self, ClipboardSync, RemoteInputEvent};
use crate::screen_capture::{CaptureConfig, CapturedFrame, ScreenCapturer};
use crate::system_info::SystemInfo;
// ws_url is passed in from caller (derived via ws_client::derive_ws_url)

/// WebSocket message structure
#[derive(Debug, Serialize, Deserialize)]
struct WsMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
}

/// Start screen stream request payload
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StartStreamPayload {
    interval_ms: Option<u64>,
    quality: Option<u8>,
    max_width: Option<u32>,
    requested_by: Option<String>,
}

/// Screen frame payload
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScreenFramePayload {
    data: String,
    width: u32,
    height: u32,
    timestamp: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    requested_by: Option<String>,
}

/// Streaming client that maintains persistent WebSocket connection
pub struct StreamingClient {
    stop_flag: Arc<AtomicBool>,
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl StreamingClient {
    /// Create and start a new streaming client
    pub fn new(system_info: SystemInfo, user_id: u32, organization_id: u32, ws_url: String) -> Result<Self, String> {

        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = stop_flag.clone();

        let thread_handle = thread::spawn(move || {
            streaming_loop(ws_url, system_info, user_id, organization_id, stop_flag_clone);
        });

        Ok(Self {
            stop_flag,
            thread_handle: Some(thread_handle),
        })
    }

    /// Shutdown the streaming client
    pub fn shutdown(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for StreamingClient {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Parse "WIDTHxHEIGHT" string into (width, height) pixels
fn parse_screen_resolution(res: &str) -> (u32, u32) {
    let parts: Vec<&str> = res.split('x').collect();
    if parts.len() == 2 {
        let w = parts[0].trim().parse::<u32>().unwrap_or(1920);
        let h = parts[1].trim().parse::<u32>().unwrap_or(1080);
        return (w, h);
    }
    (1920, 1080)
}

/// Main streaming loop - maintains WebSocket connection and handles messages
fn streaming_loop(ws_url: String, system_info: SystemInfo, user_id: u32, organization_id: u32, stop_flag: Arc<AtomicBool>) {
    info!("Streaming client starting, connecting to: {}", ws_url);

    let (screen_width, screen_height) = parse_screen_resolution(&system_info.screenresolution);
    info!("Screen resolution: {}x{}", screen_width, screen_height);

    let mut reconnect_delay = Duration::from_secs(1);
    let max_reconnect_delay = Duration::from_secs(30);

    while !stop_flag.load(Ordering::SeqCst) {
        // Try to connect
        match connect_and_run(&ws_url, &system_info, user_id, organization_id, screen_width, screen_height, &stop_flag) {
            Ok(()) => {
                // Clean exit
                break;
            }
            Err(e) => {
                if stop_flag.load(Ordering::SeqCst) {
                    break;
                }
                warn!(
                    "WebSocket connection lost: {}. Reconnecting in {:?}...",
                    e, reconnect_delay
                );
                thread::sleep(reconnect_delay);

                // Exponential backoff
                reconnect_delay = std::cmp::min(reconnect_delay * 2, max_reconnect_delay);
            }
        }
    }

    info!("Streaming client stopped");
}

/// Sanitise a browser-supplied filename so it is safe to write to disk.
fn sanitize_filename(name: &str) -> String {
    let safe: String = name
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || matches!(c, '.' | '-' | '_' | ' ') {
                c
            } else {
                '_'
            }
        })
        .collect();
    let safe = safe.trim().to_string();
    if safe.is_empty() {
        "transferred_file".to_string()
    } else {
        safe
    }
}

/// Connect to WebSocket and handle messages
fn connect_and_run(
    ws_url: &str,
    system_info: &SystemInfo,
    user_id: u32,
    organization_id: u32,
    screen_width: u32,
    screen_height: u32,
    stop_flag: &Arc<AtomicBool>,
) -> Result<(), String> {
    let (mut socket, response) =
        connect(ws_url).map_err(|e| format!("Connection failed: {}", e))?;

    info!(
        "WebSocket connected for streaming, status: {}",
        response.status()
    );

    // Read welcome message
    match socket.read() {
        Ok(Message::Text(text)) => {
            if let Ok(msg) = serde_json::from_str::<WsMessage>(&text) {
                if msg.msg_type == "connected" {
                    info!("Received connected acknowledgment");
                }
            }
        }
        _ => {}
    }

    // Register as desktop client
    register_desktop(&mut socket, system_info, user_id, organization_id)?;

    // Set socket to non-blocking for message handling
    // Must handle both plain (ws://) and TLS (wss://) streams
    match socket.get_ref() {
        MaybeTlsStream::Plain(ref stream) => {
            let _ = stream.set_nonblocking(true);
        }
        MaybeTlsStream::NativeTls(ref tls_stream) => {
            // Get the underlying TcpStream from the TLS wrapper
            let _ = tls_stream.get_ref().set_nonblocking(true);
        }
        _ => {}
    }

    // Screen capturer (None initially)
    let capturer: Arc<Mutex<Option<ScreenCapturer>>> = Arc::new(Mutex::new(None));
    let socket_mutex = Arc::new(Mutex::new(socket));

    // Current requested_by for frames
    let requested_by: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));

    // Channel for clipboard sync from remote_control to WebSocket sender
    let (clipboard_tx, clipboard_rx) = mpsc::channel::<ClipboardSync>();

    // State for incoming file transfers: (filename, accumulated_bytes)
    let pending_file_transfer: Arc<Mutex<Option<(String, Vec<u8>)>>> = Arc::new(Mutex::new(None));

    loop {
        if stop_flag.load(Ordering::SeqCst) {
            // Clean shutdown
            let mut socket_guard = socket_mutex.lock().unwrap();
            let _ = socket_guard.close(None);
            return Ok(());
        }

        // Check for clipboard sync messages (non-blocking)
        while let Ok(clipboard_sync) = clipboard_rx.try_recv() {
            let req_by = requested_by.lock().unwrap().clone();
            let msg = WsMessage {
                msg_type: "clipboard_sync".to_string(),
                payload: Some(serde_json::json!({
                    "text": clipboard_sync.text,
                    "requestedBy": req_by,
                })),
                timestamp: Some(chrono::Utc::now().to_rfc3339()),
            };
            if let Ok(msg_text) = serde_json::to_string(&msg) {
                if let Ok(mut socket) = socket_mutex.lock() {
                    let _ = socket.send(Message::Text(msg_text));
                    info!("Sent clipboard_sync: {} chars", clipboard_sync.text.len());
                }
            }
        }

        // Check for incoming WebSocket messages
        let mut socket_guard = socket_mutex.lock().unwrap();
        match socket_guard.read() {
            Ok(Message::Text(text)) => {
                drop(socket_guard); // Release lock before processing
                handle_incoming_message(&text, &capturer, &socket_mutex, &requested_by, screen_width, screen_height, &clipboard_tx, &pending_file_transfer);
            }
            Ok(Message::Ping(data)) => {
                let _ = socket_guard.send(Message::Pong(data));
            }
            Ok(Message::Close(_)) => {
                return Err("Server closed connection".to_string());
            }
            Err(tungstenite::Error::Io(ref e))
                if e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                // No message available, continue
                drop(socket_guard);
                thread::sleep(Duration::from_millis(10));
            }
            Err(e) => {
                return Err(format!("WebSocket error: {}", e));
            }
            _ => {}
        }
    }
}

/// Register as desktop client
fn register_desktop(
    socket: &mut WebSocket<MaybeTlsStream<TcpStream>>,
    system_info: &SystemInfo,
    user_id: u32,
    organization_id: u32,
) -> Result<(), String> {
    let register_msg = WsMessage {
        msg_type: "register_desktop".to_string(),
        payload: Some(serde_json::json!({
            "machineId": system_info.machineid,
            "macAddress": system_info.macaddress,
            // All physical adapter MACs so the relay can match any stored address
            "macAddresses": system_info.mac_addresses,
            "ipAddress": system_info.ipaddress,
            "hostname": system_info.hostname,
            "operatingSystem": system_info.operatingsystem,
            "userId": user_id,
            "organizationId": organization_id,
        })),
        timestamp: Some(chrono::Utc::now().to_rfc3339()),
    };

    let msg_text =
        serde_json::to_string(&register_msg).map_err(|e| format!("Serialize error: {}", e))?;

    socket
        .send(Message::Text(msg_text))
        .map_err(|e| format!("Send error: {}", e))?;

    info!("Registered as desktop client for streaming");
    Ok(())
}

/// Handle incoming WebSocket message
fn handle_incoming_message(
    text: &str,
    capturer: &Arc<Mutex<Option<ScreenCapturer>>>,
    socket_mutex: &Arc<Mutex<WebSocket<MaybeTlsStream<TcpStream>>>>,
    requested_by: &Arc<Mutex<Option<String>>>,
    screen_width: u32,
    screen_height: u32,
    clipboard_tx: &mpsc::Sender<ClipboardSync>,
    pending_file_transfer: &Arc<Mutex<Option<(String, Vec<u8>)>>>,
) {
    let msg: WsMessage = match serde_json::from_str(text) {
        Ok(m) => m,
        Err(_) => return,
    };

    match msg.msg_type.as_str() {
        "start_screen_stream" => {
            info!("Received start_screen_stream request");

            let payload: StartStreamPayload = msg
                .payload
                .and_then(|p| serde_json::from_value(p).ok())
                .unwrap_or(StartStreamPayload {
                    interval_ms: None,
                    quality: None,
                    max_width: None,
                    requested_by: None,
                });

            let config = CaptureConfig {
                interval_ms: payload.interval_ms.unwrap_or(100),
                quality: payload.quality.unwrap_or(80),
                max_width: payload.max_width.unwrap_or(1920),
            };

            *requested_by.lock().unwrap() = payload.requested_by.clone();

            let socket_clone = socket_mutex.clone();
            let req_by_clone = requested_by.clone();

            let mut capturer_guard = capturer.lock().unwrap();

            // Stop existing capturer if any
            if let Some(mut c) = capturer_guard.take() {
                c.stop();
            }

            // Start new capturer
            let new_capturer = ScreenCapturer::start(config.clone(), move |frame| {
                send_frame(&socket_clone, &frame, &req_by_clone);
            });

            *capturer_guard = Some(new_capturer);

            // Send acknowledgment
            let mut socket_guard = socket_mutex.lock().unwrap();
            let ack_msg = WsMessage {
                msg_type: "screen_stream_started".to_string(),
                payload: Some(serde_json::json!({
                    "success": true,
                    "requestedBy": payload.requested_by,
                    "interval_ms": config.interval_ms,
                })),
                timestamp: Some(chrono::Utc::now().to_rfc3339()),
            };
            let _ = socket_guard.send(Message::Text(serde_json::to_string(&ack_msg).unwrap()));
        }

        "stop_screen_stream" => {
            info!("Received stop_screen_stream request");

            let mut capturer_guard = capturer.lock().unwrap();
            if let Some(mut c) = capturer_guard.take() {
                c.stop();
            }
            *requested_by.lock().unwrap() = None;

            // Hide the accessor name overlay on the employee's screen
            crate::cursor_overlay::hide_label();

            let mut socket_guard = socket_mutex.lock().unwrap();
            let ack_msg = WsMessage {
                msg_type: "screen_stream_stopped".to_string(),
                payload: Some(serde_json::json!({ "success": true })),
                timestamp: Some(chrono::Utc::now().to_rfc3339()),
            };
            let _ = socket_guard.send(Message::Text(serde_json::to_string(&ack_msg).unwrap()));
        }

        "request_screenshot" => {
            info!("Received screenshot request");

            let payload: StartStreamPayload = msg
                .payload
                .and_then(|p| serde_json::from_value(p).ok())
                .unwrap_or(StartStreamPayload {
                    interval_ms: None,
                    quality: None,
                    max_width: None,
                    requested_by: None,
                });

            let config = CaptureConfig {
                interval_ms: 1000,
                quality: payload.quality.unwrap_or(100),
                max_width: payload.max_width.unwrap_or(1920),
            };

            let socket_clone = socket_mutex.clone();
            let req_by = payload.requested_by;

            // Spawn thread for single capture
            thread::spawn(move || {
                capture_single_frame(&socket_clone, &config, req_by);
            });
        }

        "remote_input" => {
            // Browser viewer sent a mouse/keyboard event — simulate it locally
            if let Some(payload) = msg.payload {
                match serde_json::from_value::<RemoteInputEvent>(payload) {
                    Ok(input_event) => {
                        if input_event.event_type == "open_file_picker" {
                            // Open a native file-chooser on the remote desktop, send selected file back
                            handle_open_file_picker(socket_mutex);
                        } else if input_event.event_type == "get_clipboard" {
                            // Read remote clipboard — files (CF_HDROP) first on Windows, text on all
                            #[cfg(target_os = "windows")]
                            handle_get_clipboard_win(socket_mutex, clipboard_tx);
                            #[cfg(not(target_os = "windows"))]
                            handle_get_clipboard_unix(clipboard_tx);
                        } else {
                            remote_control::simulate_event_with_clipboard(
                                &input_event, screen_width, screen_height, Some(clipboard_tx),
                            );
                        }
                    }
                    Err(e) => {
                        warn!("Failed to parse remote_input payload: {}", e);
                    }
                }
            }
        }

        "file_transfer" => {
            if let Some(ref event) = msg.payload {
                let action = event.get("action").and_then(|v| v.as_str()).unwrap_or("");
                match action {
                    "start" => {
                        let filename = sanitize_filename(
                            event.get("fileName").and_then(|v| v.as_str()).unwrap_or("transferred_file"),
                        );
                        info!("File transfer start: {}", filename);
                        *pending_file_transfer.lock().unwrap() = Some((filename, Vec::new()));
                    }
                    "chunk" => {
                        if let Some(data_b64) = event.get("data").and_then(|v| v.as_str()) {
                            use base64::{engine::general_purpose, Engine as _};
                            match general_purpose::STANDARD.decode(data_b64) {
                                Ok(chunk_bytes) => {
                                    if let Some((_, ref mut buf)) = *pending_file_transfer.lock().unwrap() {
                                        buf.extend_from_slice(&chunk_bytes);
                                    }
                                }
                                Err(e) => warn!("file_transfer: base64 decode error: {}", e),
                            }
                        }
                    }
                    "end" => {
                        let file_data = pending_file_transfer.lock().unwrap().take();
                        if let Some((filename, data)) = file_data {
                            let downloads_dir = dirs::download_dir()
                                .or_else(dirs::desktop_dir)
                                .unwrap_or_else(|| std::path::PathBuf::from("."));
                            // Avoid overwriting an existing file
                            let mut dest = downloads_dir.join(&filename);
                            if dest.exists() {
                                let stem = std::path::Path::new(&filename)
                                    .file_stem().and_then(|s| s.to_str()).unwrap_or(&filename);
                                let ext = std::path::Path::new(&filename)
                                    .extension().and_then(|e| e.to_str());
                                let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S").to_string();
                                let new_name = match ext {
                                    Some(e) => format!("{}_{}.{}", stem, ts, e),
                                    None    => format!("{}_{}", stem, ts),
                                };
                                dest = downloads_dir.join(new_name);
                            }
                            match std::fs::write(&dest, &data) {
                                Ok(()) => info!("File saved: {:?} ({} bytes)", dest, data.len()),
                                Err(e) => warn!("file_transfer: failed to save {:?}: {}", dest, e),
                            }
                        }
                    }
                    _ => warn!("file_transfer: unknown action '{}'", action),
                }
            }
        }

        "reinstall_desktop" => {
            // Admin triggered a remote reinstall — download new binary and run --update
            if let Some(payload) = msg.payload {
                if let Some(url) = payload.get("downloadUrl").and_then(|v| v.as_str()) {
                    let url = url.to_string();
                    info!("Received reinstall_desktop, downloading from: {}", url);
                    thread::spawn(move || {
                        self_update_from_url(&url);
                    });
                }
            }
        }

        _ => {
            // Unknown message type, ignore
        }
    }
}

// ── Remote → Local file transfer helpers (Windows) ──────────────────────────

/// Send a file back to the browser as chunked base64 `file_transfer_back` messages.
/// Runs in a background thread so it never blocks the main streaming loop.
/// Retries each send on WouldBlock (non-blocking socket buffer full) so no chunks are lost.
fn send_file_back(
    socket_mutex: &Arc<Mutex<WebSocket<MaybeTlsStream<TcpStream>>>>,
    filename: &str,
    data: &[u8],
) {
    use base64::{engine::general_purpose, Engine as _};
    const CHUNK: usize = 128 * 1024; // 128 KB — smaller chunks reduce WouldBlock frequency

    let socket = socket_mutex.clone();
    let filename = filename.to_string();
    let data = data.to_vec();

    thread::spawn(move || {
        // Send one JSON message, retrying on WouldBlock until it succeeds.
        let send_reliable = |json: serde_json::Value| {
            let text = match serde_json::to_string(&json) {
                Ok(t) => t,
                Err(_) => return,
            };
            loop {
                let result = {
                    let mut s = match socket.lock() {
                        Ok(s) => s,
                        Err(_) => return,
                    };
                    s.send(Message::Text(text.clone()))
                };
                match result {
                    Ok(_) => break,
                    Err(tungstenite::Error::Io(ref e))
                        if e.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        // Send buffer full — back off and retry
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(e) => {
                        warn!("send_file_back: send error: {}", e);
                        return;
                    }
                }
            }
        };

        let total = data.len();
        send_reliable(serde_json::json!({
            "type": "file_transfer_back",
            "payload": { "action": "start", "fileName": filename, "fileSize": total },
            "timestamp": chrono::Utc::now().to_rfc3339()
        }));

        for chunk in data.chunks(CHUNK) {
            send_reliable(serde_json::json!({
                "type": "file_transfer_back",
                "payload": { "action": "chunk", "data": general_purpose::STANDARD.encode(chunk) },
                "timestamp": chrono::Utc::now().to_rfc3339()
            }));
        }

        send_reliable(serde_json::json!({
            "type": "file_transfer_back",
            "payload": { "action": "end", "fileName": filename },
            "timestamp": chrono::Utc::now().to_rfc3339()
        }));

        info!("Sent file '{}' ({} bytes) back to browser", filename, total);
    });
}

/// Open a native file-chooser dialog on the remote desktop.
/// The user selects a file which is then sent back to the browser as `file_transfer_back`.
/// Runs in a background thread so it never blocks the streaming loop.
fn handle_open_file_picker(socket_mutex: &Arc<Mutex<WebSocket<MaybeTlsStream<TcpStream>>>>) {
    let socket = socket_mutex.clone();
    thread::spawn(move || {
        #[cfg(target_os = "windows")]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x08000000;
            // PowerShell: show OpenFileDialog (WinForms), print selected path to stdout.
            // CREATE_NO_WINDOW suppresses the console flash; the dialog itself still appears.
            let ps = concat!(
                "Add-Type -AssemblyName System.Windows.Forms;",
                "$d = New-Object System.Windows.Forms.OpenFileDialog;",
                "$d.Title = 'Select file to download';",
                "$d.Multiselect = $false;",
                "if ($d.ShowDialog() -eq [System.Windows.Forms.DialogResult]::OK) { $d.FileName }",
            );
            match std::process::Command::new("powershell")
                .args(["-NoProfile", "-Command", ps])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
            {
                Ok(out) => {
                    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if path.is_empty() {
                        info!("open_file_picker: user cancelled");
                        return;
                    }
                    match std::fs::read(&path) {
                        Ok(data) => {
                            let name = std::path::Path::new(&path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("file")
                                .to_string();
                            info!("open_file_picker: sending '{}' ({} bytes)", name, data.len());
                            send_file_back(&socket, &name, &data);
                        }
                        Err(e) => warn!("open_file_picker: read '{}' failed: {}", path, e),
                    }
                }
                Err(e) => warn!("open_file_picker: powershell error: {}", e),
            }
        }
        #[cfg(target_os = "macos")]
        {
            match std::process::Command::new("osascript")
                .args(["-e", "POSIX path of (choose file with prompt \"Select file to download\")"])
                .output()
            {
                Ok(out) if out.status.success() => {
                    let path = String::from_utf8_lossy(&out.stdout).trim().to_string();
                    if path.is_empty() { return; }
                    match std::fs::read(&path) {
                        Ok(data) => {
                            let name = std::path::Path::new(&path)
                                .file_name()
                                .and_then(|n| n.to_str())
                                .unwrap_or("file")
                                .to_string();
                            send_file_back(&socket, &name, &data);
                        }
                        Err(e) => warn!("open_file_picker: read '{}' failed: {}", path, e),
                    }
                }
                _ => info!("open_file_picker: osascript cancelled or unavailable"),
            }
        }
        #[cfg(target_os = "linux")]
        {
            // Try zenity, fall back to kdialog
            let out = std::process::Command::new("zenity")
                .args(["--file-selection", "--title=Select file to download"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .or_else(|| {
                    std::process::Command::new("kdialog")
                        .args(["--getopenfilename", "~"])
                        .output()
                        .ok()
                        .filter(|o| o.status.success())
                });
            if let Some(o) = out {
                let path = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if path.is_empty() { return; }
                match std::fs::read(&path) {
                    Ok(data) => {
                        let name = std::path::Path::new(&path)
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or("file")
                            .to_string();
                        send_file_back(&socket, &name, &data);
                    }
                    Err(e) => warn!("open_file_picker: read '{}' failed: {}", path, e),
                }
            } else {
                info!("open_file_picker: no file dialog tool available (install zenity or kdialog)");
            }
        }
    });
}

/// Handle `get_clipboard` on Windows: check CF_HDROP (files) first, then CF_UNICODETEXT (text).
#[cfg(target_os = "windows")]
fn handle_get_clipboard_win(
    socket_mutex: &Arc<Mutex<WebSocket<MaybeTlsStream<TcpStream>>>>,
    clipboard_tx: &mpsc::Sender<ClipboardSync>,
) {
    use windows::Win32::Foundation::{HGLOBAL, HWND};
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};

    const CF_UNICODETEXT: u32 = 13;
    const CF_HDROP: u32 = 15;

    std::thread::sleep(std::time::Duration::from_millis(150));

    // ── Check for files (CF_HDROP) ────────────────────────────────────────
    let files = unsafe {
        let mut result: Vec<(String, Vec<u8>)> = Vec::new();
        if IsClipboardFormatAvailable(CF_HDROP).is_ok() {
            if OpenClipboard(HWND(0)).is_ok() {
                if let Ok(handle) = GetClipboardData(CF_HDROP) {
                    let ptr = GlobalLock(HGLOBAL(handle.0 as *mut std::ffi::c_void));
                    if !ptr.is_null() {
                        // DROPFILES layout: pFiles(u32,4) + pt.x(4) + pt.y(4) + fNC(4) + fWide(4)
                        let base = ptr as *const u8;
                        let p_files = std::ptr::read_unaligned(base as *const u32) as usize;
                        let f_wide  = std::ptr::read_unaligned(base.add(16) as *const u32) != 0;
                        let names_base = base.add(p_files);

                        let paths: Vec<String> = if f_wide {
                            let mut cur = names_base as *const u16;
                            let mut v = Vec::new();
                            loop {
                                let mut len = 0usize;
                                while *cur.add(len) != 0 { len += 1; if len > 32_768 { break; } }
                                if len == 0 { break; }
                                v.push(String::from_utf16_lossy(
                                    std::slice::from_raw_parts(cur, len)).to_owned());
                                cur = cur.add(len + 1);
                            }
                            v
                        } else {
                            let mut cur = names_base;
                            let mut v = Vec::new();
                            loop {
                                let mut len = 0usize;
                                while *cur.add(len) != 0 { len += 1; if len > 32_768 { break; } }
                                if len == 0 { break; }
                                v.push(String::from_utf8_lossy(
                                    std::slice::from_raw_parts(cur, len)).into_owned());
                                cur = cur.add(len + 1);
                            }
                            v
                        };

                        let _ = GlobalUnlock(HGLOBAL(handle.0 as *mut std::ffi::c_void));
                        for path in &paths {
                            match std::fs::read(path) {
                                Ok(data) => {
                                    let name = std::path::Path::new(path)
                                        .file_name()
                                        .and_then(|n| n.to_str())
                                        .unwrap_or("file")
                                        .to_string();
                                    result.push((name, data));
                                }
                                Err(e) => warn!("get_clipboard: read '{}' failed: {}", path, e),
                            }
                        }
                    }
                }
                let _ = CloseClipboard();
            }
        }
        result
    };

    if !files.is_empty() {
        for (name, data) in &files {
            send_file_back(socket_mutex, name, data);
        }
        return;
    }

    // ── Fall back to text (CF_UNICODETEXT) ────────────────────────────────
    let text = unsafe {
        let mut out = String::new();
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_ok() {
            if OpenClipboard(HWND(0)).is_ok() {
                if let Ok(handle) = GetClipboardData(CF_UNICODETEXT) {
                    let ptr = GlobalLock(HGLOBAL(handle.0 as *mut std::ffi::c_void));
                    if !ptr.is_null() {
                        let wide = ptr as *const u16;
                        let mut len = 0usize;
                        while *wide.add(len) != 0 { len += 1; if len > 1_000_000 { break; } }
                        out = String::from_utf16_lossy(std::slice::from_raw_parts(wide, len))
                            .to_owned();
                        let _ = GlobalUnlock(HGLOBAL(handle.0 as *mut std::ffi::c_void));
                    }
                }
                let _ = CloseClipboard();
            }
        }
        out
    };
    if !text.is_empty() {
        let _ = clipboard_tx.send(ClipboardSync { text });
    }
}

/// Handle `get_clipboard` on macOS/Linux: read text clipboard via arboard.
#[cfg(not(target_os = "windows"))]
fn handle_get_clipboard_unix(clipboard_tx: &mpsc::Sender<ClipboardSync>) {
    std::thread::sleep(std::time::Duration::from_millis(150));
    match arboard::Clipboard::new() {
        Ok(mut clipboard) => {
            if let Ok(text) = clipboard.get_text() {
                if !text.is_empty() {
                    let _ = clipboard_tx.send(ClipboardSync { text });
                }
            }
        }
        Err(e) => warn!("get_clipboard: failed to access clipboard: {:?}", e),
    }
}

/// Send a captured frame via WebSocket.
/// If frame.status is Some("screen_unavailable"), sends a status message instead of a frame.
fn send_frame(
    socket_mutex: &Arc<Mutex<WebSocket<MaybeTlsStream<TcpStream>>>>,
    frame: &CapturedFrame,
    requested_by: &Arc<Mutex<Option<String>>>,
) {
    let req_by = requested_by.lock().unwrap().clone();

    // Screen unavailable (sleep / lock / frozen) — send status, not a frame
    if let Some(ref status) = frame.status {
        let msg = WsMessage {
            msg_type: "screen_status".to_string(),
            payload: Some(serde_json::json!({
                "status": status,
                "timestamp": frame.timestamp,
                "requestedBy": req_by,
            })),
            timestamp: None,
        };
        if let Ok(msg_text) = serde_json::to_string(&msg) {
            if let Ok(mut socket) = socket_mutex.lock() {
                let _ = socket.send(Message::Text(msg_text));
            }
        }
        return;
    }

    let payload = ScreenFramePayload {
        data: frame.data.clone(),
        width: frame.width,
        height: frame.height,
        timestamp: frame.timestamp,
        requested_by: req_by,
    };

    let msg = WsMessage {
        msg_type: "screen_frame".to_string(),
        payload: Some(serde_json::to_value(&payload).unwrap()),
        timestamp: None,
    };

    if let Ok(msg_text) = serde_json::to_string(&msg) {
        if let Ok(mut socket) = socket_mutex.lock() {
            if let Err(e) = socket.send(Message::Text(msg_text)) {
                // WouldBlock (os 10035) = send buffer full — receiver is slow.
                // Silently drop this frame; the next one will be sent when buffer clears.
                if let tungstenite::Error::Io(ref io_err) = e {
                    if io_err.kind() == std::io::ErrorKind::WouldBlock {
                        return;
                    }
                }
                warn!("Failed to send frame: {}", e);
            }
        }
    }
}

/// Capture a single frame (for screenshot requests)
fn capture_single_frame(
    socket_mutex: &Arc<Mutex<WebSocket<MaybeTlsStream<TcpStream>>>>,
    config: &CaptureConfig,
    requested_by: Option<String>,
) {
    match crate::screen_capture::take_screenshot(config) {
        Ok(captured) => {
            let payload = ScreenFramePayload {
                data: captured.data,
                width: captured.width,
                height: captured.height,
                timestamp: captured.timestamp,
                requested_by,
            };

            let msg = WsMessage {
                msg_type: "screen_frame".to_string(),
                payload: Some(serde_json::to_value(&payload).unwrap()),
                timestamp: None,
            };

            if let Ok(msg_text) = serde_json::to_string(&msg) {
                if let Ok(mut socket) = socket_mutex.lock() {
                    let _ = socket.send(Message::Text(msg_text));
                }
            }
        }
        Err(e) => {
            error!("Screenshot capture failed: {}", e);

            let msg = WsMessage {
                msg_type: "screen_capture_error".to_string(),
                payload: Some(serde_json::json!({
                    "error": e,
                    "requestedBy": requested_by,
                })),
                timestamp: Some(chrono::Utc::now().to_rfc3339()),
            };

            if let Ok(msg_text) = serde_json::to_string(&msg) {
                if let Ok(mut socket) = socket_mutex.lock() {
                    let _ = socket.send(Message::Text(msg_text));
                }
            }
        }
    }
}

/// Write/update reinstall_log.txt in the config directory.
/// File is human-readable so the admin can open it directly on the employee PC.
fn write_reinstall_log() {
    let log_path = crate::config::get_config_dir().join("reinstall_log.txt");
    let now = chrono::Local::now().format("%Y-%m-%d %H:%M:%S").to_string();

    // Read existing count + history lines
    let (count, mut history): (u32, Vec<String>) = if log_path.exists() {
        let content = std::fs::read_to_string(&log_path).unwrap_or_default();
        let mut cnt = 0u32;
        let mut hist: Vec<String> = Vec::new();
        for line in content.lines() {
            if let Some(val) = line.strip_prefix("Reinstall Count:") {
                cnt = val.trim().parse().unwrap_or(0);
            } else if line.len() > 3 {
                // History lines look like "1. 2026-03-10 14:30:00"
                if let Some(ts) = line.splitn(2, ". ").nth(1) {
                    hist.push(ts.trim().to_string());
                }
            }
        }
        (cnt, hist)
    } else {
        (0, Vec::new())
    };

    let new_count = count + 1;
    history.push(now.clone());

    let mut content = format!(
        "Reinstall Count: {}\nLast Reinstall : {}\n\nHistory:\n",
        new_count, now
    );
    for (i, ts) in history.iter().enumerate() {
        content.push_str(&format!("{}. {}\n", i + 1, ts));
    }

    let _ = std::fs::create_dir_all(crate::config::get_config_dir());
    if let Err(e) = std::fs::write(&log_path, &content) {
        warn!("reinstall_log: failed to write: {}", e);
    } else {
        info!("reinstall_log: count={} written to {:?}", new_count, log_path);
    }
}

/// Download a new binary from `download_url`, save to temp, and run `--update`.
/// The spawned process handles killing the old service and restarting with the new binary.
pub fn self_update_from_url(download_url: &str) {
    // Record this reinstall event in a human-readable log file on the employee PC
    write_reinstall_log();

    #[cfg(target_os = "windows")]
    let temp_path = std::env::temp_dir().join("dawellservice_update.exe");
    #[cfg(not(target_os = "windows"))]
    let temp_path = std::env::temp_dir().join("dawellservice_update");

    info!("self_update: downloading from {}", download_url);

    let bytes = match reqwest::blocking::get(download_url).and_then(|r| r.bytes()) {
        Ok(b) => b,
        Err(e) => {
            warn!("self_update: download failed: {}", e);
            return;
        }
    };

    if let Err(e) = std::fs::write(&temp_path, &bytes) {
        warn!("self_update: write failed: {}", e);
        return;
    }

    info!("self_update: {} bytes written to {:?}", bytes.len(), temp_path);

    #[cfg(target_os = "windows")]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;
        match std::process::Command::new(&temp_path)
            .arg("--update")
            .creation_flags(CREATE_NO_WINDOW)
            .spawn()
        {
            Ok(_) => info!("self_update: update process launched"),
            Err(e) => warn!("self_update: launch failed: {}", e),
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        #[allow(unused_imports)]
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&temp_path, std::fs::Permissions::from_mode(0o755));
        match std::process::Command::new(&temp_path).arg("--update").spawn() {
            Ok(_) => info!("self_update: update process launched"),
            Err(e) => warn!("self_update: launch failed: {}", e),
        }
    }
}
