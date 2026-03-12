use log::{info, warn};
use reqwest::Client;
use serde::Serialize;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

#[derive(Debug, Clone, Serialize)]
pub struct KeystrokeEntry {
    pub timestamp: u64,
    pub key: String,
    pub window_title: String,
    pub application: String,
    pub is_password_field: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct ClipboardEntry {
    pub timestamp: u64,
    pub content_type: String,
    pub content: String,
    pub content_length: usize,
    pub window_title: String,
    pub application: String,
}

pub struct InputLogger {
    client: Client,
    api_base_url: String,
    macaddress: String,
    organization_id: u32,
    keystroke_logging: bool,
    clipboard_monitoring: bool,
    exclude_passwords: bool,
}

impl InputLogger {
    pub fn new(
        client: Client,
        api_base_url: String,
        macaddress: String,
        organization_id: u32,
        keystroke_logging: bool,
        clipboard_monitoring: bool,
        exclude_passwords: bool,
    ) -> Self {
        Self {
            client,
            api_base_url,
            macaddress,
            organization_id,
            keystroke_logging,
            clipboard_monitoring,
            exclude_passwords,
        }
    }

    pub async fn run(self) {
        let keystrokes: Arc<Mutex<Vec<KeystrokeEntry>>> = Arc::new(Mutex::new(Vec::new()));
        let clipboard_logs: Arc<Mutex<Vec<ClipboardEntry>>> = Arc::new(Mutex::new(Vec::new()));

        let ks_clone = keystrokes.clone();
        let cl_clone = clipboard_logs.clone();
        let keystroke_logging = self.keystroke_logging;
        let clipboard_monitoring = self.clipboard_monitoring;
        let exclude_passwords = self.exclude_passwords;

        // Background polling thread (runs independently of tokio)
        std::thread::spawn(move || {
            #[cfg(target_os = "windows")]
            {
                let mut key_states = [false; 256];
                let mut last_clipboard_hash: u64 = 0;
                let mut clipboard_tick: u32 = 0;

                loop {
                    let now_ms = now_millis();
                    let (window_title, app_name) = get_foreground_info();

                    // --- Keystroke polling ---
                    if keystroke_logging {
                        for vk in 0u16..=254 {
                            let pressed = is_key_down(vk);
                            if pressed && !key_states[vk as usize] {
                                key_states[vk as usize] = true;
                                if let Some(key_str) = vk_to_str(vk) {
                                    // Skip password fields if exclude_passwords is true
                                    // (We detect common password-related apps/titles heuristically)
                                    let is_pw = exclude_passwords
                                        && (window_title.to_lowercase().contains("password")
                                            || window_title.to_lowercase().contains("sign in")
                                            || window_title.to_lowercase().contains("login"));
                                    if !is_pw {
                                        if let Ok(mut ks) = ks_clone.lock() {
                                            ks.push(KeystrokeEntry {
                                                timestamp: now_ms,
                                                key: key_str,
                                                window_title: window_title.clone(),
                                                application: app_name.clone(),
                                                is_password_field: false,
                                            });
                                        }
                                    }
                                }
                            } else if !pressed {
                                key_states[vk as usize] = false;
                            }
                        }
                    }

                    // --- Clipboard polling (every 2 seconds = 20 * 100ms) ---
                    clipboard_tick += 1;
                    if clipboard_monitoring && clipboard_tick >= 20 {
                        clipboard_tick = 0;
                        if let Some((text, hash)) = read_clipboard_text() {
                            if hash != last_clipboard_hash && !text.is_empty() {
                                last_clipboard_hash = hash;
                                let content_length = text.len();
                                let content: String = text.chars().take(2000).collect();
                                if let Ok(mut cl) = cl_clone.lock() {
                                    cl.push(ClipboardEntry {
                                        timestamp: now_ms,
                                        content_type: "text".to_string(),
                                        content,
                                        content_length,
                                        window_title: window_title.clone(),
                                        application: app_name.clone(),
                                    });
                                }
                            }
                        }
                    }

                    std::thread::sleep(Duration::from_millis(100));
                }
            }

            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                use rdev::{listen, Button, Event, EventType};
                use std::sync::{Arc, Mutex};

                // Shared window state — polled every second, used by the rdev listener closure
                let window_state: Arc<Mutex<(String, String)>> = Arc::new(Mutex::new((
                    "Unknown".to_string(),
                    "Unknown".to_string(),
                )));

                // Window polling thread (osascript is slow ~50ms, so poll separately)
                let ws_poll = window_state.clone();
                std::thread::spawn(move || loop {
                    let info = get_foreground_info();
                    if let Ok(mut w) = ws_poll.lock() {
                        *w = info;
                    }
                    std::thread::sleep(Duration::from_secs(1));
                });

                // Clipboard polling thread (pbpaste every 2 seconds)
                let cl_mac = cl_clone.clone();
                std::thread::spawn(move || {
                    let mut last_hash: u64 = 0;
                    loop {
                        std::thread::sleep(Duration::from_secs(2));
                        if !clipboard_monitoring {
                            continue;
                        }
                        if let Some((text, hash)) = read_clipboard_text() {
                            if hash != last_hash && !text.is_empty() {
                                last_hash = hash;
                                let now_ms = now_millis();
                                let (window_title, app_name) = get_foreground_info();
                                let content_length = text.len();
                                let content: String = text.chars().take(2000).collect();
                                if let Ok(mut cl) = cl_mac.lock() {
                                    cl.push(ClipboardEntry {
                                        timestamp: now_ms,
                                        content_type: "text".to_string(),
                                        content,
                                        content_length,
                                        window_title,
                                        application: app_name,
                                    });
                                }
                            }
                        }
                    }
                });

                // rdev listener: handles BOTH keyboard events AND mouse state updates.
                // listen() blocks this thread via CFRunLoopRun — must be on a dedicated thread.
                // Requires: Accessibility permission (System Prefs → Privacy → Input Monitoring).
                let ks_mac = ks_clone.clone();
                let ws_listen = window_state.clone();
                let _ = listen(move |event: Event| {
                    match event.event_type {
                        EventType::MouseMove { x, y } => {
                            unix_state::set_mouse(x as i32, y as i32);
                        }
                        EventType::ButtonPress(Button::Left) => {
                            unix_state::set_left_down(true);
                        }
                        EventType::ButtonRelease(Button::Left) => {
                            unix_state::set_left_down(false);
                        }
                        EventType::KeyPress(key) => {
                            if !keystroke_logging {
                                return;
                            }
                            let now_ms = now_millis();
                            let (window_title, app_name) = ws_listen
                                .lock()
                                .map(|w| w.clone())
                                .unwrap_or_else(|_| {
                                    ("Unknown".to_string(), "Unknown".to_string())
                                });
                            if let Some(key_str) = rdev_key_to_str(key) {
                                let is_pw = exclude_passwords
                                    && (window_title.to_lowercase().contains("password")
                                        || window_title.to_lowercase().contains("sign in")
                                        || window_title.to_lowercase().contains("login"));
                                if !is_pw {
                                    if let Ok(mut ks) = ks_mac.lock() {
                                        ks.push(KeystrokeEntry {
                                            timestamp: now_ms,
                                            key: key_str,
                                            window_title,
                                            application: app_name,
                                            is_password_field: false,
                                        });
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                });
                // listen() returned (error or stopped) — keep thread alive
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }

            #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
            {
                // Other platforms: nothing to do
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
        });

        info!("Input logger started (keystroke={}, clipboard={})", keystroke_logging, clipboard_monitoring);

        // Upload loop — every 60 seconds
        loop {
            sleep(Duration::from_secs(60)).await;

            // Re-fetch settings each cycle — picks up admin enable/disable without restart
            let cap_settings = crate::capture_settings::fetch_capture_settings(
                &self.client, &self.api_base_url, self.organization_id,
            ).await;

            // Drain collected data
            let ks_batch = {
                if let Ok(mut ks) = keystrokes.lock() {
                    std::mem::take(&mut *ks)
                } else {
                    Vec::new()
                }
            };

            let cl_batch = {
                if let Ok(mut cl) = clipboard_logs.lock() {
                    std::mem::take(&mut *cl)
                } else {
                    Vec::new()
                }
            };

            // Skip upload if both features are disabled
            if !cap_settings.keystroke_logging && !cap_settings.clipboard_monitoring {
                continue;
            }

            // Only send data for enabled features
            let ks_to_send = if cap_settings.keystroke_logging { ks_batch } else { vec![] };
            let cl_to_send = if cap_settings.clipboard_monitoring { cl_batch } else { vec![] };

            if ks_to_send.is_empty() && cl_to_send.is_empty() {
                continue;
            }

            let url = format!("{}/input-logs/upload", self.api_base_url);
            let body = json!({
                "macaddress": self.macaddress,
                "organization_id": self.organization_id,
                "keystrokes": ks_to_send,
                "clipboard": cl_to_send,
            });

            let upload_ok = match tokio::time::timeout(
                Duration::from_secs(30),
                self.client.post(&url).json(&body).send(),
            )
            .await
            {
                Ok(Ok(resp)) if resp.status().is_success() => {
                    info!(
                        "Input logs uploaded: {} keystrokes, {} clipboard",
                        ks_to_send.len(),
                        cl_to_send.len()
                    );
                    true
                }
                Ok(Ok(resp)) => {
                    warn!("Input logs upload failed: HTTP {} (will retry next cycle)", resp.status());
                    false
                }
                Ok(Err(e)) => {
                    warn!("Input logs upload failed: {} (will retry next cycle)", e);
                    false
                }
                Err(_) => {
                    warn!("Input logs upload timed out (will retry next cycle)");
                    false
                }
            };

            // On failure, put data back to front of queue so next cycle retries
            if !upload_ok {
                if let Ok(mut ks) = keystrokes.lock() {
                    let mut recovered = ks_to_send;
                    recovered.extend(ks.drain(..));
                    *ks = recovered;
                }
                if let Ok(mut cl) = clipboard_logs.lock() {
                    let mut recovered = cl_to_send;
                    recovered.extend(cl.drain(..));
                    *cl = recovered;
                }
            }
        }
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

// ===== Windows-specific helpers =====

#[cfg(target_os = "windows")]
fn is_key_down(vk: u16) -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 }
}

#[cfg(target_os = "windows")]
pub fn get_foreground_info() -> (String, String) {
    use windows::Win32::Foundation::CloseHandle;
    use windows::Win32::System::ProcessStatus::GetModuleFileNameExW;
    use windows::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_VM_READ,
    };
    use windows::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, GetWindowTextW, GetWindowThreadProcessId,
    };

    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.0 == 0 {
            return ("Unknown".to_string(), "Unknown".to_string());
        }

        // Window title
        let mut title_buf = vec![0u16; 512];
        let title_len = GetWindowTextW(hwnd, &mut title_buf);
        let window_title = if title_len > 0 {
            String::from_utf16_lossy(&title_buf[..title_len as usize])
        } else {
            "Unknown".to_string()
        };

        // Process name
        let mut pid = 0u32;
        GetWindowThreadProcessId(hwnd, Some(&mut pid));

        let app_name = if pid > 0 {
            match OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid) {
                Ok(handle) => {
                    let mut exe_buf = vec![0u16; 512];
                    let exe_len = GetModuleFileNameExW(
                        handle,
                        None,
                        &mut exe_buf,
                    );
                    let _ = CloseHandle(handle);
                    if exe_len > 0 {
                        let path =
                            String::from_utf16_lossy(&exe_buf[..exe_len as usize]);
                        std::path::Path::new(&path)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or("Unknown")
                            .to_string()
                    } else {
                        "Unknown".to_string()
                    }
                }
                Err(_) => "Unknown".to_string(),
            }
        } else {
            "Unknown".to_string()
        };

        (window_title, app_name)
    }
}

#[cfg(target_os = "macos")]
pub fn get_foreground_info() -> (String, String) {
    use std::process::Command;
    // Returns "window_title|app_name" via osascript
    let script = r#"tell application "System Events"
        set frontProc to first process whose frontmost is true
        set appName to name of frontProc
        set winTitle to ""
        try
            set winTitle to title of front window of frontProc
        end try
        return winTitle & "|" & appName
    end tell"#;
    if let Ok(out) = Command::new("osascript").args(["-e", script]).output() {
        let result = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if let Some(sep) = result.find('|') {
            let title = result[..sep].trim().to_string();
            let app = result[sep + 1..].trim().to_string();
            if !app.is_empty() {
                let window_title = if title.is_empty() { app.clone() } else { title };
                return (window_title, app);
            }
        }
    }
    ("Unknown".to_string(), "Unknown".to_string())
}

#[cfg(target_os = "linux")]
pub fn get_foreground_info() -> (String, String) {
    // Pure Rust X11 via x11rb — no xdotool install needed
    use x11rb::connection::Connection;
    use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
    use x11rb::rust_connection::RustConnection;

    let display = std::env::var("DISPLAY").unwrap_or_else(|_| ":0".to_string());
    let (conn, screen_num) = match RustConnection::connect(Some(&display)) {
        Ok(c) => c,
        Err(_) => return ("Unknown".to_string(), "Unknown".to_string()),
    };
    let root = conn.setup().roots[screen_num].root;

    // Get _NET_ACTIVE_WINDOW atom → active window id
    let active_atom = conn.intern_atom(false, b"_NET_ACTIVE_WINDOW").ok()
        .and_then(|c| c.reply().ok()).map(|r| r.atom);
    let win = active_atom.and_then(|atom| {
        conn.get_property(false, root, atom, AtomEnum::WINDOW, 0, 1).ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| r.value32().and_then(|mut i| i.next()))
    });

