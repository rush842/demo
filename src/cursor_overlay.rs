/// Floating "Remote Access" label shown on the employee's screen when an admin is
/// controlling their PC via remote desktop.
///
/// Windows: an always-on-top, click-through layered popup window that follows the
/// remote cursor, showing "Remote: <Admin Name>".
///
/// Non-Windows: no-ops.

// ── Non-Windows stubs ─────────────────────────────────────────────────────────
#[cfg(not(target_os = "windows"))]
#[allow(dead_code)]
pub fn show_label(_name: &str, _cursor_x: i32, _cursor_y: i32) {}

#[cfg(not(target_os = "windows"))]
pub fn hide_label() {}

// ── Windows implementation ────────────────────────────────────────────────────
#[cfg(target_os = "windows")]
pub use win_overlay::show_label;

#[cfg(target_os = "windows")]
pub use win_overlay::hide_label;

#[cfg(target_os = "windows")]
mod win_overlay {
    use lazy_static::lazy_static;
    use log::warn;
    use std::sync::Mutex;
    use std::thread;
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::*;
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows::Win32::UI::WindowsAndMessaging::*;

    // ── Custom window messages ────────────────────────────────────────────────
    const WM_OVERLAY_MOVE: u32 = WM_USER + 200; // wParam = x, lParam = y (screen coords)
    const WM_OVERLAY_HIDE: u32 = WM_USER + 201;

    // ── Auto-hide timer ───────────────────────────────────────────────────────
    const TIMER_ID_AUTOHIDE: usize = 1;
    const AUTOHIDE_MS: u32 = 2000; // hide label 2 s after last mouse move

    // ── Overlay dimensions ────────────────────────────────────────────────────
    const OL_W: i32 = 230;
    const OL_H: i32 = 28;
    const OL_OFFSET_X: i32 = 18;
    const OL_OFFSET_Y: i32 = 18;

    // ── Global handle ─────────────────────────────────────────────────────────
    struct OverlayHandle {
        hwnd: HWND,
        name: String, // current accessor name — used to detect admin switches
    }
    // HWND is a pointer — we promise to only use it from the overlay thread after creation,
    // and from any thread only via PostMessageW (which is thread-safe).
    unsafe impl Send for OverlayHandle {}
    unsafe impl Sync for OverlayHandle {}

    lazy_static! {
        static ref OVERLAY: Mutex<Option<OverlayHandle>> = Mutex::new(None);
    }

    /// Show/update the cursor label.  Call on every remote mousemove.
    pub fn show_label(accessor_name: &str, cursor_x: i32, cursor_y: i32) {
        let mut guard = OVERLAY.lock().unwrap();

        // If accessor name changed (different admin), destroy old overlay so it
        // gets recreated with the new name.
        let name_changed = guard.as_ref().map(|h| h.name != accessor_name).unwrap_or(false);
        if name_changed {
            if let Some(ref old_h) = *guard {
                unsafe { let _ = PostMessageW(old_h.hwnd, WM_OVERLAY_HIDE, WPARAM(0), LPARAM(0)); }
            }
            *guard = None;
        }

        // Spawn overlay thread if not yet running
        if guard.is_none() {
            let name_clone = accessor_name.to_string();
            let (tx, rx) = std::sync::mpsc::channel::<HWND>();
            let _ = thread::Builder::new()
                .name("cursor-overlay".into())
                .spawn(move || overlay_thread(name_clone, tx));

            match rx.recv_timeout(std::time::Duration::from_secs(3)) {
                Ok(hwnd) if hwnd.0 != 0 => {
                    *guard = Some(OverlayHandle { hwnd, name: accessor_name.to_string() });
                }
                _ => {
                    warn!("cursor_overlay: failed to create overlay window");
                    return;
                }
            }
        }

        if let Some(ref h) = *guard {
            let wx = cursor_x + OL_OFFSET_X;
            let wy = cursor_y + OL_OFFSET_Y;
            unsafe {
                let _ = PostMessageW(
                    h.hwnd,
                    WM_OVERLAY_MOVE,
                    WPARAM(wx as isize as usize),
                    LPARAM(wy as isize),
                );
            }
        }
    }

    /// Hide and destroy the cursor label.  Call when the remote session ends.
    pub fn hide_label() {
        let mut guard = OVERLAY.lock().unwrap();
        if let Some(ref h) = *guard {
            unsafe {
                let _ = PostMessageW(h.hwnd, WM_OVERLAY_HIDE, WPARAM(0), LPARAM(0));
            }
        }
        *guard = None;
    }

    // ── Overlay window thread ─────────────────────────────────────────────────

