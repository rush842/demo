use log::{info, warn};
use serde::{Deserialize, Serialize};
use tungstenite::{connect, Message};

use crate::system_info::SystemInfo;

/// WebSocket message structure
#[derive(Debug, Serialize, Deserialize)]
struct WsMessage {
    #[serde(rename = "type")]
    msg_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    payload: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    timestamp: Option<String>,
}

/// Registration payload for desktop app
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct RegisterDesktopPayload {
    machine_id: String,
    mac_address: String,
    ip_address: String,
    hostname: String,
    operating_system: String,
    os_version: String,
    cpu_model: String,
    cpu_core: u32,
    totalram: String,
    screenresolution: String,
    user_id: u32,
    organization_id: u32,
}

/// Payload for unregistering desktop (service stop)
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UnregisterDesktopPayload {
    machine_id: String,
    mac_address: String,
    user_id: u32,
    organization_id: u32,
}

/// Derive WebSocket URL from API base URL at runtime.
/// e.g. "https://deskpulse.org/api/client" → "wss://deskpulse.org/ws"
///      "http://192.168.1.40:4000/api/client" → "ws://192.168.1.40:4000/ws"
pub fn derive_ws_url(api_base_url: &str) -> String {
    // Strip trailing slash and /api/client (or /api) suffix
    let base = api_base_url.trim_end_matches('/');
    let root = if let Some(pos) = base.find("/api") {
        &base[..pos]
    } else {
        base
    };
    // Convert http(s) scheme to ws(s)
    let ws_root = if let Some(rest) = root.strip_prefix("https://") {
        format!("wss://{}", rest)
    } else if let Some(rest) = root.strip_prefix("http://") {
        format!("ws://{}", rest)
    } else {
        root.to_string()
    };
    format!("{}/ws", ws_root)
}

/// Register desktop via WebSocket
/// This sends `register_desktop` message which triggers `installation_online` broadcast
pub fn register_via_websocket(
    user_id: u32,
    organization_id: u32,
    system_info: &SystemInfo,
    ws_url: &str,
) -> Result<(), String> {
    let ws_url = ws_url.to_string();

    info!("Connecting to WebSocket: {}", ws_url);

    // Connect using string URL directly (tungstenite accepts &str)
    let (mut socket, response) = connect(&ws_url)
        .map_err(|e| format!("WebSocket connection failed: {}", e))?;

    info!("WebSocket connected, status: {}", response.status());

    // Wait for 'connected' message from server
    match socket.read() {
        Ok(Message::Text(text)) => {
            if let Ok(msg) = serde_json::from_str::<WsMessage>(&text) {
                if msg.msg_type == "connected" {
                    info!("Received connected acknowledgment from server");
                }
            }
        }
        Ok(_) => {}
        Err(e) => warn!("Error reading welcome message: {}", e),
    }

    // Send register_desktop message
    let payload = RegisterDesktopPayload {
        machine_id: system_info.machineid.clone(),
        mac_address: system_info.macaddress.clone(),
        ip_address: system_info.ipaddress.clone(),
        hostname: system_info.hostname.clone(),
        operating_system: system_info.operatingsystem.clone(),
        os_version: system_info.os_version.clone(),
        cpu_model: system_info.cpu_model.clone(),
        cpu_core: system_info.cpu_core,
        totalram: system_info.totalram.clone(),
        screenresolution: system_info.screenresolution.clone(),
        user_id,
        organization_id,
    };

    let register_msg = WsMessage {
        msg_type: "register_desktop".to_string(),
        payload: Some(serde_json::to_value(&payload).unwrap()),
        timestamp: Some(chrono::Utc::now().to_rfc3339()),
    };

    let msg_text = serde_json::to_string(&register_msg)
        .map_err(|e| format!("Failed to serialize message: {}", e))?;

    info!("Sending register_desktop message...");
    socket.send(Message::Text(msg_text))
        .map_err(|e| format!("Failed to send message: {}", e))?;

    // Wait for acknowledgment
    match socket.read() {
        Ok(Message::Text(text)) => {
            if let Ok(msg) = serde_json::from_str::<WsMessage>(&text) {
                if msg.msg_type == "register_desktop_ack" {
                    info!("Desktop registration acknowledged via WebSocket");
                } else {
                    info!("Received message: {}", msg.msg_type);
                }
            }
        }
        Ok(_) => {}
        Err(e) => warn!("Error reading ack: {}", e),
    }

    // Close connection gracefully
    let _ = socket.close(None);

    info!("WebSocket registration complete");
    Ok(())
}

/// Unregister desktop via WebSocket (service stop)
/// This sends `unregister_desktop` message which triggers `installation_offline` broadcast
pub fn unregister_via_websocket(
    user_id: u32,
    organization_id: u32,
    system_info: &SystemInfo,
    ws_url: &str,
) -> Result<(), String> {
    let ws_url = ws_url.to_string();

    info!("Connecting to WebSocket for unregistration: {}", ws_url);

    let (mut socket, _) = connect(&ws_url)
        .map_err(|e| format!("WebSocket connection failed: {}", e))?;

    // Read welcome message
    let _ = socket.read();

    // Send unregister_desktop message
    let payload = UnregisterDesktopPayload {
        machine_id: system_info.machineid.clone(),
        mac_address: system_info.macaddress.clone(),
        user_id,
        organization_id,
    };

    let unregister_msg = WsMessage {
        msg_type: "unregister_desktop".to_string(),
        payload: Some(serde_json::to_value(&payload).unwrap()),
        timestamp: Some(chrono::Utc::now().to_rfc3339()),
    };

    let msg_text = serde_json::to_string(&unregister_msg)
        .map_err(|e| format!("Failed to serialize message: {}", e))?;

    info!("Sending unregister_desktop message...");
    socket.send(Message::Text(msg_text))
        .map_err(|e| format!("Failed to send message: {}", e))?;

    // Brief wait for ack
    let _ = socket.read();

    // Close connection
    let _ = socket.close(None);

    info!("WebSocket unregistration complete");
    Ok(())
}