    let win = match win {
        Some(w) if w != 0 => w,
        _ => return ("Unknown".to_string(), "Unknown".to_string()),
    };

    // Window title via _NET_WM_NAME (UTF-8)
    let utf8_atom = conn.intern_atom(false, b"UTF8_STRING").ok()
        .and_then(|c| c.reply().ok()).map(|r| r.atom).unwrap_or(AtomEnum::STRING.into());
    let title_atom = conn.intern_atom(false, b"_NET_WM_NAME").ok()
        .and_then(|c| c.reply().ok()).map(|r| r.atom);
    let title = title_atom.and_then(|atom| {
        conn.get_property(false, win, atom, utf8_atom, 0, 1024).ok()
            .and_then(|c| c.reply().ok())
            .and_then(|r| String::from_utf8(r.value).ok())
            .map(|s| s.trim_end_matches('\0').to_string())
    }).unwrap_or_default();

    // App name via WM_CLASS (second null-separated value = class)
    let app = conn.get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 1024).ok()
        .and_then(|c| c.reply().ok())
        .and_then(|r| {
            let s = String::from_utf8_lossy(&r.value).to_string();
            s.split('\0').nth(1).map(|s| s.to_string())
        }).unwrap_or_else(|| "Unknown".to_string());

    let window_title = if title.is_empty() { app.clone() } else { title };
    (window_title, app)
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub fn get_foreground_info() -> (String, String) {
    ("Unknown".to_string(), "Unknown".to_string())
}

#[cfg(target_os = "windows")]
fn read_clipboard_text() -> Option<(String, u64)> {
    use windows::Win32::System::DataExchange::{
        CloseClipboard, GetClipboardData, IsClipboardFormatAvailable, OpenClipboard,
    };
    use windows::Win32::System::Memory::{GlobalLock, GlobalSize, GlobalUnlock};

    const CF_UNICODETEXT: u32 = 13;

    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return None;
        }
        if OpenClipboard(None).is_err() {
            return None;
        }

