use log::{info, warn};
use reqwest::Client;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;


pub struct VideoRecorder {
    client: Client,
    api_base_url: String,
    macaddress: String,
    organization_id: u32,
}

impl VideoRecorder {
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

    pub async fn run(self) {
        // Wait before first recording so the service settles
        sleep(Duration::from_secs(10)).await;

        loop {
            // Re-fetch settings each cycle — picks up admin enable/disable without restart
            let settings = crate::capture_settings::fetch_capture_settings(
                &self.client, &self.api_base_url, self.organization_id,
            ).await;

            if !settings.video_enabled {
                // Not enabled — check again in 30 seconds
                sleep(Duration::from_secs(30)).await;
                continue;
            }

            // Clip length = video_interval (minutes) → seconds, no artificial cap
            let clip_secs = settings.video_interval.max(1) * 60;
            info!(
                "Starting video clip ({} sec, quality={})...",
                clip_secs, settings.video_quality
            );

            // Cancel flag — set by the settings watcher if settings change mid-recording
            let cancel = Arc::new(AtomicBool::new(false));

            // Spawn a watcher: every 30 s checks if settings changed materially.
            // If so, sets cancel flag so the recording loop exits early.
            let cancel_w = cancel.clone();
            let client_w = self.client.clone();
            let url_w    = self.api_base_url.clone();
            let org_id   = self.organization_id;
            let snap_quality  = settings.video_quality.clone();
            let snap_interval = settings.video_interval;
            let watcher = tokio::spawn(async move {
                loop {
                    sleep(Duration::from_secs(30)).await;
                    let s = crate::capture_settings::fetch_capture_settings(
                        &client_w, &url_w, org_id,
                    ).await;
                    let changed = !s.video_enabled
                        || s.video_quality  != snap_quality
                        || s.video_interval != snap_interval;
                    if changed {
                        info!("Settings changed mid-recording — cancelling clip early");
                        cancel_w.store(true, Ordering::SeqCst);
                        break;
                    }
                }
            });

            match self.record_and_upload(clip_secs, &settings.video_quality, cancel).await {
                Ok(()) => info!("Video clip uploaded successfully"),
                Err(e) => warn!("Video clip failed: {}", e),
            }

            // Stop the watcher (no-op if it already exited)
            watcher.abort();
            // No extra sleep — recording itself takes clip_secs; next cycle re-checks settings.
        }
    }

    async fn record_and_upload(&self, duration_secs: u64, quality: &str, cancel: Arc<AtomicBool>) -> Result<(), String> {
        let quality = quality.to_string();
        let quality_for_closure = quality.clone();

        // Record to a temp video file (blocking)
        // Windows → H.264 MP4 via Media Foundation
        // macOS/Linux → MJPEG AVI via pure Rust (no ffmpeg)
        let (temp_path, width, height, fps, actual_secs) =
            tokio::task::spawn_blocking(move || record_screen_mp4(&quality_for_closure, duration_secs, cancel))
                .await
                .map_err(|e| format!("Task join: {}", e))??;

        // Read file bytes
        let video_bytes = tokio::fs::read(&temp_path)
            .await
            .map_err(|e| format!("Read temp file: {}", e))?;

        // Clean up temp file
        let _ = tokio::fs::remove_file(&temp_path).await;

        if video_bytes.is_empty() {
            return Err("Recorded video file is empty".to_string());
        }

        info!(
            "Video ready: {}x{} @{}fps {}s → {} bytes (quality={})",
            width, height, fps, actual_secs, video_bytes.len(), quality
        );

        // Capture thumbnail — GDI on Windows, scrap on macOS/Linux
        let quality_for_thumb = quality.clone();
        #[cfg(target_os = "windows")]
        let thumbnail = tokio::task::spawn_blocking(move || capture_thumbnail_gdi(&quality_for_thumb))
            .await.ok().and_then(|r| r.ok());
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let thumbnail = tokio::task::spawn_blocking(move || capture_thumbnail_scrap(&quality_for_thumb))
            .await.ok().and_then(|r| r.ok());
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        let thumbnail: Option<Vec<u8>> = None;

        let app_name = crate::input_logger::get_foreground_info().0;

        // Build multipart form
        let url = format!("{}/videos/upload", self.api_base_url);

        // Windows → .mp4 / video/mp4 ; macOS+Linux → .avi / video/x-msvideo (MJPEG AVI)
        #[cfg(target_os = "windows")]
        let (video_filename, video_mime) = ("recording.mp4", "video/mp4");
        #[cfg(any(target_os = "macos", target_os = "linux"))]
        let (video_filename, video_mime) = ("recording.avi", "video/x-msvideo");
        #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
        let (video_filename, video_mime) = ("recording.mp4", "video/mp4");

        let video_part = reqwest::multipart::Part::bytes(video_bytes)
            .file_name(video_filename)
            .mime_str(video_mime)
            .map_err(|e| format!("MIME: {}", e))?;

        let mut form = reqwest::multipart::Form::new()
            .text("macaddress", self.macaddress.clone())
            .text("organization_id", self.organization_id.to_string())
            .text("quality", quality.clone())
            .text("width", width.to_string())
            .text("height", height.to_string())
            .text("fps", fps.to_string())
            .text("duration", actual_secs.to_string())
            .text("include_audio", "false")
            .text("app_name", app_name)
            .part("video", video_part);

        if let Some(thumb) = thumbnail {
            if let Ok(part) = reqwest::multipart::Part::bytes(thumb)
                .file_name("thumb.jpg")
                .mime_str("image/jpeg")
            {
                form = form.part("thumbnail", part);
            }
        }

        // Upload timeout scales with clip duration: base 120s + 10s per minute of video
        let upload_timeout_secs = 120 + duration_secs / 60 * 10;
        match tokio::time::timeout(
            Duration::from_secs(upload_timeout_secs),
            self.client.post(&url).multipart(form).send(),
        )
        .await
        {
            Ok(Ok(resp)) if resp.status().is_success() => Ok(()),
            Ok(Ok(resp)) => Err(format!("Upload HTTP {}", resp.status())),
            Ok(Err(e)) => Err(format!("Upload request: {}", e)),
            Err(_) => Err("Upload timed out".to_string()),
        }
    }
}

