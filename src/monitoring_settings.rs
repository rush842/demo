use log::{info, warn};
use reqwest::Client;
use serde::Deserialize;
use std::sync::Mutex;
use std::time::Duration;

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct MonitoringSettings {
    #[serde(default)]
    pub activity_tracking: bool,
    #[serde(default)]
    pub website_tracking: bool,
    #[serde(default)]
    pub app_usage: bool,
    #[serde(default)]
    pub communication_monitoring: bool,
    #[serde(default)]
    pub data_transfer: bool,
    #[serde(default)]
    pub harmful_activities: bool,
    #[serde(default)]
    pub fraudulent_acts: bool,
}

impl Default for MonitoringSettings {
    fn default() -> Self {
        Self {
            activity_tracking: false,
            website_tracking: false,
            app_usage: false,
            communication_monitoring: false,
            data_transfer: false,
            harmful_activities: false,
            fraudulent_acts: false,
        }
    }
}

// Global cache for last successfully fetched settings
lazy_static::lazy_static! {
    static ref CACHED_MONITORING_SETTINGS: Mutex<Option<MonitoringSettings>> = Mutex::new(None);
}

/// Get cached settings
pub fn get_cached_settings() -> Option<MonitoringSettings> {
    CACHED_MONITORING_SETTINGS.lock().ok()?.clone()
}

/// Save settings to cache
fn cache_settings(settings: &MonitoringSettings) {
    if let Ok(mut cache) = CACHED_MONITORING_SETTINGS.lock() {
        *cache = Some(settings.clone());
    }
}

/// Fast fetch - tries once with short timeout, returns cached on failure
pub async fn fetch_monitoring_settings(
    client: &Client,
    api_base_url: &str,
    organization_id: u32,
) -> MonitoringSettings {
    let url = format!("{}/monitoring-settings?organization_id={}", api_base_url, organization_id);

    // Fast single attempt with 5 second timeout
    match tokio::time::timeout(Duration::from_secs(5), client.get(&url).send()).await {
        Ok(Ok(resp)) => {
            if let Ok(json) = resp.json::<serde_json::Value>().await {
                if let Some(data) = json.get("data") {
                    if let Ok(settings) = serde_json::from_value::<MonitoringSettings>(data.clone()) {
                        cache_settings(&settings);
                        return settings;
                    }
                }
            }
        }
        Ok(Err(e)) => {
            warn!("Monitoring settings fetch failed: {}", e);
        }
        Err(_) => {
            warn!("Monitoring settings fetch timed out (5s)");
        }
    }

    // Return cached settings immediately
    if let Some(cached) = get_cached_settings() {
        return cached;
    }

    MonitoringSettings::default()
}

/// Initial fetch with retries - use only at startup
pub async fn fetch_monitoring_settings_with_retry(
    client: &Client,
    api_base_url: &str,
    organization_id: u32,
) -> MonitoringSettings {
    let url = format!("{}/monitoring-settings?organization_id={}", api_base_url, organization_id);

    for attempt in 1..=3 {
        match tokio::time::timeout(Duration::from_secs(5), client.get(&url).send()).await {
            Ok(Ok(resp)) => {
                if let Ok(json) = resp.json::<serde_json::Value>().await {
                    if let Some(data) = json.get("data") {
                        if let Ok(settings) = serde_json::from_value::<MonitoringSettings>(data.clone()) {
                            cache_settings(&settings);
                            info!("Monitoring settings loaded: activity={}, app_usage={}",
                                  settings.activity_tracking, settings.app_usage);
                            return settings;
                        }
                    }
                }
            }
            Ok(Err(e)) => {
                warn!("Monitoring settings fetch failed (attempt {}/3): {}", attempt, e);
            }
            Err(_) => {
                warn!("Monitoring settings fetch timed out (attempt {}/3)", attempt);
            }
        }

        if attempt < 3 {
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    }

    if let Some(cached) = get_cached_settings() {
        info!("Using cached monitoring settings");
        return cached;
    }

    warn!("No monitoring settings available, using defaults");
    MonitoringSettings::default()
}