        let result = (|| {
            use windows::Win32::Foundation::HGLOBAL;
            let handle = GetClipboardData(CF_UNICODETEXT).ok()?;
            let hglobal = HGLOBAL(handle.0 as *mut _);
            let ptr = GlobalLock(hglobal) as *const u16;
            if ptr.is_null() {
                return None;
            }
            let size_bytes = GlobalSize(hglobal);
            let size_words = (size_bytes / 2).min(4096);
            let slice = std::slice::from_raw_parts(ptr, size_words);
            let null_pos = slice.iter().position(|&c| c == 0).unwrap_or(slice.len());
            let text = String::from_utf16_lossy(&slice[..null_pos]);
            let _ = GlobalUnlock(hglobal);

            if text.trim().is_empty() {
                return None;
            }

            let hash: u64 = text
                .bytes()
                .enumerate()
                .fold(0u64, |acc, (i, b)| acc.wrapping_add((b as u64).wrapping_mul(i as u64 + 1)));

            Some((text, hash))
        })();

        let _ = CloseClipboard();
        result
    }
}

#[cfg(target_os = "macos")]
fn read_clipboard_text() -> Option<(String, u64)> {
    // arboard — pure Rust clipboard, no pbpaste needed
    let text = arboard::Clipboard::new().ok()?.get_text().ok()?;
    if text.trim().is_empty() {
        return None;
    }
    let hash: u64 = text
        .bytes()
        .enumerate()
        .fold(0u64, |acc, (i, b)| acc.wrapping_add((b as u64).wrapping_mul(i as u64 + 1)));
    Some((text, hash))
}

