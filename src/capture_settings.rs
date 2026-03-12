use log::{info, warn};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct CaptureSettings {
    #[serde(default)]
    pub screenshot_enabled: bool,
    #[serde(default = "default_screenshot_interval")]
    pub screenshot_interval: u64, // minutes
    #[serde(default = "default_medium")]
    pub screenshot_quality: String,
    #[serde(default)]
    pub blur_sensitive: bool,
    #[serde(default)]
    pub video_enabled: bool,
    #[serde(default = "default_720p")]
    pub video_quality: String,
    #[serde(default = "default_one")]
    pub video_interval: u64,
    #[serde(default)]
    pub include_audio: bool,
    #[serde(default)]
    pub keystroke_logging: bool,
    #[serde(default)]
    pub clipboard_monitoring: bool,
    #[serde(default)]
    pub exclude_passwords: bool,
    #[serde(default = "default_retention")]
    pub retention_days: u32,
    #[serde(default)]
    pub auto_delete: bool,
}

fn default_screenshot_interval() -> u64 { 5 }
fn default_medium() -> String { "medium".to_string() }
fn default_720p() -> String { "720p".to_string() }
fn default_one() -> u64 { 1 }
fn default_retention() -> u32 { 90 }

impl Default for CaptureSettings {
    fn default() -> Self {
        Self {
            screenshot_enabled: false,
            screenshot_interval: 5,
            screenshot_quality: "medium".to_string(),
            blur_sensitive: false,
            video_enabled: false,
            video_quality: "720p".to_string(),
            video_interval: 1,
            include_audio: false,
            keystroke_logging: false,
            clipboard_monitoring: false,
            exclude_passwords: false,
            retention_days: 90,
            auto_delete: false,
        }
    }
}

// Global cache for last successfully fetched settings
lazy_static::lazy_static! {
    static ref CACHED_SETTINGS: Mutex<Option<CaptureSettings>> = Mutex::new(None);
}

/// Get cached settings (returns None if never successfully fetched)
pub fn get_cached_settings() -> Option<CaptureSettings> {
    CACHED_SETTINGS.lock().ok()?.clone()
}

/// Save settings to cache
fn cache_settings(settings: &CaptureSettings) {
    if let Ok(mut cache) = CACHED_SETTINGS.lock() {
        *cache = Some(settings.clone());
    }
}

/// Fast fetch - tries once with short timeout, returns cached on failure
/// Use this for screenshot loop - no waiting, immediate response
pub async fn fetch_capture_settings(
    client: &Client,
    api_base_url: &str,
    organization_id: u32,
) -> CaptureSettings {
    let url = format!("{}/capture-settings?organization_id={}", api_base_url, organization_id);

    // Fast single attempt with 5 second timeout
    match tokio::time::timeout(Duration::from_secs(5), client.get(&url).send()).await {
        Ok(Ok(resp)) => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(data) = json.get("data") {
                    if let Ok(settings) = serde_json::from_value::<CaptureSettings>(data.clone()) {
                        cache_settings(&settings);
                        return settings;
                    }
                }
            }
        }
        Ok(Err(e)) => {
            warn!("Settings fetch failed: {}", e);
        }
        Err(_) => {
            warn!("Settings fetch timed out (5s)");
        }
    }

    // Return cached settings immediately (no retry delay)
    if let Some(cached) = get_cached_settings() {
        return cached;
    }

    // No cache - return default
    CaptureSettings::default()
}

/// Initial fetch with retries - use only at startup to populate cache
pub async fn fetch_capture_settings_with_retry(
    client: &Client,
    api_base_url: &str,
    organization_id: u32,
) -> CaptureSettings {
    let url = format!("{}/capture-settings?organization_id={}", api_base_url, organization_id);

    // 3 attempts with 5s timeout each
    for attempt in 1..=3 {
        match tokio::time::timeout(Duration::from_secs(5), client.get(&url).send()).await {
            Ok(Ok(resp)) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(data) = json.get("data") {
                        if let Ok(settings) = serde_json::from_value::<CaptureSettings>(data.clone()) {
                            cache_settings(&settings);
                            info!("Capture settings loaded: screenshot_enabled={}, interval={}min",
                                  settings.screenshot_enabled, settings.screenshot_interval);
                            return settings;
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                warn!("Settings fetch failed (attempt {}/3): {}", attempt, e);
            }
            Err(_) => {
                warn!("Settings fetch timed out (attempt {}/3)", attempt);
            }
        }

        if attempt < 3 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    // All retries failed
    if let Some(cached) = get_cached_settings() {
        info!("Using cached settings: screenshot_enabled={}", cached.screenshot_enabled);
        return cached;
    }

    warn!("No settings available, using defaults");
    CaptureSettings::default()
}
