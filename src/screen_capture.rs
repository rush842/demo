use image::codecs::jpeg::JpegEncoder;
use image::{ImageBuffer, ImageEncoder, Rgb};
use log::{info, warn};
use std::io::Cursor;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

// scrap (DXGI) is only used on non-Windows platforms.
// On Windows the service runs in a background session where DXGI Desktop
// Duplication is unavailable; GDI capture (used by screenshot_capture) works
// in all Windows session types.
#[cfg(not(target_os = "windows"))]
use scrap::{Capturer, Display};

/// Screen capture configuration
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    /// Interval between frames in milliseconds
    pub interval_ms: u64,
    /// JPEG quality (1-100)
    pub quality: u8,
    /// Maximum width (will scale down if larger)
    pub max_width: u32,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        Self {
            interval_ms: 100,
            quality: 80,
            max_width: 1920,
        }
    }
}

/// Captured frame data
#[derive(Debug, Clone)]
pub struct CapturedFrame {
    /// Base64-encoded JPEG data (empty when status is Some)
    pub data: String,
    /// Frame width
    pub width: u32,
    /// Frame height
    pub height: u32,
    /// Capture timestamp (milliseconds since epoch)
    pub timestamp: u64,
    /// Optional status: "screen_unavailable" when PC is sleeping/locked/frozen
    pub status: Option<String>,
}

/// Screen capturer that runs in a separate thread
pub struct ScreenCapturer {
    /// Flag to stop capture
    stop_flag: Arc<AtomicBool>,
    /// Capture thread handle
    thread_handle: Option<thread::JoinHandle<()>>,
}

impl ScreenCapturer {
    /// Start screen capture with the given configuration
    pub fn start<F>(config: CaptureConfig, on_frame: F) -> Self
    where
        F: Fn(CapturedFrame) + Send + 'static,
    {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let stop_flag_clone = stop_flag.clone();

        let thread_handle = thread::spawn(move || {
            capture_loop(config, stop_flag_clone, on_frame);
        });

        Self {
            stop_flag,
            thread_handle: Some(thread_handle),
        }
    }

    /// Stop the screen capture
    pub fn stop(&mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }
}