// ─── Helpers ────────────────────────────────────────────────────────────────

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

/// Get target resolution based on quality setting
/// 480p, 720p, 1080p
fn get_target_resolution(quality: &str, original_width: u32, original_height: u32) -> (u32, u32) {
    let target_height: u32 = match quality {
        "480p" => 480,
        "720p" => 720,
        "1080p" => 1080,
        _ => 720, // default to 720p
    };

    // If original is smaller than target, keep original
    if original_height <= target_height {
        return (original_width, original_height);
    }

    // Calculate new width maintaining aspect ratio
    let aspect_ratio = original_width as f64 / original_height as f64;
    let new_width = (target_height as f64 * aspect_ratio).round() as u32;

    // Ensure dimensions are even (required for video encoding)
    let new_width = (new_width / 2) * 2;
    let target_height = (target_height / 2) * 2;

    (new_width, target_height)
}

/// Capture a JPEG screenshot for use as video thumbnail using GDI (Windows only)
#[cfg(target_os = "windows")]
fn capture_thumbnail_gdi(quality: &str) -> Result<Vec<u8>, String> {
    use image::codecs::jpeg::JpegEncoder;
    use image::imageops::FilterType;
    use image::{ImageBuffer, ImageEncoder, RgbImage};
    use std::io::Cursor;

    // Use GDI capture (independent of streaming)
    let (rgb_data, orig_width, orig_height) = screenshot_capture::capture_screen_with_retry(3)?;

    let img: RgbImage = ImageBuffer::from_raw(orig_width, orig_height, rgb_data)
        .ok_or("Failed to create image buffer")?;

    // Resize based on quality
    let (target_width, target_height) = get_target_resolution(quality, orig_width, orig_height);
    let final_img: RgbImage = if target_width != orig_width || target_height != orig_height {
        image::imageops::resize(&img, target_width, target_height, FilterType::Triangle)
    } else {
        img
    };

    let w = final_img.width();
    let h = final_img.height();

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut Cursor::new(&mut jpeg), 70)
        .write_image(final_img.as_raw(), w, h, image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("JPEG encode: {}", e))?;

    Ok(jpeg)
}