#[cfg(target_os = "linux")]
fn read_clipboard_text() -> Option<(String, u64)> {
    // arboard — pure Rust clipboard, no xclip install needed
    let text = arboard::Clipboard::new().ok()?.get_text().ok()?;
    if text.trim().is_empty() {
        return None;
    }
    let hash: u64 = text
        .bytes()
        .enumerate()
        .fold(0u64, |acc, (i, b)| acc.wrapping_add((b as u64).wrapping_mul(i as u64 + 1)));
    Some((text, hash))
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn read_clipboard_text() -> Option<(String, u64)> {
    None
}

// ===== macOS + Linux: shared atomic mouse state (updated by rdev listener) =====

#[cfg(any(target_os = "macos", target_os = "linux"))]
pub mod unix_state {
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    static MOUSE_X: AtomicI32 = AtomicI32::new(0);
    static MOUSE_Y: AtomicI32 = AtomicI32::new(0);
    static LEFT_DOWN: AtomicBool = AtomicBool::new(false);

    pub fn set_mouse(x: i32, y: i32) {
        MOUSE_X.store(x, Ordering::Relaxed);
        MOUSE_Y.store(y, Ordering::Relaxed);
    }

    pub fn get_mouse() -> (i32, i32) {
        (MOUSE_X.load(Ordering::Relaxed), MOUSE_Y.load(Ordering::Relaxed))
    }

    pub fn set_left_down(down: bool) {
        LEFT_DOWN.store(down, Ordering::Relaxed);
    }

    pub fn get_left_down() -> bool {
        LEFT_DOWN.load(Ordering::Relaxed)
    }
}

/// Map rdev Key to a readable string (macOS + Linux)
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn rdev_key_to_str(key: rdev::Key) -> Option<String> {
    use rdev::Key;
    match key {
        Key::KeyA => Some("A".to_string()),
        Key::KeyB => Some("B".to_string()),
        Key::KeyC => Some("C".to_string()),
        Key::KeyD => Some("D".to_string()),
        Key::KeyE => Some("E".to_string()),
        Key::KeyF => Some("F".to_string()),
        Key::KeyG => Some("G".to_string()),
        Key::KeyH => Some("H".to_string()),
        Key::KeyI => Some("I".to_string()),
        Key::KeyJ => Some("J".to_string()),
        Key::KeyK => Some("K".to_string()),
        Key::KeyL => Some("L".to_string()),
        Key::KeyM => Some("M".to_string()),
        Key::KeyN => Some("N".to_string()),
        Key::KeyO => Some("O".to_string()),
        Key::KeyP => Some("P".to_string()),
        Key::KeyQ => Some("Q".to_string()),
        Key::KeyR => Some("R".to_string()),
        Key::KeyS => Some("S".to_string()),
        Key::KeyT => Some("T".to_string()),
        Key::KeyU => Some("U".to_string()),
        Key::KeyV => Some("V".to_string()),
        Key::KeyW => Some("W".to_string()),
        Key::KeyX => Some("X".to_string()),
        Key::KeyY => Some("Y".to_string()),
        Key::KeyZ => Some("Z".to_string()),
        Key::Num0 => Some("0".to_string()),
        Key::Num1 => Some("1".to_string()),
        Key::Num2 => Some("2".to_string()),
        Key::Num3 => Some("3".to_string()),
        Key::Num4 => Some("4".to_string()),
        Key::Num5 => Some("5".to_string()),
        Key::Num6 => Some("6".to_string()),
        Key::Num7 => Some("7".to_string()),
        Key::Num8 => Some("8".to_string()),
        Key::Num9 => Some("9".to_string()),
        Key::Kp0 => Some("[NUM0]".to_string()),
        Key::Kp1 => Some("[NUM1]".to_string()),
        Key::Kp2 => Some("[NUM2]".to_string()),
        Key::Kp3 => Some("[NUM3]".to_string()),
        Key::Kp4 => Some("[NUM4]".to_string()),
        Key::Kp5 => Some("[NUM5]".to_string()),
        Key::Kp6 => Some("[NUM6]".to_string()),
        Key::Kp7 => Some("[NUM7]".to_string()),
        Key::Kp8 => Some("[NUM8]".to_string()),
        Key::Kp9 => Some("[NUM9]".to_string()),
        Key::Backspace => Some("[BACKSPACE]".to_string()),
        Key::Tab => Some("[TAB]".to_string()),
        Key::Return | Key::KpReturn => Some("[ENTER]".to_string()),
        Key::Escape => Some("[ESC]".to_string()),
        Key::Space => Some("[SPACE]".to_string()),
        Key::LeftArrow => Some("[LEFT]".to_string()),
        Key::RightArrow => Some("[RIGHT]".to_string()),
        Key::UpArrow => Some("[UP]".to_string()),
        Key::DownArrow => Some("[DOWN]".to_string()),
        Key::Delete => Some("[DELETE]".to_string()),
        Key::Home => Some("[HOME]".to_string()),
        Key::End => Some("[END]".to_string()),
        Key::PageUp => Some("[PGUP]".to_string()),
        Key::PageDown => Some("[PGDN]".to_string()),
        Key::F1 => Some("[F1]".to_string()),
        Key::F2 => Some("[F2]".to_string()),
        Key::F3 => Some("[F3]".to_string()),
        Key::F4 => Some("[F4]".to_string()),
        Key::F5 => Some("[F5]".to_string()),
        Key::F6 => Some("[F6]".to_string()),
        Key::F7 => Some("[F7]".to_string()),
        Key::F8 => Some("[F8]".to_string()),
        Key::F9 => Some("[F9]".to_string()),
        Key::F10 => Some("[F10]".to_string()),
        Key::F11 => Some("[F11]".to_string()),
        Key::F12 => Some("[F12]".to_string()),
        Key::Comma => Some(",".to_string()),
        Key::Dot => Some(".".to_string()),
        Key::Slash => Some("/".to_string()),
        Key::SemiColon => Some(";".to_string()),
        Key::Quote => Some("'".to_string()),
        Key::LeftBracket => Some("[".to_string()),
        Key::RightBracket => Some("]".to_string()),
        Key::BackSlash => Some("\\".to_string()),
        Key::Minus => Some("-".to_string()),
        Key::Equal => Some("=".to_string()),
        Key::BackQuote => Some("`".to_string()),
        Key::KpMinus => Some("-".to_string()),
        Key::KpPlus => Some("+".to_string()),
        Key::KpMultiply => Some("*".to_string()),
        Key::KpDivide => Some("/".to_string()),
        _ => None,
    }
}

/// Map Windows Virtual Key code to a readable string
#[cfg(target_os = "windows")]
fn vk_to_str(vk: u16) -> Option<String> {
    match vk {
        // A-Z
        0x41..=0x5A => Some(char::from_u32(vk as u32)?.to_string()),
        // 0-9
        0x30..=0x39 => Some(char::from_u32(vk as u32)?.to_string()),
        // Special
        0x08 => Some("[BACKSPACE]".to_string()),
        0x09 => Some("[TAB]".to_string()),
        0x0D => Some("[ENTER]".to_string()),
        0x1B => Some("[ESC]".to_string()),
        0x20 => Some("[SPACE]".to_string()),
        0x25 => Some("[LEFT]".to_string()),
        0x26 => Some("[UP]".to_string()),
        0x27 => Some("[RIGHT]".to_string()),
        0x28 => Some("[DOWN]".to_string()),
        0x2E => Some("[DELETE]".to_string()),
        // Numpad 0-9
        0x60..=0x69 => Some(format!("[NUM{}]", vk - 0x60)),
        // F1-F12
        0x70..=0x7B => Some(format!("[F{}]", vk - 0x6F)),
        // OEM punctuation
        0xBA => Some(";".to_string()),
        0xBB => Some("=".to_string()),
        0xBC => Some(",".to_string()),
        0xBD => Some("-".to_string()),
        0xBE => Some(".".to_string()),
        0xBF => Some("/".to_string()),
        0xC0 => Some("`".to_string()),
        0xDB => Some("[".to_string()),
        0xDC => Some("\\".to_string()),
        0xDD => Some("]".to_string()),
        0xDE => Some("'".to_string()),
        _ => None,
    }
}
