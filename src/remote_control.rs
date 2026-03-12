/// Remote desktop input simulation
/// Receives mouse/keyboard events from the web viewer and simulates them on this machine.
///
/// Event coordinate system:
///   x, y are percentages (0.0–1.0) of screen width/height.
///   The Rust side converts them to absolute pixel coordinates.
///
/// Platform support:
///   Windows  → Win32 SendInput API (no extra deps)
///   macOS    → rdev::simulate
///   Linux    → rdev::simulate

use log::{debug, warn};
use serde::Deserialize;
use std::sync::mpsc::Sender;

/// Incoming remote input event (forwarded by relay from browser viewer)
#[derive(Debug, Deserialize)]
pub struct RemoteInputEvent {
    #[serde(rename = "type")]
    pub event_type: String,

    /// Mouse X position as fraction of screen width (0.0 – 1.0)
    pub x: Option<f64>,
    /// Mouse Y position as fraction of screen height (0.0 – 1.0)
    pub y: Option<f64>,
    /// Mouse button: 0=left  1=middle  2=right
    pub button: Option<u8>,

    /// Web KeyboardEvent.key  (e.g. "Enter", "a", "Shift")
    pub key: Option<String>,
    /// Web KeyboardEvent.code (e.g. "KeyA", "Enter", "ShiftLeft")
    pub code: Option<String>,

    /// Scroll delta X (pixels)
    pub dx: Option<f64>,
    /// Scroll delta Y (pixels)
    pub dy: Option<f64>,

    /// Name of the admin/accessor (forwarded by relay, shown on employee screen)
    #[serde(rename = "accessorName")]
    pub accessor_name: Option<String>,

    /// Clipboard text content (for paste operation from remote viewer)
    pub text: Option<String>,
}

/// Clipboard sync message to send back to browser
#[derive(Debug, Clone)]
pub struct ClipboardSync {
    pub text: String,
}

/// Simulate the given remote input event.
/// `screen_width` / `screen_height` are the actual pixel dimensions of the primary monitor.
/// This is kept for backward compatibility; use `simulate_event_with_clipboard` for clipboard sync.
#[allow(dead_code)]
pub fn simulate_event(event: &RemoteInputEvent, screen_width: u32, screen_height: u32) {
    simulate_event_with_clipboard(event, screen_width, screen_height, None);
}

/// Simulate the given remote input event with optional clipboard sync callback.
/// When Ctrl+C is detected and clipboard_tx is provided, reads clipboard and sends it.
pub fn simulate_event_with_clipboard(
    event: &RemoteInputEvent,
    screen_width: u32,
    screen_height: u32,
    clipboard_tx: Option<&Sender<ClipboardSync>>,
) {
    debug!(
        "remote_input: type={} x={:?} y={:?} btn={:?} code={:?}",
        event.event_type, event.x, event.y, event.button, event.code
    );

    #[cfg(target_os = "windows")]
    simulate_windows(event, screen_width, screen_height, clipboard_tx);

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    simulate_rdev(event, screen_width, screen_height, clipboard_tx);

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        let _ = (screen_width, screen_height, clipboard_tx);
        warn!("remote_input: unsupported platform");
    }
}

// ── Windows implementation ────────────────────────────────────────────────────