    fn overlay_thread(name: String, tx: std::sync::mpsc::Sender<HWND>) {
        unsafe {
            let class_name_wide: Vec<u16> =
                "DawellCursorOverlay\0".encode_utf16().collect();

            let hmodule = GetModuleHandleW(PCWSTR::null()).unwrap_or_default();
            let hinstance = HINSTANCE(hmodule.0);

            // Register window class (ignore error if already registered)
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(overlay_wndproc),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinstance,
                hIcon: HICON(0),
                hCursor: HCURSOR(0),
                hbrBackground: HBRUSH(0), // painted in WM_ERASEBKGND
                lpszMenuName: PCWSTR::null(),
                lpszClassName: PCWSTR(class_name_wide.as_ptr()),
                hIconSm: HICON(0),
            };
            let _ = RegisterClassExW(&wc); // may fail if already registered — that's fine

            // Window title = accessor name (read back in WM_PAINT)
            let title_wide: Vec<u16> = name.encode_utf16().chain(std::iter::once(0)).collect();

            let hwnd = CreateWindowExW(
                WS_EX_TOPMOST
                    | WS_EX_NOACTIVATE
                    | WS_EX_TOOLWINDOW
                    | WS_EX_LAYERED
                    | WS_EX_TRANSPARENT, // click-through
                PCWSTR(class_name_wide.as_ptr()),
                PCWSTR(title_wide.as_ptr()),
                WS_POPUP,
                -4000, 0, // start offscreen
                OL_W, OL_H,
                HWND(0),
                HMENU(0),
                hinstance,
                None,
            );

            if hwnd.0 == 0 {
                let _ = tx.send(HWND(0));
                return;
            }

            // 88 % opaque dark background
            let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0), 225, LWA_ALPHA);

            let _ = tx.send(hwnd);

            // Message loop
            let mut msg = MSG::default();
            loop {
                let ret = GetMessageW(&mut msg, HWND(0), 0, 0).0;
                if ret <= 0 {
                    break;
                }
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }

    unsafe extern "system" fn overlay_wndproc(
        hwnd: HWND,
        msg: u32,
        wp: WPARAM,
        lp: LPARAM,
    ) -> LRESULT {
        match msg {
            m if m == WM_OVERLAY_MOVE => {
                let x = wp.0 as isize as i32;
                let y = lp.0 as i32;
                // Position + show window
                let _ = SetWindowPos(
                    hwnd,
                    HWND_TOPMOST,
                    x, y, OL_W, OL_H,
                    SWP_SHOWWINDOW | SWP_NOACTIVATE,
                );
                // Reset auto-hide timer — fires AUTOHIDE_MS after last move
                let _ = KillTimer(hwnd, TIMER_ID_AUTOHIDE);
                let _ = SetTimer(hwnd, TIMER_ID_AUTOHIDE, AUTOHIDE_MS, None);
                // Repaint
                let _ = RedrawWindow(
                    hwnd,
                    None,
                    HRGN(0),
                    RDW_INVALIDATE | RDW_UPDATENOW,
                );
                LRESULT(0)
            }
            WM_TIMER => {
                if wp.0 == TIMER_ID_AUTOHIDE {
                    // Mouse stopped — hide the label until next mousemove
                    let _ = KillTimer(hwnd, TIMER_ID_AUTOHIDE);
                    let _ = ShowWindow(hwnd, SW_HIDE);
                }
                LRESULT(0)
            }
            m if m == WM_OVERLAY_HIDE => {
                // Session ended — kill timer, destroy window
                let _ = KillTimer(hwnd, TIMER_ID_AUTOHIDE);
                let _ = ShowWindow(hwnd, SW_HIDE);
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_ERASEBKGND => {
                // Dark rounded background
                let hdc = HDC(wp.0 as isize);
                let brush = CreateSolidBrush(COLORREF(0x00_22_22_22));
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                FillRect(hdc, &rc, brush);
                let _ = DeleteObject(HGDIOBJ(brush.0 as isize));
                LRESULT(1) // prevent default erase
            }
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                let hdc = BeginPaint(hwnd, &mut ps);

                // Read accessor name from window title
                let mut name_buf = [0u16; 128];
                let name_len = GetWindowTextW(hwnd, &mut name_buf) as usize;
                let accessor = String::from_utf16_lossy(&name_buf[..name_len]);
                let label = format!("  Remote: {}", accessor);
                let mut label_wide: Vec<u16> = label.encode_utf16().collect();

                // White text on transparent background
                SetBkMode(hdc, TRANSPARENT);
                SetTextColor(hdc, COLORREF(0x00_FF_FF_FF));

                // Use default GUI font
                let font = GetStockObject(DEFAULT_GUI_FONT);
                let prev_font = SelectObject(hdc, font);

                // Draw text vertically centred
                let mut rc = RECT::default();
                let _ = GetClientRect(hwnd, &mut rc);
                rc.left += 4;
                DrawTextW(
                    hdc,
                    &mut label_wide,
                    &mut rc,
                    DT_LEFT | DT_SINGLELINE | DT_VCENTER,
                );

                SelectObject(hdc, prev_font);
                EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
