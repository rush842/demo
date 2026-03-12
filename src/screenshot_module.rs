// Screenshot module - uses Windows GDI capture (independent of streaming)

use base64::Engine;
use image::codecs::jpeg::JpegEncoder;
use image::imageops::FilterType;
use image::{ImageBuffer, ImageEncoder, RgbImage};
use log::info;
use reqwest::Client;
use serde_json::json;
use std::io::Cursor;
use std::time::Duration;

use crate::screenshot_capture;

pub struct ScreenshotUploader {
    client: Client,
    api_base_url: String,
    macaddress: String,
    organization_id: u32,
}

impl ScreenshotUploader {
    pub fn new(
        client: Client,
        api_base_url: String,
        macaddress: String,
        organization_id: u32,
    ) -> Self {
        Self {
            client,
            api_base_url,
            macaddress,
            organization_id,
        }
    }

    pub async fn take_and_upload(&self, quality: &str) -> Result<(), String> {
        // Capture screen using GDI (independent of streaming's scrap capture)
        let quality_str = quality.to_string();
        let result = tokio::task::spawn_blocking(move || {
            capture_and_encode_jpeg(&quality_str)
        })
        .await
        .map_err(|e| format!("Spawn blocking failed: {}", e))?;

        let (jpeg_bytes, width, height) = result?;

        // Encode as base64
        let image_data = base64::engine::general_purpose::STANDARD.encode(&jpeg_bytes);

        // Get active app name
        let app_name = get_active_window_title();

        // Upload
        let url = format!("{}/screenshots/upload", self.api_base_url);
        let body = json!({
            "macaddress": self.macaddress,
            "organization_id": self.organization_id,
            "image_data": image_data,
            "quality": quality,
            "width": width,
            "height": height,
            "app_name": app_name,
        });

        match tokio::time::timeout(
            Duration::from_secs(60),
            self.client.post(&url).json(&body).send(),
        )
        .await
        {
            Ok(Ok(resp)) if resp.status().is_success() => {
                info!("Screenshot uploaded ({}x{}, {} bytes, quality={})", width, height, jpeg_bytes.len(), quality);
                Ok(())
            }
            Ok(Ok(resp)) => Err(format!("Screenshot upload failed: HTTP {}", resp.status())),
            Ok(Err(e)) => Err(format!("Screenshot upload request failed: {}", e)),
            Err(_) => Err("Screenshot upload timed out".to_string()),
        }
    }
}

/// Get target resolution based on quality setting
/// low = 480p, medium = 720p, high = 1080p (original)
fn get_target_resolution(quality: &str, original_width: u32, original_height: u32) -> (u32, u32) {
    let target_height: u32 = match quality {
        "low" => 480,
        "medium" => 720,
        "high" => 1080,
        _ => 720, // default to medium
    };

    // If original is smaller than target, keep original
    if original_height <= target_height {
        return (original_width, original_height);
    }

    // Calculate new width maintaining aspect ratio
    let aspect_ratio = original_width as f64 / original_height as f64;
    let new_width = (target_height as f64 * aspect_ratio).round() as u32;

    (new_width, target_height)
}

/// Capture screen using GDI and encode to JPEG with quality-based resolution
fn capture_and_encode_jpeg(quality: &str) -> Result<(Vec<u8>, u32, u32), String> {
    // Use GDI capture (independent of scrap)
    let (rgb_data, orig_width, orig_height) = screenshot_capture::capture_screen_with_retry(3)?;

    // Create image buffer from captured data
    let img: RgbImage = ImageBuffer::from_raw(orig_width, orig_height, rgb_data)
        .ok_or("Failed to create image buffer")?;

    // Get target resolution based on quality
    let (target_width, target_height) = get_target_resolution(quality, orig_width, orig_height);

    // Resize if needed
    let final_img: RgbImage = if target_width != orig_width || target_height != orig_height {
        image::imageops::resize(&img, target_width, target_height, FilterType::Triangle)
    } else {
        img
    };

    let final_width = final_img.width();
    let final_height = final_img.height();

    // Determine JPEG compression quality
    let jpeg_quality: u8 = match quality {
        "high" => 85,
        "low" => 50,
        _ => 70, // medium
    };

    // Encode to JPEG
    let mut jpeg_bytes = Vec::new();
    let mut cursor = Cursor::new(&mut jpeg_bytes);
    JpegEncoder::new_with_quality(&mut cursor, jpeg_quality)
        .write_image(final_img.as_raw(), final_width, final_height, image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("JPEG encode failed: {}", e))?;

    Ok((jpeg_bytes, final_width, final_height))
}

/// Get the title of the currently active/foreground window
fn get_active_window_title() -> Option<String> {
    #[cfg(target_os = "windows")]
    {
        use windows::Win32::UI::WindowsAndMessaging::{GetForegroundWindow, GetWindowTextW};
        unsafe {
            let hwnd = GetForegroundWindow();
            if hwnd.0 == 0 {
                return None;
            }
            let mut buf = vec![0u16; 512];
            let len = GetWindowTextW(hwnd, &mut buf);
            if len > 0 {
                Some(String::from_utf16_lossy(&buf[..len as usize]))
            } else {
                None
            }
        }
    }
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        let output = Command::new("osascript")
            .args([
                "-e",
                "tell application \"System Events\" to get name of first process whose frontmost is true",
            ])
            .output()
            .ok()?;
        let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !name.is_empty() { Some(name) } else { None }
    }
    #[cfg(target_os = "linux")]
    {
        // Reuse the full foreground info from input_logger (x11rb-based)
        let (title, _app) = crate::input_logger::get_foreground_info();
        if title != "Unknown" { Some(title) } else { None }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        None
    }
}