/// Track modifier key states for detecting Ctrl+C
#[cfg(target_os = "windows")]
static CTRL_PRESSED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "windows")]
fn simulate_windows(
    event: &RemoteInputEvent,
    screen_width: u32,
    screen_height: u32,
    clipboard_tx: Option<&Sender<ClipboardSync>>,
) {
    use std::sync::atomic::Ordering;
    use windows::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYBD_EVENT_FLAGS,
        KEYEVENTF_KEYUP, KEYEVENTF_UNICODE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_HWHEEL,
        MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP,
        MOUSEEVENTF_MOVE, MOUSEEVENTF_RIGHTDOWN, MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_VIRTUALDESK,
        MOUSEEVENTF_WHEEL, MOUSEINPUT, MOUSE_EVENT_FLAGS, VIRTUAL_KEY,
    };

    let sw = screen_width as f64;
    let sh = screen_height as f64;

    // Convert percentage to MOUSEEVENTF_ABSOLUTE range (0–65535)
    let abs_x = |xp: f64| -> i32 { ((xp * sw / sw) * 65535.0) as i32 };
    let abs_y = |yp: f64| -> i32 { ((yp * sh / sh) * 65535.0) as i32 };

    // Simpler: percentage → 0..65535 directly
    let norm_x = |xp: f64| -> i32 { (xp.clamp(0.0, 1.0) * 65535.0) as i32 };
    let norm_y = |yp: f64| -> i32 { (yp.clamp(0.0, 1.0) * 65535.0) as i32 };

    let _ = (abs_x, abs_y); // suppress unused warning

    match event.event_type.as_str() {
        "mousemove" => {
            if let (Some(xp), Some(yp)) = (event.x, event.y) {
                let input = [INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx: norm_x(xp),
                            dy: norm_y(yp),
                            mouseData: 0,
                            dwFlags: MOUSEEVENTF_MOVE
                                | MOUSEEVENTF_ABSOLUTE
                                | MOUSEEVENTF_VIRTUALDESK,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                }];
                unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) };

                // Show "Remote: <AdminName>" label on the employee's screen near the cursor
                if let Some(ref name) = event.accessor_name {
                    let pixel_x = (xp.clamp(0.0, 1.0) * sw) as i32;
                    let pixel_y = (yp.clamp(0.0, 1.0) * sh) as i32;
                    crate::cursor_overlay::show_label(name, pixel_x, pixel_y);
                }
            }
        }

        "mousedown" | "mouseup" => {
            let pressed = event.event_type == "mousedown";
            let flags: MOUSE_EVENT_FLAGS = match event.button.unwrap_or(0) {
                1 => {
                    if pressed { MOUSEEVENTF_MIDDLEDOWN } else { MOUSEEVENTF_MIDDLEUP }
                }
                2 => {
                    if pressed { MOUSEEVENTF_RIGHTDOWN } else { MOUSEEVENTF_RIGHTUP }
                }
                _ => {
                    if pressed { MOUSEEVENTF_LEFTDOWN } else { MOUSEEVENTF_LEFTUP }
                }
            };

            // Move first if position is available, then click
            if let (Some(xp), Some(yp)) = (event.x, event.y) {
                let move_input = [INPUT {
                    r#type: INPUT_MOUSE,
                    Anonymous: INPUT_0 {
                        mi: MOUSEINPUT {
                            dx: norm_x(xp),
                            dy: norm_y(yp),
                            mouseData: 0,
                            dwFlags: MOUSEEVENTF_MOVE
                                | MOUSEEVENTF_ABSOLUTE
                                | MOUSEEVENTF_VIRTUALDESK,
                            time: 0,
                            dwExtraInfo: 0,
                        },
                    },
                }];
                unsafe { SendInput(&move_input, std::mem::size_of::<INPUT>() as i32) };
            }

            let click_input = [INPUT {
                r#type: INPUT_MOUSE,
                Anonymous: INPUT_0 {
                    mi: MOUSEINPUT {
                        dx: 0,
                        dy: 0,
                        mouseData: 0,
                        dwFlags: flags,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            }];
            unsafe { SendInput(&click_input, std::mem::size_of::<INPUT>() as i32) };
        }

        "scroll" => {
            // Browser deltaY ≈ 100 per scroll notch (Chrome pixel mode).
            // Windows WHEEL_DELTA = 120 per notch → scale = 120/100 = 1.2.
            // Positive browser dy = scroll down; WM_MOUSEWHEEL positive = scroll up → negate.
            if let Some(dy) = event.dy {
                let wheel_delta = (-(dy * 1.2).clamp(-3600.0, 3600.0).round()) as i32;
                if wheel_delta != 0 {
                    let input = [INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0,
                                dy: 0,
                                mouseData: wheel_delta as u32,
                                dwFlags: MOUSEEVENTF_WHEEL,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    }];
                    unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) };
                }
            }
            // Horizontal scroll (same scale)
            if let Some(dx) = event.dx {
                let hwheel_delta = ((dx * 1.2).clamp(-3600.0, 3600.0).round()) as i32;
                if hwheel_delta != 0 {
                    let input = [INPUT {
                        r#type: INPUT_MOUSE,
                        Anonymous: INPUT_0 {
                            mi: MOUSEINPUT {
                                dx: 0,
                                dy: 0,
                                mouseData: hwheel_delta as u32,
                                dwFlags: MOUSEEVENTF_HWHEEL,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    }];
                    unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) };
                }
            }
        }

        "keydown" | "keyup" => {
            let key_up = event.event_type == "keyup";

            // Track Ctrl key state for detecting Ctrl+C
            if let Some(ref code) = event.code {
                if code == "ControlLeft" || code == "ControlRight" {
                    CTRL_PRESSED.store(!key_up, Ordering::SeqCst);
                }
            }

            // Flag: should we read clipboard after simulating this key?
            // Detect on keyup of C while Ctrl is held (simulate key FIRST, then read)
            let read_clipboard_after = key_up
                && event.code.as_deref() == Some("KeyC")
                && CTRL_PRESSED.load(Ordering::SeqCst)
                && clipboard_tx.is_some();

            // Simulate the key via SendInput FIRST
            if let Some(ref code) = event.code {
                if let Some(vk) = web_code_to_vk(code) {
                    let flags = if key_up {
                        KEYEVENTF_KEYUP
                    } else {
                        KEYBD_EVENT_FLAGS(0)
                    };
                    let input = [INPUT {
                        r#type: INPUT_KEYBOARD,
                        Anonymous: INPUT_0 {
                            ki: KEYBDINPUT {
                                wVk: vk,
                                wScan: 0,
                                dwFlags: flags,
                                time: 0,
                                dwExtraInfo: 0,
                            },
                        },
                    }];
                    unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) };
                    // Fall through to clipboard check below (don't return early)
                } else {
                    // Fallback: send as Unicode character
                    if let Some(ref key) = event.key {
                        if key.chars().count() == 1 {
                            let ch = key.chars().next().unwrap() as u16;
                            let flags = if key_up {
                                KEYEVENTF_KEYUP | KEYEVENTF_UNICODE
                            } else {
                                KEYEVENTF_UNICODE
                            };
                            let input = [INPUT {
                                r#type: INPUT_KEYBOARD,
                                Anonymous: INPUT_0 {
                                    ki: KEYBDINPUT {
                                        wVk: VIRTUAL_KEY(0),
                                        wScan: ch,
                                        dwFlags: flags,
                                        time: 0,
                                        dwExtraInfo: 0,
                                    },
                                },
                            }];
                            unsafe { SendInput(&input, std::mem::size_of::<INPUT>() as i32) };
                        }
                    }
                }
            }

            // AFTER simulating keyup C: wait for app to complete the copy, then read clipboard
            if read_clipboard_after {
                if let Some(tx) = clipboard_tx {
                    std::thread::sleep(std::time::Duration::from_millis(200));
                    match get_clipboard_text_windows() {
                        Ok(text) if !text.is_empty() => {
                            debug!("Ctrl+C detected, clipboard has {} chars", text.len());
                            let _ = tx.send(ClipboardSync { text });
                        }
                        _ => {}
                    }
                }
            }
        }

        "clipboard" => {
            // Set the system clipboard with the received text
            if let Some(ref text) = event.text {
                if let Err(e) = set_clipboard_text_windows(text) {
                    warn!("Failed to set clipboard: {:?}", e);
                }
            }
        }

        unknown => {
            warn!("remote_input: unknown event type '{}'", unknown);
        }
    }
}

