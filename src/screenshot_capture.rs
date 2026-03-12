// Dedicated screenshot capture module - independent of streaming
// Uses Windows GDI for screen capture to avoid conflict with scrap-based streaming

use log::warn;
use std::time::Duration;

#[cfg(target_os = "windows")]
use windows::Win32::Graphics::Gdi::{
    BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
    GetDC, GetDIBits, ReleaseDC, SelectObject, BITMAPINFO, BITMAPINFOHEADER,
    BI_RGB, DIB_RGB_COLORS, SRCCOPY,
};
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

/// Capture screenshot using Windows GDI (independent of scrap)
#[cfg(target_os = "windows")]
pub fn capture_screen_gdi() -> Result<(Vec<u8>, u32, u32), String> {
    unsafe {
        // Get screen dimensions
        let width = GetSystemMetrics(SM_CXSCREEN) as u32;
        let height = GetSystemMetrics(SM_CYSCREEN) as u32;

        if width == 0 || height == 0 {
            return Err("Failed to get screen dimensions".to_string());
        }

        // Get device context for the screen
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return Err("Failed to get screen DC".to_string());
        }

        // Create compatible DC and bitmap
        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_invalid() {
            ReleaseDC(None, hdc_screen);
            return Err("Failed to create compatible DC".to_string());
        }

        let hbitmap = CreateCompatibleBitmap(hdc_screen, width as i32, height as i32);
        if hbitmap.is_invalid() {
            DeleteDC(hdc_mem);
            ReleaseDC(None, hdc_screen);
            return Err("Failed to create bitmap".to_string());
        }

        // Select bitmap into memory DC
        let old_bitmap = SelectObject(hdc_mem, hbitmap);

        // Copy screen to bitmap
        let result = BitBlt(
            hdc_mem,
            0, 0,
            width as i32, height as i32,
            hdc_screen,
            0, 0,
            SRCCOPY,
        );

        if result.is_err() {
            SelectObject(hdc_mem, old_bitmap);
            DeleteObject(hbitmap);
            DeleteDC(hdc_mem);
            ReleaseDC(None, hdc_screen);
            return Err("BitBlt failed".to_string());
        }

        // Prepare bitmap info header
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // Negative for top-down
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [Default::default()],
        };

        // Allocate buffer for pixel data (BGRA format)
        let buffer_size = (width * height * 4) as usize;
        let mut buffer: Vec<u8> = vec![0; buffer_size];

        // Get bitmap bits
        let lines = GetDIBits(
            hdc_mem,
            hbitmap,
            0,
            height,
            Some(buffer.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        // Cleanup GDI objects
        SelectObject(hdc_mem, old_bitmap);
        DeleteObject(hbitmap);
        DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);

        if lines == 0 {
            return Err("GetDIBits failed".to_string());
        }

        // Convert BGRA to RGB
        let mut rgb_data = Vec::with_capacity((width * height * 3) as usize);
        for pixel in buffer.chunks_exact(4) {
            rgb_data.push(pixel[2]); // R
            rgb_data.push(pixel[1]); // G
            rgb_data.push(pixel[0]); // B
        }

        Ok((rgb_data, width, height))
    }
}

#[cfg(not(target_os = "windows"))]
pub fn capture_screen_gdi() -> Result<(Vec<u8>, u32, u32), String> {
    use scrap::{Capturer, Display};

    let display =
        Display::primary().map_err(|e| format!("Failed to get primary display: {}", e))?;
    let logical_width = display.width() as u32;
    let logical_height = display.height() as u32;

    let mut capturer =
        Capturer::new(display).map_err(|e| format!("Failed to create capturer: {}", e))?;

    // Try up to 50 times (WouldBlock means the frame isn't ready yet)
    for _ in 0..50 {
        match capturer.frame() {
            Ok(frame) => {
                // scrap returns BGRA on macOS/Linux.
                // On macOS Retina, the frame is at physical (2x) resolution
                // while display.width()/height() are logical dimensions.
                // Detect actual dimensions from byte count to fix diagonal-stripe glitch.
                let frame_bytes = frame.len();
                let logical_area = (logical_width * logical_height) as usize;
                let actual_area = frame_bytes / 4;
                let (width, height) = if logical_area == 0 || actual_area == logical_area {
                    (logical_width, logical_height)
                } else {
                    let scale_sq = (actual_area + logical_area / 2) / logical_area;
                    let scale = (scale_sq as f64).sqrt().round() as u32;
                    (logical_width * scale.max(1), logical_height * scale.max(1))
                };
                let stride = frame_bytes / height as usize;
                let mut rgb_data = Vec::with_capacity((width * height * 3) as usize);
                for row in frame.chunks_exact(stride).take(height as usize) {
                    for pixel in row.chunks_exact(4).take(width as usize) {
                        rgb_data.push(pixel[2]); // R (from BGRA)
                        rgb_data.push(pixel[1]); // G
                        rgb_data.push(pixel[0]); // B
                    }
                }
                return Ok((rgb_data, width, height));
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => return Err(format!("Screen capture failed: {}", e)),
        }
    }

    Err("Screen capture timed out after 50 attempts".to_string())
}

/// Capture with retry logic
pub fn capture_screen_with_retry(max_retries: u32) -> Result<(Vec<u8>, u32, u32), String> {
    for attempt in 1..=max_retries {
        match capture_screen_gdi() {
            Ok(result) => return Ok(result),
            Err(e) => {
                if attempt < max_retries {
                    warn!("Screenshot capture failed (attempt {}/{}): {}", attempt, max_retries, e);
                    std::thread::sleep(Duration::from_millis(100));
                } else {
                    return Err(format!("All {} capture attempts failed: {}", max_retries, e));
                }
            }
        }
    }
    Err("Capture failed".to_string())
}