/// Capture a JPEG thumbnail using scrap (macOS + Linux)
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn capture_thumbnail_scrap(quality: &str) -> Result<Vec<u8>, String> {
    use image::codecs::jpeg::JpegEncoder;
    use image::imageops::FilterType;
    use image::{ImageBuffer, ImageEncoder, Rgb as ImgRgb};
    use scrap::{Capturer, Display};
    use std::io::{Cursor, ErrorKind};

    let display = Display::primary().map_err(|e| format!("Display: {}", e))?;
    let orig_width  = display.width()  as u32;
    let orig_height = display.height() as u32;
    let mut capturer = Capturer::new(display).map_err(|e| format!("Capturer: {}", e))?;

    // Retry up to 30 × 10 ms = 300 ms for a valid frame
    let frame_data = {
        let mut result = None;
        for _ in 0..30 {
            match capturer.frame() {
                Ok(frame) => {
                    let bytes = frame.len();
                    let logical_area = (orig_width * orig_height) as usize;
                    let actual_area  = bytes / 4;
                    let (aw, ah) = if logical_area == 0 || actual_area == logical_area {
                        (orig_width, orig_height)
                    } else {
                        let scale = ((actual_area + logical_area / 2) / logical_area) as f64;
                        let s = scale.sqrt().round() as u32;
                        (orig_width * s.max(1), orig_height * s.max(1))
                    };
                    let stride = bytes / ah as usize;
                    let mut rgb = Vec::with_capacity((aw * ah * 3) as usize);
                    for row in frame.chunks_exact(stride).take(ah as usize) {
                        for px in row.chunks_exact(4).take(aw as usize) {
                            rgb.push(px[2]); rgb.push(px[1]); rgb.push(px[0]);
                        }
                    }
                    result = Some((rgb, aw, ah));
                    break;
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }
        result.ok_or_else(|| "Failed to capture thumbnail frame".to_string())?
    };

    let (rgb, aw, ah) = frame_data;
    let img: image::ImageBuffer<ImgRgb<u8>, Vec<u8>> =
        ImageBuffer::from_raw(aw, ah, rgb).ok_or("Image buffer")?;

    let (tw, th) = get_target_resolution(quality, aw, ah);
    let final_img = if tw != aw || th != ah {
        image::imageops::resize(&img, tw, th, FilterType::Triangle)
    } else {
        img
    };

    let mut jpeg = Vec::new();
    JpegEncoder::new_with_quality(&mut Cursor::new(&mut jpeg), 70)
        .write_image(final_img.as_raw(), tw, th, image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("JPEG encode: {}", e))?;
    Ok(jpeg)
}

/// Record the primary display to a temp video file.
/// Returns (path, width, height, fps, duration_seconds).
fn record_screen_mp4(
    quality: &str,
    duration_secs: u64,
    cancel: Arc<AtomicBool>,
) -> Result<(PathBuf, u32, u32, u32, u64), String> {
    #[cfg(target_os = "windows")]
    return record_screen_mp4_win(quality, duration_secs, cancel);

    #[cfg(any(target_os = "macos", target_os = "linux"))]
    return record_screen_mp4_mac(quality, duration_secs, cancel);

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    Err("Video recording not supported on this platform".to_string())
}

/// macOS + Linux: capture frames with scrap, encode to MJPEG AVI (pure Rust, no ffmpeg).
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn record_screen_mp4_mac(
    quality: &str,
    duration_secs: u64,
    cancel: Arc<AtomicBool>,
) -> Result<(PathBuf, u32, u32, u32, u64), String> {
    use image::codecs::jpeg::JpegEncoder;
    use image::{ImageBuffer, ImageEncoder, Rgb as ImgRgb};
    use scrap::{Capturer, Display};
    use std::io::ErrorKind;

    const FPS: u32 = 3;

    let display =
        Display::primary().map_err(|e| format!("Failed to get display: {}", e))?;
    let orig_width = display.width() as u32;
    let orig_height = display.height() as u32;
    let (target_width, target_height) =
        get_target_resolution(quality, orig_width, orig_height);

    let mut capturer =
        Capturer::new(display).map_err(|e| format!("Failed to create capturer: {}", e))?;

    let jpeg_quality: u8 = match quality {
        "480p" => 60,
        "1080p" => 80,
        _ => 70,
    };

    let total_frames = duration_secs * FPS as u64;
    let frame_interval = std::time::Duration::from_millis(1000 / FPS as u64);

    // Collect all JPEG frames in memory
    // Pre-allocate RGB buffer once — reused across all frames (avoids ~6MB alloc per frame)
    let mut rgb_buf: Vec<u8> = Vec::with_capacity((orig_width * orig_height * 4 * 3) as usize);
    let mut jpeg_frames: Vec<Vec<u8>> = Vec::with_capacity(total_frames as usize);

    for _ in 0..total_frames {
        if cancel.load(Ordering::SeqCst) {
            break;
        }

        let t_start = std::time::Instant::now();

        // Capture with retries (scrap may return WouldBlock on macOS Retina).
        rgb_buf.clear();
        let mut frame_dims: Option<(u32, u32)> = None;

        for _ in 0..30 {
            match capturer.frame() {
                Ok(frame) => {
                    let frame_bytes = frame.len();
                    let logical_area = (orig_width * orig_height) as usize;
                    let actual_area = frame_bytes / 4;
                    let (actual_w, actual_h) = if logical_area == 0 || actual_area == logical_area {
                        (orig_width, orig_height)
                    } else {
                        let scale_sq = (actual_area + logical_area / 2) / logical_area;
                        let scale = (scale_sq as f64).sqrt().round() as u32;
                        (orig_width * scale.max(1), orig_height * scale.max(1))
                    };
                    let stride = frame_bytes / actual_h as usize;
                    for row in frame.chunks_exact(stride).take(actual_h as usize) {
                        for px in row.chunks_exact(4).take(actual_w as usize) {
                            rgb_buf.push(px[2]);
                            rgb_buf.push(px[1]);
                            rgb_buf.push(px[0]);
                        }
                    }
                    frame_dims = Some((actual_w, actual_h));
                    break;
                }
                Err(ref e) if e.kind() == ErrorKind::WouldBlock => {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                Err(_) => break,
            }
        }

        if let Some((actual_w, actual_h)) = frame_dims {
            let mut jpeg_data = Vec::new();

            if target_width != actual_w || target_height != actual_h {
                // Resize needed: move rgb_buf into ImageBuffer, resize, recover buffer
                let img: image::ImageBuffer<ImgRgb<u8>, Vec<u8>> =
                    ImageBuffer::from_raw(actual_w, actual_h, std::mem::take(&mut rgb_buf))
                        .ok_or("Failed to create image buffer")?;
                let resized = image::imageops::resize(
                    &img,
                    target_width,
                    target_height,
                    image::imageops::FilterType::Triangle,
                );
                rgb_buf = img.into_raw(); // Recover buffer capacity for reuse

                JpegEncoder::new_with_quality(
                    &mut std::io::Cursor::new(&mut jpeg_data),
                    jpeg_quality,
                )
                .write_image(
                    resized.as_raw(),
                    resized.width(),
                    resized.height(),
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| format!("JPEG encode: {}", e))?;
            } else {
                // No resize: encode directly from rgb_buf (zero-copy)
                JpegEncoder::new_with_quality(
                    &mut std::io::Cursor::new(&mut jpeg_data),
                    jpeg_quality,
                )
                .write_image(
                    &rgb_buf,
                    actual_w,
                    actual_h,
                    image::ExtendedColorType::Rgb8,
                )
                .map_err(|e| format!("JPEG encode: {}", e))?;
            }

            jpeg_frames.push(jpeg_data);
        }

        let elapsed = t_start.elapsed();
        if elapsed < frame_interval {
            std::thread::sleep(frame_interval - elapsed);
        }
    }

    if jpeg_frames.is_empty() {
        return Err("No frames captured".to_string());
    }

    // Write MJPEG AVI — pure Rust, no ffmpeg needed
    let out_path = std::env::temp_dir().join(format!("dawell_{}.avi", now_millis()));
    let frames_written = jpeg_frames.len() as u64;
    let avi_bytes = encode_mjpeg_avi(&jpeg_frames, target_width, target_height, FPS);
    std::fs::write(&out_path, &avi_bytes)
        .map_err(|e| format!("Write AVI: {}", e))?;

    let actual_secs = frames_written / FPS as u64;
    Ok((out_path, target_width, target_height, FPS, actual_secs))
}

/// Pure Rust MJPEG AVI encoder.
/// Writes an AVI RIFF container with MJPEG video stream — no external tools needed.
#[cfg(any(target_os = "macos", target_os = "linux"))]
fn encode_mjpeg_avi(frames: &[Vec<u8>], width: u32, height: u32, fps: u32) -> Vec<u8> {
    let mut buf: Vec<u8> = Vec::new();
    let frame_count = frames.len() as u32;

    // ── helpers ───────────────────────────────────────────────────────────────
    fn w32(buf: &mut Vec<u8>, v: u32) { buf.extend_from_slice(&v.to_le_bytes()); }
    fn w16(buf: &mut Vec<u8>, v: u16) { buf.extend_from_slice(&v.to_le_bytes()); }
    fn wcc(buf: &mut Vec<u8>, s: &[u8; 4]) { buf.extend_from_slice(s); }
    fn set32(buf: &mut Vec<u8>, pos: usize, v: u32) {
        buf[pos..pos+4].copy_from_slice(&v.to_le_bytes());
    }

    // ── RIFF AVI header ───────────────────────────────────────────────────────
    wcc(&mut buf, b"RIFF");
    let riff_size_pos = buf.len(); w32(&mut buf, 0);
    wcc(&mut buf, b"AVI ");

    // ── LIST hdrl ─────────────────────────────────────────────────────────────
    wcc(&mut buf, b"LIST");
    let hdrl_size_pos = buf.len(); w32(&mut buf, 0);
    wcc(&mut buf, b"hdrl");

    // avih — MainAVIHeader (56 bytes of data)
    wcc(&mut buf, b"avih"); w32(&mut buf, 56);
    w32(&mut buf, 1_000_000 / fps.max(1)); // dwMicroSecPerFrame
    w32(&mut buf, 0);                       // dwMaxBytesPerSec
    w32(&mut buf, 0);                       // dwPaddingGranularity
    w32(&mut buf, 0x10);                    // dwFlags: AVIF_HASINDEX
    w32(&mut buf, frame_count);             // dwTotalFrames
    w32(&mut buf, 0);                       // dwInitialFrames
    w32(&mut buf, 1);                       // dwStreams
    w32(&mut buf, 0);                       // dwSuggestedBufferSize
    w32(&mut buf, width);                   // dwWidth
    w32(&mut buf, height);                  // dwHeight
    w32(&mut buf, 0); w32(&mut buf, 0); w32(&mut buf, 0); w32(&mut buf, 0); // reserved[4]

    // LIST strl
    wcc(&mut buf, b"LIST");
    let strl_size_pos = buf.len(); w32(&mut buf, 0);
    wcc(&mut buf, b"strl");

    // strh — AVISTREAMHEADER (56 bytes of data)
    wcc(&mut buf, b"strh"); w32(&mut buf, 56);
    wcc(&mut buf, b"vids");         // fccType
    wcc(&mut buf, b"MJPG");         // fccHandler
    w32(&mut buf, 0);               // dwFlags
    w16(&mut buf, 0);               // wPriority
    w16(&mut buf, 0);               // wLanguage
    w32(&mut buf, 0);               // dwInitialFrames
    w32(&mut buf, 1);               // dwScale
    w32(&mut buf, fps);             // dwRate  (fps/1 = fps)
    w32(&mut buf, 0);               // dwStart
    w32(&mut buf, frame_count);     // dwLength
    w32(&mut buf, 0);               // dwSuggestedBufferSize
    w32(&mut buf, 0xFFFF_FFFF);     // dwQuality (-1 = default)
    w32(&mut buf, 0);               // dwSampleSize
    // rcFrame as 4 × SHORT (left, top, right, bottom)
    w16(&mut buf, 0); w16(&mut buf, 0);
    w16(&mut buf, width as u16); w16(&mut buf, height as u16);

    // strf — BITMAPINFOHEADER (40 bytes of data)
    wcc(&mut buf, b"strf"); w32(&mut buf, 40);
    w32(&mut buf, 40);              // biSize
    w32(&mut buf, width);           // biWidth
    w32(&mut buf, height);          // biHeight
    w16(&mut buf, 1);               // biPlanes
    w16(&mut buf, 24);              // biBitCount
    wcc(&mut buf, b"MJPG");         // biCompression
    w32(&mut buf, width * height * 3); // biSizeImage
    w32(&mut buf, 0); w32(&mut buf, 0); // XPels, YPels
    w32(&mut buf, 0); w32(&mut buf, 0); // ClrUsed, ClrImportant

    // Close strl + hdrl
    let n = (buf.len() - strl_size_pos - 4) as u32;
    set32(&mut buf, strl_size_pos, n);
    let n = (buf.len() - hdrl_size_pos - 4) as u32;
    set32(&mut buf, hdrl_size_pos, n);

    // ── LIST movi ─────────────────────────────────────────────────────────────
    wcc(&mut buf, b"LIST");
    let movi_size_pos = buf.len(); w32(&mut buf, 0);
    wcc(&mut buf, b"movi");
    let movi_data_start = buf.len(); // offsets in idx1 are relative to here

    // Frame chunks + build index
    let mut index: Vec<(u32, u32)> = Vec::with_capacity(frames.len());
    for frame in frames {
        let frame_offset = (buf.len() - movi_data_start) as u32;
        let frame_size   = frame.len() as u32;
        wcc(&mut buf, b"00dc");
        w32(&mut buf, frame_size);
        buf.extend_from_slice(frame);
        if frame_size % 2 != 0 { buf.push(0); } // word-align padding
        index.push((frame_offset, frame_size));
    }
    let n = (buf.len() - movi_size_pos - 4) as u32;
    set32(&mut buf, movi_size_pos, n);

    // ── idx1 ─────────────────────────────────────────────────────────────────
    wcc(&mut buf, b"idx1");
    w32(&mut buf, (index.len() as u32) * 16);
    for (offset, size) in &index {
        wcc(&mut buf, b"00dc");
        w32(&mut buf, 0x10); // AVIIF_KEYFRAME
        w32(&mut buf, *offset);
        w32(&mut buf, *size);
    }

    // Fill RIFF size
    let n = (buf.len() - riff_size_pos - 4) as u32;
    set32(&mut buf, riff_size_pos, n);

    buf
}

#[cfg(target_os = "windows")]
fn record_screen_mp4_win(quality: &str, duration_secs: u64, cancel: Arc<AtomicBool>) -> Result<(PathBuf, u32, u32, u32, u64), String> {
    use windows::core::PCWSTR;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    const FPS: u32 = 3;

    // Get screen dimensions using GDI
    let (orig_width, orig_height) = get_screen_dimensions()?;

    // Calculate target resolution based on quality
    let (width, height) = get_target_resolution(quality, orig_width, orig_height);

    info!("Recording at {}x{} (quality={}, original={}x{})", width, height, quality, orig_width, orig_height);

    // Temp file path (UTF-16 for Windows API)
    let temp_path = std::env::temp_dir().join(format!("dawell_{}.mp4", now_millis()));
    let path_wide: Vec<u16> = temp_path
        .to_str()
        .ok_or("Invalid temp path")?
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let total_frames = duration_secs as u32 * FPS;
    let frame_interval = Duration::from_millis(1000 / FPS as u64);
    let frame_duration_100ns: i64 = 10_000_000 / FPS as i64;
    let frame_bytes = (width * height * 4) as usize;

    let mut frames_written: u32 = 0;

    unsafe {
        // Initialize COM on this thread (required by MF on some Windows versions)
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        // Initialize Media Foundation
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET)
            .map_err(|e| format!("MFStartup: {}", e))?;

        // Create SinkWriter — writes directly to the .mp4 file
        let sink_writer =
            MFCreateSinkWriterFromURL(PCWSTR(path_wide.as_ptr()), None, None)
                .map_err(|e| format!("MFCreateSinkWriterFromURL: {}", e))?;

        // ── Output media type: H.264 ─────────────────────────────────────────
        let out_type =
            MFCreateMediaType().map_err(|e| format!("MFCreateMediaType(out): {}", e))?;
        out_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| format!("out major: {}", e))?;
        out_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_H264)
            .map_err(|e| format!("out H264: {}", e))?;

        // Bitrate based on resolution
        let bitrate = match quality {
            "480p" => 500_000,
            "720p" => 800_000,
            "1080p" => 1_500_000,
            _ => 800_000,
        };
        out_type
            .SetUINT32(&MF_MT_AVG_BITRATE, bitrate)
            .map_err(|e| format!("out bitrate: {}", e))?;
        out_type
            .SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))
            .map_err(|e| format!("out frame_size: {}", e))?;
        out_type
            .SetUINT64(&MF_MT_FRAME_RATE, pack(FPS, 1))
            .map_err(|e| format!("out frame_rate: {}", e))?;
        out_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))
            .map_err(|e| format!("out par: {}", e))?;
        out_type
            .SetUINT32(
                &MF_MT_INTERLACE_MODE,
                MFVideoInterlace_Progressive.0 as u32,
            )
            .map_err(|e| format!("out interlace: {}", e))?;

        let stream_idx = sink_writer
            .AddStream(&out_type)
            .map_err(|e| format!("AddStream: {}", e))?;

        // ── Input media type: RGB32 (= BGRA bottom-up) ───────────────────────
        let in_type =
            MFCreateMediaType().map_err(|e| format!("MFCreateMediaType(in): {}", e))?;
        in_type
            .SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)
            .map_err(|e| format!("in major: {}", e))?;
        in_type
            .SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)
            .map_err(|e| format!("in RGB32: {}", e))?;
        in_type
            .SetUINT64(&MF_MT_FRAME_SIZE, pack(width, height))
            .map_err(|e| format!("in frame_size: {}", e))?;
        in_type
            .SetUINT64(&MF_MT_FRAME_RATE, pack(FPS, 1))
            .map_err(|e| format!("in frame_rate: {}", e))?;
        in_type
            .SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))
            .map_err(|e| format!("in par: {}", e))?;
        in_type
            .SetUINT32(
                &MF_MT_INTERLACE_MODE,
                MFVideoInterlace_Progressive.0 as u32,
            )
            .map_err(|e| format!("in interlace: {}", e))?;
        // Positive stride = top-down: GDI captures top-down (negative biHeight in GetDIBits),
        // so stride must be positive to match. Negative stride would cause MF to flip
        // the data again, producing an upside-down video.
        in_type
            .SetUINT32(&MF_MT_DEFAULT_STRIDE, width * 4)
            .map_err(|e| format!("in stride: {}", e))?;

        sink_writer
            .SetInputMediaType(stream_idx, &in_type, None)
            .map_err(|e| format!("SetInputMediaType: {}", e))?;

        sink_writer
            .BeginWriting()
            .map_err(|e| format!("BeginWriting: {}", e))?;

        // ── Frame capture + encode loop using GDI ────────────────────────────
        info!("Starting frame capture loop: {} total frames", total_frames);
        for frame_idx in 0..total_frames {
            // Check if settings changed — exit early so new settings apply immediately
            if cancel.load(Ordering::SeqCst) {
                info!("Recording cancelled at frame {}/{} due to settings change", frame_idx, total_frames);
                break;
            }

            let t_start = std::time::Instant::now();

            // Capture frame using GDI and resize if needed
            let frame_result = capture_frame_for_video(orig_width, orig_height, width, height);

            let raw = match frame_result {
                Ok(data) if !data.is_empty() => data,
                Ok(_) => {
                    warn!("Frame {} capture returned empty data", frame_idx);
                    let elapsed = t_start.elapsed();
                    if elapsed < frame_interval {
                        std::thread::sleep(frame_interval - elapsed);
                    }
                    continue;
                }
                Err(e) => {
                    warn!("Frame {} capture failed: {}", frame_idx, e);
                    let elapsed = t_start.elapsed();
                    if elapsed < frame_interval {
                        std::thread::sleep(frame_interval - elapsed);
                    }
                    continue;
                }
            };

            // Log progress every 30 frames (10 seconds at 3fps)
            if frame_idx % 30 == 0 {
                info!("Recording progress: frame {}/{}", frame_idx, total_frames);
            }

            // Wrap frame data in an IMFMediaBuffer (top-down, no flip needed)
            let buf = MFCreateMemoryBuffer(frame_bytes as u32)
                .map_err(|e| format!("MFCreateMemoryBuffer: {}", e))?;
            {
                let mut p: *mut u8 = std::ptr::null_mut();
                buf.Lock(&mut p, None, None)
                    .map_err(|e| format!("Lock: {}", e))?;
                std::ptr::copy_nonoverlapping(raw.as_ptr(), p, frame_bytes);
                buf.Unlock().map_err(|e| format!("Unlock: {}", e))?;
            }
            buf.SetCurrentLength(frame_bytes as u32)
                .map_err(|e| format!("SetCurrentLength: {}", e))?;

            // Create IMFSample and add buffer
            let sample = MFCreateSample().map_err(|e| format!("MFCreateSample: {}", e))?;
            sample
                .AddBuffer(&buf)
                .map_err(|e| format!("AddBuffer: {}", e))?;
            sample
                .SetSampleTime(frame_idx as i64 * frame_duration_100ns)
                .map_err(|e| format!("SetSampleTime: {}", e))?;
            sample
                .SetSampleDuration(frame_duration_100ns)
                .map_err(|e| format!("SetSampleDuration: {}", e))?;

            sink_writer
                .WriteSample(stream_idx, &sample)
                .map_err(|e| format!("WriteSample: {}", e))?;

            frames_written += 1;

            // Throttle to target FPS
            let elapsed = t_start.elapsed();
            if elapsed < frame_interval {
                std::thread::sleep(frame_interval - elapsed);
            }
        }

        info!("Frame capture complete: {} frames written", frames_written);

        // Finalize the MP4 file
        info!("Finalizing MP4 file...");
        sink_writer
            .Finalize()
            .map_err(|e| format!("Finalize: {}", e))?;

        info!("MP4 finalization complete");

        // Explicitly drop COM objects before shutdown to prevent hang
        info!("Dropping sink_writer...");
        drop(sink_writer);
        info!("Dropping media types...");
        drop(out_type);
        drop(in_type);

        info!("Shutting down Media Foundation...");
        let _ = MFShutdown();
        info!("MFShutdown complete, calling CoUninitialize...");
        CoUninitialize();
        info!("CoUninitialize complete, about to exit unsafe block");
    } // unsafe block ends here

    info!("Exited unsafe block, frames_written={}", frames_written);
    let actual_secs = if FPS > 0 { frames_written / FPS } else { 0 };
    info!("Recording complete: {} frames, {} seconds", frames_written, actual_secs);
    Ok((temp_path, width, height, FPS, actual_secs as u64))
}