/// Set Windows clipboard text
#[cfg(target_os = "windows")]
fn set_clipboard_text_windows(text: &str) -> Result<(), Box<dyn std::error::Error>> {
    use windows::Win32::System::DataExchange::{
        OpenClipboard, CloseClipboard, EmptyClipboard, SetClipboardData,
    };
    use windows::Win32::System::Memory::{
        GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE,
    };
    use windows::Win32::Foundation::HWND;

    // CF_UNICODETEXT = 13
    const CF_UNICODETEXT: u32 = 13;

    unsafe {
        // Open clipboard
        if OpenClipboard(HWND(0)).is_err() {
            return Err("Failed to open clipboard".into());
        }

        // Empty existing content
        let _ = EmptyClipboard();

        // Convert text to UTF-16 with null terminator
        let wide: Vec<u16> = text.encode_utf16().chain(std::iter::once(0)).collect();
        let size = wide.len() * 2;

        // Allocate global memory
        let hmem = GlobalAlloc(GMEM_MOVEABLE, size)?;

        // Lock and copy data
        let ptr = GlobalLock(hmem);
        if ptr.is_null() {
            let _ = CloseClipboard();
            return Err("Failed to lock memory".into());
        }
        std::ptr::copy_nonoverlapping(wide.as_ptr(), ptr as *mut u16, wide.len());
        let _ = GlobalUnlock(hmem);

        // Set clipboard data
        let _ = SetClipboardData(CF_UNICODETEXT, windows::Win32::Foundation::HANDLE(hmem.0 as isize));

        let _ = CloseClipboard();
    }

    debug!("Clipboard set: {} chars", text.len());
    Ok(())
}

