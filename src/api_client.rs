use log::info;
use reqwest::Client;
use serde_json::{json, Value};
use std::time::Duration;

use crate::config::ServiceConfig;
use crate::system_info::SystemInfo;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// Register the installation with the server
pub async fn register(
    client: &Client,
    config: &ServiceConfig,
    info: &SystemInfo,
) -> Result<(), String> {
    let url = format!("{}/dawell360-installations", config.api_base_url);

    let body = json!({
        "machineid": info.machineid,
        "macaddress": info.macaddress,
        "ipaddress": info.ipaddress,
        "hostname": info.hostname,
        "operatingsystem": info.operatingsystem,
        "os_version": info.os_version,
        "cpu_model": info.cpu_model,
        "cpu_core": info.cpu_core,
        "totalram": info.totalram,
        "screenresolution": info.screenresolution,
        "status": "online",
        "user_id": config.user_id,
        "organization_id": config.organization_id,
    });

    let response = client
        .post(&url)
        .timeout(REQUEST_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Registration request failed: {}", e))?;

    let status = response.status();
    if status.is_success() {
        info!("Registration successful (status: {})", status);
        Ok(())
    } else {
        let text = response.text().await.unwrap_or_default();
        Err(format!("Registration failed (status: {}): {}", status, text))
    }
}

/// Send a heartbeat to the server.
/// Returns `Ok(Some(url))` if the server has a pending reinstall for this machine.
pub async fn heartbeat(
    client: &Client,
    config: &ServiceConfig,
    info: &SystemInfo,
) -> Result<Option<String>, String> {
    let url = format!("{}/dawell360-installations/heartbeat", config.api_base_url);

    let body = json!({
        "macaddress": info.macaddress,
        "ipaddress": info.ipaddress,
        "machineid": info.machineid,
    });

    let response = client
        .post(&url)
        .timeout(REQUEST_TIMEOUT)
        .json(&body)
        .send()
        .await
        .map_err(|e| format!("Heartbeat request failed: {}", e))?;

    let status = response.status();
    if status.is_success() {
        // Check if server wants us to self-update
        if let Ok(json) = response.json::<Value>().await {
            if let Some(reinstall_url) = json.get("reinstall_url").and_then(|v| v.as_str()) {
                info!("Heartbeat: server requested reinstall from {}", reinstall_url);
                return Ok(Some(reinstall_url.to_string()));
            }
        }
        Ok(None)
    } else {
        Err(format!("Heartbeat failed (status: {})", status))
    }
}