impl Drop for ScreenCapturer {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Main capture loop — dispatches to GDI on Windows, scrap on other platforms.
fn capture_loop<F>(config: CaptureConfig, stop_flag: Arc<AtomicBool>, on_frame: F)
where
    F: Fn(CapturedFrame),
{
    info!("Screen capture starting with config: {:?}", config);

    #[cfg(target_os = "windows")]
    capture_loop_gdi(config, stop_flag, on_frame);

    #[cfg(not(target_os = "windows"))]
    capture_loop_scrap(config, stop_flag, on_frame);
}

/// Windows: GDI-based capture loop.
/// GDI works reliably in all Windows session types (services, RDP, etc.).
/// DXGI Desktop Duplication (used by scrap) only works in interactive sessions.
#[cfg(target_os = "windows")]
fn capture_loop_gdi<F>(config: CaptureConfig, stop_flag: Arc<AtomicBool>, on_frame: F)
where
    F: Fn(CapturedFrame),
{
    info!("Screen capture (GDI) starting...");
    let interval = Duration::from_millis(config.interval_ms);
    let mut last_capture = Instant::now() - interval;
    let mut consecutive_errors: u32 = 0;
    const MAX_ERRORS: u32 = 10;
    // After MAX_ERRORS, send placeholder every 3s and keep retrying
    const RETRY_INTERVAL: Duration = Duration::from_secs(3);

    while !stop_flag.load(Ordering::SeqCst) {
        let elapsed = last_capture.elapsed();
        if elapsed < interval {
            thread::sleep(interval - elapsed);
        }
        last_capture = Instant::now();

        match crate::screenshot_capture::capture_screen_gdi() {
            Ok((rgb_data, orig_width, orig_height)) => {
                if consecutive_errors >= MAX_ERRORS {
                    info!("GDI: screen capture recovered after {} errors", consecutive_errors);
                }
                consecutive_errors = 0;

                let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
                    match ImageBuffer::from_raw(orig_width, orig_height, rgb_data) {
                        Some(i) => i,
                        None => {
                            warn!("GDI: failed to create image buffer");
                            continue;
                        }
                    };

                // Scale down if wider than max_width
                let (final_width, final_height, final_img) = if orig_width > config.max_width {
                    let scale = config.max_width as f32 / orig_width as f32;
                    let new_height = (orig_height as f32 * scale) as u32;
                    let resized = image::imageops::resize(
                        &img,
                        config.max_width,
                        new_height,
                        image::imageops::FilterType::Triangle,
                    );
                    (config.max_width, new_height, resized)
                } else {
                    (orig_width, orig_height, img)
                };

                let mut jpeg_buf = Cursor::new(Vec::new());
                match JpegEncoder::new_with_quality(&mut jpeg_buf, config.quality)
                    .write_image(
                        final_img.as_raw(),
                        final_width,
                        final_height,
                        image::ExtendedColorType::Rgb8,
                    ) {
                    Ok(_) => {
                        let jpeg_data = jpeg_buf.into_inner();
                        let base64_data = base64::Engine::encode(
                            &base64::engine::general_purpose::STANDARD,
                            &jpeg_data,
                        );
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .unwrap_or_default()
                            .as_millis() as u64;
                        on_frame(CapturedFrame {
                            data: base64_data,
                            width: final_width,
                            height: final_height,
                            timestamp,
                            status: None,
                        });
                    }
                    Err(e) => warn!("GDI: JPEG encode failed: {}", e),
                }
            }
            Err(e) => {
                consecutive_errors += 1;
                warn!("GDI capture error ({}/{}): {}", consecutive_errors, MAX_ERRORS, e);

                if consecutive_errors >= MAX_ERRORS {
                    // PC is sleeping / locked / frozen — send unavailable status and retry
                    let timestamp = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    on_frame(CapturedFrame {
                        data: String::new(),
                        width: 0,
                        height: 0,
                        timestamp,
                        status: Some("screen_unavailable".to_string()),
                    });
                    // Wait before retrying — don't hammer a sleeping/locked PC
                    sleep_interruptible(RETRY_INTERVAL, &stop_flag);
                    consecutive_errors = 0; // reset so we retry fresh
                } else {
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
    }

    info!("Screen capture (GDI) stopped");
}

/// Non-Windows: scrap (DXGI / display API) capture loop.
#[cfg(not(target_os = "windows"))]
fn capture_loop_scrap<F>(config: CaptureConfig, stop_flag: Arc<AtomicBool>, on_frame: F)
where
    F: Fn(CapturedFrame),
{
    const MAX_ERRORS: u32 = 10;
    const RETRY_INTERVAL: Duration = Duration::from_secs(3);

    'outer: loop {
        if stop_flag.load(Ordering::SeqCst) { break; }

        // Get primary display
        let display = match Display::primary() {
            Ok(d) => d,
            Err(e) => {
                log::error!("Failed to get primary display: {}", e);
                send_unavailable_frame(&on_frame);
                sleep_interruptible(RETRY_INTERVAL, &stop_flag);
                continue;
            }
        };

        let width = display.width() as u32;
        let height = display.height() as u32;
        info!("Display size: {}x{}", width, height);

        let mut capturer = match Capturer::new(display) {
            Ok(c) => c,
            Err(e) => {
                log::error!("Failed to create screen capturer: {}", e);
                send_unavailable_frame(&on_frame);
                sleep_interruptible(RETRY_INTERVAL, &stop_flag);
                continue;
            }
        };

        let interval = Duration::from_millis(config.interval_ms);
        let mut last_capture = Instant::now();
        let mut consecutive_errors: u32 = 0;
        let frame_width = capturer.width() as u32;
        let frame_height = capturer.height() as u32;

        while !stop_flag.load(Ordering::SeqCst) {
            let elapsed = last_capture.elapsed();
            if elapsed < interval {
                thread::sleep(interval - elapsed);
            }
            last_capture = Instant::now();

            match capturer.frame() {
                Ok(frame) => {
                    if consecutive_errors >= MAX_ERRORS {
                        info!("scrap: screen capture recovered after {} errors", consecutive_errors);
                    }
                    consecutive_errors = 0;
                    match encode_frame(&frame, frame_width, frame_height, &config) {
                        Ok(captured_frame) => on_frame(captured_frame),
                        Err(e) => warn!("Failed to encode frame: {}", e),
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => {
                    consecutive_errors += 1;
                    warn!("Capture error ({}/{}): {}", consecutive_errors, MAX_ERRORS, e);
                    if consecutive_errors >= MAX_ERRORS {
                        // Screen unavailable (sleep/lock/frozen) — send status and retry
                        send_unavailable_frame(&on_frame);
                        sleep_interruptible(RETRY_INTERVAL, &stop_flag);
                        consecutive_errors = 0;
                        // Re-init capturer from outer loop
                        continue 'outer;
                    }
                    thread::sleep(Duration::from_millis(100));
                }
            }
        }
        break;
    }

    info!("Screen capture stopped");
}

#[cfg(not(target_os = "windows"))]
fn send_unavailable_frame<F: Fn(CapturedFrame)>(on_frame: &F) {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64;
    on_frame(CapturedFrame {
        data: String::new(),
        width: 0,
        height: 0,
        timestamp,
        status: Some("screen_unavailable".to_string()),
    });
}

fn sleep_interruptible(dur: Duration, stop_flag: &Arc<AtomicBool>) {
    let mut waited = Duration::ZERO;
    while waited < dur && !stop_flag.load(Ordering::SeqCst) {
        thread::sleep(Duration::from_millis(100));
        waited += Duration::from_millis(100);
    }
}

/// Encode captured frame to JPEG (used by the scrap path on non-Windows)
#[cfg(not(target_os = "windows"))]
fn encode_frame(
    bgra_data: &[u8],
    width: u32,
    height: u32,
    config: &CaptureConfig,
) -> Result<CapturedFrame, String> {
    // On macOS Retina, scrap captures at physical (2x) resolution but
    // capturer.width()/height() return logical dimensions. Detect actual
    // frame dimensions from the raw byte count to avoid diagonal-stripe glitch.
    let frame_bytes = bgra_data.len();
    let logical_area = (width * height) as usize;
    let actual_area = frame_bytes / 4; // 4 bytes per BGRA pixel
    let (actual_width, actual_height) = if logical_area == 0 || actual_area == logical_area {
        (width, height)
    } else {
        let scale_sq = (actual_area + logical_area / 2) / logical_area;
        let scale = (scale_sq as f64).sqrt().round() as u32;
        (width * scale.max(1), height * scale.max(1))
    };

    // Stride = actual bytes per row (includes any row padding)
    let stride = if actual_height > 0 { frame_bytes / actual_height as usize } else { frame_bytes };

    // Build RGB Vec from BGRA — one allocation, no put_pixel overhead
    let mut rgb_data: Vec<u8> = Vec::with_capacity((actual_width * actual_height * 3) as usize);
    for row in bgra_data.chunks_exact(stride).take(actual_height as usize) {
        for pixel in row.chunks_exact(4).take(actual_width as usize) {
            rgb_data.push(pixel[2]); // R (BGRA[2])
            rgb_data.push(pixel[1]); // G (BGRA[1])
            rgb_data.push(pixel[0]); // B (BGRA[0])
            // alpha (BGRA[3]) dropped — JPEG has no alpha
        }
    }

    let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
        ImageBuffer::from_raw(actual_width, actual_height, rgb_data)
            .ok_or("Failed to create image buffer from raw RGB data")?;

    // Scale down if needed (also collapses Retina 2x → logical size)
    let (final_width, final_height, final_img) = if actual_width > config.max_width {
        let scale = config.max_width as f32 / actual_width as f32;
        let new_height = (actual_height as f32 * scale) as u32;
        let resized = image::imageops::resize(
            &img,
            config.max_width,
            new_height,
            image::imageops::FilterType::Triangle,
        );
        (config.max_width, new_height, resized)
    } else {
        (actual_width, actual_height, img)
    };

    // Encode to JPEG
    let mut jpeg_buffer = Cursor::new(Vec::new());
    let encoder = JpegEncoder::new_with_quality(&mut jpeg_buffer, config.quality);

    encoder
        .write_image(
            final_img.as_raw(),
            final_width,
            final_height,
            image::ExtendedColorType::Rgb8,
        )
        .map_err(|e| format!("JPEG encoding failed: {}", e))?;

    // Base64 encode
    let jpeg_data = jpeg_buffer.into_inner();
    let base64_data =
        base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &jpeg_data);

    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as u64;

    Ok(CapturedFrame {
        data: base64_data,
        width: final_width,
        height: final_height,
        timestamp,
        status: None,
    })
}

/// Take a single screenshot.
/// On Windows uses GDI (works in all session types).
/// On other platforms uses scrap.
pub fn take_screenshot(config: &CaptureConfig) -> Result<CapturedFrame, String> {
    #[cfg(target_os = "windows")]
    {
        let (rgb_data, orig_width, orig_height) =
            crate::screenshot_capture::capture_screen_with_retry(3)?;

        let img: ImageBuffer<Rgb<u8>, Vec<u8>> =
            ImageBuffer::from_raw(orig_width, orig_height, rgb_data)
                .ok_or("Failed to create image buffer")?;

        let (final_width, final_height, final_img) = if orig_width > config.max_width {
            let scale = config.max_width as f32 / orig_width as f32;
            let new_height = (orig_height as f32 * scale) as u32;
            let resized = image::imageops::resize(
                &img,
                config.max_width,
                new_height,
                image::imageops::FilterType::Triangle,
            );
            (config.max_width, new_height, resized)
        } else {
            (orig_width, orig_height, img)
        };

        let mut jpeg_buf = Cursor::new(Vec::new());
        JpegEncoder::new_with_quality(&mut jpeg_buf, config.quality)
            .write_image(
                final_img.as_raw(),
                final_width,
                final_height,
                image::ExtendedColorType::Rgb8,
            )
            .map_err(|e| format!("JPEG encode: {}", e))?;

        let jpeg_data = jpeg_buf.into_inner();
        let base64_data =
            base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &jpeg_data);
        let timestamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        return Ok(CapturedFrame {
            data: base64_data,
            width: final_width,
            height: final_height,
            timestamp,
            status: None,
        });
    }

    #[cfg(not(target_os = "windows"))]
    {
        let display = Display::primary().map_err(|e| format!("Failed to get display: {}", e))?;
        let mut capturer =
            Capturer::new(display).map_err(|e| format!("Failed to create capturer: {}", e))?;
        let width = capturer.width() as u32;
        let height = capturer.height() as u32;

        for _ in 0..50 {
            match capturer.frame() {
                Ok(frame) => {
                    return encode_frame(&frame, width, height, config);
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(20));
                }
                Err(e) => {
                    return Err(format!("Capture failed: {}", e));
                }
            }
        }

        Err("Capture timed out".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_capture_config_default() {
        let config = CaptureConfig::default();
        assert_eq!(config.interval_ms, 100);
        assert_eq!(config.quality, 80);
        assert_eq!(config.max_width, 1920);
    }
}