/// Get Windows clipboard text (for Ctrl+C sync to browser)
#[cfg(target_os = "windows")]
fn get_clipboard_text_windows() -> Result<String, Box<dyn std::error::Error>> {
    use windows::Win32::System::DataExchange::{
        OpenClipboard, CloseClipboard, GetClipboardData, IsClipboardFormatAvailable,
    };
    use windows::Win32::System::Memory::GlobalLock;
    use windows::Win32::Foundation::HWND;

    // CF_UNICODETEXT = 13
    const CF_UNICODETEXT: u32 = 13;

    unsafe {
        // Check if text is available
        if IsClipboardFormatAvailable(CF_UNICODETEXT).is_err() {
            return Ok(String::new());
        }

        // Open clipboard
        if OpenClipboard(HWND(0)).is_err() {
            return Err("Failed to open clipboard".into());
        }

        // Get clipboard data
        let handle = GetClipboardData(CF_UNICODETEXT);
        if handle.is_err() {
            let _ = CloseClipboard();
            return Ok(String::new());
        }
        let handle = handle.unwrap();

        // Lock and read data
        let ptr = GlobalLock(windows::Win32::Foundation::HGLOBAL(handle.0 as *mut std::ffi::c_void));
        if ptr.is_null() {
            let _ = CloseClipboard();
            return Err("Failed to lock clipboard memory".into());
        }

        // Read null-terminated UTF-16 string
        let mut len = 0usize;
        let wide_ptr = ptr as *const u16;
        while *wide_ptr.add(len) != 0 {
            len += 1;
            if len > 1_000_000 { break; } // Safety limit
        }

        let slice = std::slice::from_raw_parts(wide_ptr, len);
        let text = String::from_utf16_lossy(slice);

        // Note: GlobalUnlock returns false if lock count goes to 0, which is expected
        let _ = windows::Win32::System::Memory::GlobalUnlock(
            windows::Win32::Foundation::HGLOBAL(handle.0 as *mut std::ffi::c_void)
        );
        let _ = CloseClipboard();

        Ok(text)
    }
}