/// Get screen dimensions using Windows API
#[cfg(target_os = "windows")]
fn get_screen_dimensions() -> Result<(u32, u32), String> {
    use windows::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};
    unsafe {
        let width = GetSystemMetrics(SM_CXSCREEN) as u32;
        let height = GetSystemMetrics(SM_CYSCREEN) as u32;
        if width == 0 || height == 0 {
            return Err("Failed to get screen dimensions".to_string());
        }
        Ok((width, height))
    }
}

/// Capture a single frame using GDI and resize for video
#[cfg(target_os = "windows")]
fn capture_frame_for_video(
    orig_width: u32,
    orig_height: u32,
    target_width: u32,
    target_height: u32,
) -> Result<Vec<u8>, String> {
    use windows::Win32::Graphics::Gdi::{
        BitBlt, CreateCompatibleBitmap, CreateCompatibleDC, DeleteDC, DeleteObject,
        GetDC, GetDIBits, ReleaseDC, SelectObject, StretchBlt, BITMAPINFO, BITMAPINFOHEADER,
        BI_RGB, DIB_RGB_COLORS, SRCCOPY, HALFTONE, SetStretchBltMode,
    };

    unsafe {
        let hdc_screen = GetDC(None);
        if hdc_screen.is_invalid() {
            return Err("Failed to get screen DC".to_string());
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_invalid() {
            ReleaseDC(None, hdc_screen);
            return Err("Failed to create compatible DC".to_string());
        }

        // Create bitmap at target size
        let hbitmap = CreateCompatibleBitmap(hdc_screen, target_width as i32, target_height as i32);
        if hbitmap.is_invalid() {
            DeleteDC(hdc_mem);
            ReleaseDC(None, hdc_screen);
            return Err("Failed to create bitmap".to_string());
        }

        let old_bitmap = SelectObject(hdc_mem, hbitmap);

        // Use StretchBlt for resizing if dimensions differ
        if target_width != orig_width || target_height != orig_height {
            SetStretchBltMode(hdc_mem, HALFTONE);
            let sb_result = StretchBlt(
                hdc_mem,
                0, 0,
                target_width as i32, target_height as i32,
                hdc_screen,
                0, 0,
                orig_width as i32, orig_height as i32,
                SRCCOPY,
            );
            if sb_result.0 == 0 {
                SelectObject(hdc_mem, old_bitmap);
                DeleteObject(hbitmap);
                DeleteDC(hdc_mem);
                ReleaseDC(None, hdc_screen);
                return Err("StretchBlt failed".to_string());
            }
        } else {
            if BitBlt(
                hdc_mem,
                0, 0,
                target_width as i32, target_height as i32,
                hdc_screen,
                0, 0,
                SRCCOPY,
            ).is_err() {
                SelectObject(hdc_mem, old_bitmap);
                DeleteObject(hbitmap);
                DeleteDC(hdc_mem);
                ReleaseDC(None, hdc_screen);
                return Err("BitBlt failed".to_string());
            }
        }

        // Get BGRA data - negative biHeight for top-down DIB
        let mut bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: target_width as i32,
                biHeight: -(target_height as i32), // Negative for top-down
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

        let buffer_size = (target_width * target_height * 4) as usize;
        let mut buffer: Vec<u8> = vec![0; buffer_size];

        let lines = GetDIBits(
            hdc_mem,
            hbitmap,
            0,
            target_height,
            Some(buffer.as_mut_ptr() as *mut _),
            &mut bmi,
            DIB_RGB_COLORS,
        );

        SelectObject(hdc_mem, old_bitmap);
        DeleteObject(hbitmap);
        DeleteDC(hdc_mem);
        ReleaseDC(None, hdc_screen);

        if lines == 0 {
            return Err("GetDIBits failed".to_string());
        }

        Ok(buffer)
    }
}

/// Pack two u32 values into a UINT64 attribute (high 32 = a, low 32 = b)
#[inline]
fn pack(hi: u32, lo: u32) -> u64 {
    ((hi as u64) << 32) | lo as u64
}