/// Map Web KeyboardEvent.code → Windows VIRTUAL_KEY
#[cfg(target_os = "windows")]
fn web_code_to_vk(code: &str) -> Option<windows::Win32::UI::Input::KeyboardAndMouse::VIRTUAL_KEY> {
    use windows::Win32::UI::Input::KeyboardAndMouse::*;
    Some(match code {
        // Letters
        "KeyA" => VK_A, "KeyB" => VK_B, "KeyC" => VK_C, "KeyD" => VK_D,
        "KeyE" => VK_E, "KeyF" => VK_F, "KeyG" => VK_G, "KeyH" => VK_H,
        "KeyI" => VK_I, "KeyJ" => VK_J, "KeyK" => VK_K, "KeyL" => VK_L,
        "KeyM" => VK_M, "KeyN" => VK_N, "KeyO" => VK_O, "KeyP" => VK_P,
        "KeyQ" => VK_Q, "KeyR" => VK_R, "KeyS" => VK_S, "KeyT" => VK_T,
        "KeyU" => VK_U, "KeyV" => VK_V, "KeyW" => VK_W, "KeyX" => VK_X,
        "KeyY" => VK_Y, "KeyZ" => VK_Z,
        // Digits (main row)
        "Digit0" => VK_0, "Digit1" => VK_1, "Digit2" => VK_2, "Digit3" => VK_3,
        "Digit4" => VK_4, "Digit5" => VK_5, "Digit6" => VK_6, "Digit7" => VK_7,
        "Digit8" => VK_8, "Digit9" => VK_9,
        // Numpad
        "Numpad0" => VK_NUMPAD0, "Numpad1" => VK_NUMPAD1, "Numpad2" => VK_NUMPAD2,
        "Numpad3" => VK_NUMPAD3, "Numpad4" => VK_NUMPAD4, "Numpad5" => VK_NUMPAD5,
        "Numpad6" => VK_NUMPAD6, "Numpad7" => VK_NUMPAD7, "Numpad8" => VK_NUMPAD8,
        "Numpad9" => VK_NUMPAD9,
        "NumpadMultiply" => VK_MULTIPLY, "NumpadAdd" => VK_ADD,
        "NumpadSubtract" => VK_SUBTRACT, "NumpadDecimal" => VK_DECIMAL,
        "NumpadDivide" => VK_DIVIDE, "NumpadEnter" => VK_RETURN,
        // Function keys
        "F1" => VK_F1, "F2" => VK_F2, "F3" => VK_F3, "F4" => VK_F4,
        "F5" => VK_F5, "F6" => VK_F6, "F7" => VK_F7, "F8" => VK_F8,
        "F9" => VK_F9, "F10" => VK_F10, "F11" => VK_F11, "F12" => VK_F12,
        // Control keys
        "Enter" | "NumpadEnter2" => VK_RETURN,
        "Escape" => VK_ESCAPE,
        "Space" => VK_SPACE,
        "Backspace" => VK_BACK,
        "Tab" => VK_TAB,
        "Delete" => VK_DELETE,
        "Insert" => VK_INSERT,
        "Home" => VK_HOME,
        "End" => VK_END,
        "PageUp" => VK_PRIOR,
        "PageDown" => VK_NEXT,
        "ArrowLeft" => VK_LEFT,
        "ArrowRight" => VK_RIGHT,
        "ArrowUp" => VK_UP,
        "ArrowDown" => VK_DOWN,
        // Modifiers
        "ShiftLeft" | "ShiftRight" => VK_SHIFT,
        "ControlLeft" | "ControlRight" => VK_CONTROL,
        "AltLeft" | "AltRight" => VK_MENU,
        "MetaLeft" => VK_LWIN,
        "MetaRight" => VK_RWIN,
        "CapsLock" => VK_CAPITAL,
        "NumLock" => VK_NUMLOCK,
        "ScrollLock" => VK_SCROLL,
        // Punctuation (US layout)
        "Minus" => VK_OEM_MINUS,
        "Equal" => VK_OEM_PLUS,
        "BracketLeft" => VK_OEM_4,
        "BracketRight" => VK_OEM_6,
        "Backslash" => VK_OEM_5,
        "Semicolon" => VK_OEM_1,
        "Quote" => VK_OEM_7,
        "Comma" => VK_OEM_COMMA,
        "Period" => VK_OEM_PERIOD,
        "Slash" => VK_OEM_2,
        "Backquote" => VK_OEM_3,
        _ => return None,
    })
}

// ── macOS / Linux implementation (rdev) ──────────────────────────────────────

/// Track modifier key states for detecting Ctrl+C on macOS/Linux
#[cfg(any(target_os = "macos", target_os = "linux"))]
static CTRL_PRESSED_RDEV: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn simulate_rdev(
    event: &RemoteInputEvent,
    screen_width: u32,
    screen_height: u32,
    clipboard_tx: Option<&Sender<ClipboardSync>>,
) {
    use std::sync::atomic::Ordering;
    use rdev::{simulate, Button, EventType, Key};

    let sw = screen_width as f64;
    let sh = screen_height as f64;

    let do_sim = |et: &EventType| {
        if let Err(e) = simulate(et) {
            warn!("rdev simulate error: {:?}", e);
        }
    };

    match event.event_type.as_str() {
        "mousemove" => {
            if let (Some(xp), Some(yp)) = (event.x, event.y) {
                do_sim(&EventType::MouseMove {
                    x: xp.clamp(0.0, 1.0) * sw,
                    y: yp.clamp(0.0, 1.0) * sh,
                });
            }
        }

        "mousedown" | "mouseup" => {
            let pressed = event.event_type == "mousedown";

            // Move to position first
            if let (Some(xp), Some(yp)) = (event.x, event.y) {
                do_sim(&EventType::MouseMove {
                    x: xp.clamp(0.0, 1.0) * sw,
                    y: yp.clamp(0.0, 1.0) * sh,
                });
            }

            let button = match event.button.unwrap_or(0) {
                1 => Button::Middle,
                2 => Button::Right,
                _ => Button::Left,
            };

            if pressed {
                do_sim(&EventType::ButtonPress(button));
            } else {
                do_sim(&EventType::ButtonRelease(button));
            }
        }

        "scroll" => {
            // rdev Wheel event (delta in lines, not pixels — divide browser pixels by ~100)
            let delta_x = event.dx.map(|dx| (dx / 100.0) as i64).unwrap_or(0);
            let delta_y = event.dy.map(|dy| -(dy / 100.0) as i64).unwrap_or(0);
            if delta_x != 0 || delta_y != 0 {
                do_sim(&EventType::Wheel { delta_x, delta_y });
            }
        }

        "keydown" | "keyup" => {
            let pressed = event.event_type == "keydown";

            // Track Ctrl key state
            if let Some(ref code) = event.code {
                if code == "ControlLeft" || code == "ControlRight" {
                    CTRL_PRESSED_RDEV.store(pressed, Ordering::SeqCst);
                }
            }

            // Detect Ctrl+C (keyup of 'C' while Ctrl is pressed) - sync clipboard to browser
            if !pressed {
                if let Some(ref code) = event.code {
                    if code == "KeyC" && CTRL_PRESSED_RDEV.load(Ordering::SeqCst) {
                        if let Some(tx) = clipboard_tx {
                            std::thread::sleep(std::time::Duration::from_millis(100));
                            if let Ok(mut clipboard) = arboard::Clipboard::new() {
                                if let Ok(text) = clipboard.get_text() {
                                    if !text.is_empty() {
                                        debug!("Ctrl+C detected, clipboard has {} chars", text.len());
                                        let _ = tx.send(ClipboardSync { text });
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if let Some(ref code) = event.code {
                if let Some(key) = web_code_to_rdev_key(code) {
                    if pressed {
                        do_sim(&EventType::KeyPress(key));
                    } else {
                        do_sim(&EventType::KeyRelease(key));
                    }
                }
            }
        }

        "clipboard" => {
            // Set the system clipboard with the received text using arboard
            if let Some(ref text) = event.text {
                match arboard::Clipboard::new() {
                    Ok(mut clipboard) => {
                        if let Err(e) = clipboard.set_text(text) {
                            warn!("Failed to set clipboard: {:?}", e);
                        } else {
                            debug!("Clipboard set: {} chars", text.len());
                        }
                    }
                    Err(e) => warn!("Failed to access clipboard: {:?}", e),
                }
            }
        }

        unknown => {
            warn!("remote_input: unknown event type '{}'", unknown);
        }
    }
}

/// Map Web KeyboardEvent.code → rdev Key
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn web_code_to_rdev_key(code: &str) -> Option<rdev::Key> {
    use rdev::Key;
    Some(match code {
        // Letters
        "KeyA" => Key::KeyA, "KeyB" => Key::KeyB, "KeyC" => Key::KeyC, "KeyD" => Key::KeyD,
        "KeyE" => Key::KeyE, "KeyF" => Key::KeyF, "KeyG" => Key::KeyG, "KeyH" => Key::KeyH,
        "KeyI" => Key::KeyI, "KeyJ" => Key::KeyJ, "KeyK" => Key::KeyK, "KeyL" => Key::KeyL,
        "KeyM" => Key::KeyM, "KeyN" => Key::KeyN, "KeyO" => Key::KeyO, "KeyP" => Key::KeyP,
        "KeyQ" => Key::KeyQ, "KeyR" => Key::KeyR, "KeyS" => Key::KeyS, "KeyT" => Key::KeyT,
        "KeyU" => Key::KeyU, "KeyV" => Key::KeyV, "KeyW" => Key::KeyW, "KeyX" => Key::KeyX,
        "KeyY" => Key::KeyY, "KeyZ" => Key::KeyZ,
        // Digits (main row)
        "Digit0" => Key::Num0, "Digit1" => Key::Num1, "Digit2" => Key::Num2,
        "Digit3" => Key::Num3, "Digit4" => Key::Num4, "Digit5" => Key::Num5,
        "Digit6" => Key::Num6, "Digit7" => Key::Num7, "Digit8" => Key::Num8,
        "Digit9" => Key::Num9,
        // Numpad
        "Numpad0" => Key::Kp0, "Numpad1" => Key::Kp1, "Numpad2" => Key::Kp2,
        "Numpad3" => Key::Kp3, "Numpad4" => Key::Kp4, "Numpad5" => Key::Kp5,
        "Numpad6" => Key::Kp6, "Numpad7" => Key::Kp7, "Numpad8" => Key::Kp8,
        "Numpad9" => Key::Kp9,
        "NumpadMultiply" => Key::KpMultiply, "NumpadAdd" => Key::KpPlus,
        "NumpadSubtract" => Key::KpMinus,
        "NumpadDivide" => Key::KpDivide,
        // Control keys
        "Enter" | "NumpadEnter" => Key::Return,
        "Escape" => Key::Escape,
        "Space" => Key::Space,
        "Backspace" => Key::Backspace,
        "Tab" => Key::Tab,
        "Delete" => Key::Delete,
        "Insert" => Key::Insert,
        "Home" => Key::Home,
        "End" => Key::End,
        "PageUp" => Key::PageUp,
        "PageDown" => Key::PageDown,
        "ArrowLeft" => Key::LeftArrow,
        "ArrowRight" => Key::RightArrow,
        "ArrowUp" => Key::UpArrow,
        "ArrowDown" => Key::DownArrow,
        // Function keys
        "F1" => Key::F1, "F2" => Key::F2, "F3" => Key::F3, "F4" => Key::F4,
        "F5" => Key::F5, "F6" => Key::F6, "F7" => Key::F7, "F8" => Key::F8,
        "F9" => Key::F9, "F10" => Key::F10, "F11" => Key::F11, "F12" => Key::F12,
        // Modifiers
        "ShiftLeft" => Key::ShiftLeft,
        "ShiftRight" => Key::ShiftRight,
        "ControlLeft" => Key::ControlLeft,
        "ControlRight" => Key::ControlRight,
        "AltLeft" => Key::Alt,
        "AltRight" => Key::AltGr,
        "MetaLeft" => Key::MetaLeft,
        "MetaRight" => Key::MetaRight,
        "CapsLock" => Key::CapsLock,
        "NumLock" => Key::NumLock,
        "ScrollLock" => Key::ScrollLock,
        // Punctuation (US layout)
        "Minus" => Key::Minus,
        "Equal" => Key::Equal,
        "BracketLeft" => Key::LeftBracket,
        "BracketRight" => Key::RightBracket,
        "Backslash" => Key::BackSlash,
        "Semicolon" => Key::SemiColon,
        "Quote" => Key::Quote,
        "Comma" => Key::Comma,
        "Period" => Key::Dot,
        "Slash" => Key::Slash,
        "Backquote" => Key::BackQuote,
        _ => return None,
    })
}
