use aes_gcm::{
    aead::{Aead, AeadCore, KeyInit, OsRng},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceConfig {
    pub user_id: u32,
    pub organization_id: u32,
    pub api_base_url: String,
}

#[derive(Debug, Deserialize)]
struct TokenPayload {
    user_id: serde_json::Value,
    organization_id: serde_json::Value,
}

/// Decode a base64-encoded JSON token into (user_id, organization_id)
pub fn decode_token(token: &str) -> Result<(u32, u32), String> {
    let decoded_bytes = base64::engine::general_purpose::STANDARD
        .decode(token)
        .map_err(|e| format!("Failed to decode base64 token: {}", e))?;

    let json_str = String::from_utf8(decoded_bytes)
        .map_err(|e| format!("Invalid UTF-8 in token: {}", e))?;

    let payload: TokenPayload = serde_json::from_str(&json_str)
        .map_err(|e| format!("Failed to parse token JSON: {}", e))?;

    let user_id = parse_id_value(&payload.user_id)
        .ok_or_else(|| "Invalid user_id in token".to_string())?;

    let organization_id = parse_id_value(&payload.organization_id)
        .ok_or_else(|| "Invalid organization_id in token".to_string())?;

    Ok((user_id, organization_id))
}

/// Parse a JSON value that could be a number or string into u32
fn parse_id_value(value: &serde_json::Value) -> Option<u32> {
    match value {
        serde_json::Value::Number(n) => n.as_u64().map(|v| v as u32),
        serde_json::Value::String(s) => s.parse::<u32>().ok(),
        _ => None,
    }
}

/// Get the config directory path
/// On Windows: C:\ProgramData\DawellService (shared between user and SYSTEM service account)
/// On macOS/Linux: user config dir
pub fn get_config_dir() -> PathBuf {
    #[cfg(target_os = "windows")]
    {
        // Use ProgramData so both the installing user AND the SYSTEM service account
        // can read the same config file
        PathBuf::from(r"C:\ProgramData\DawellService")
    }

    #[cfg(target_os = "macos")]
    {
        dirs::config_dir()
            .map(|d| d.join("DawellService"))
            .unwrap_or_else(|| PathBuf::from(".dawellservice"))
    }

    #[cfg(target_os = "linux")]
    {
        dirs::config_dir()
            .map(|d| d.join("dawellservice"))
            .unwrap_or_else(|| PathBuf::from(".dawellservice"))
    }

    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        PathBuf::from(".dawellservice")
    }
}

/// Get the config file path (config.json - but with encrypted content)
fn get_config_path() -> PathBuf {
    get_config_dir().join("config.json")
}

/// Derive encryption key from machine-specific identifiers
/// This ensures the config can only be decrypted on the same machine
fn derive_encryption_key() -> [u8; 32] {
    // Get machine ID and MAC address as seed
    let machine_id = crate::system_info::get_machine_id();
    let mac_address = crate::system_info::get_mac_address();

    // Combine and hash
    let seed = format!("{}|{}", machine_id, mac_address);
    let hash = Sha256::digest(seed.as_bytes());

    let mut key = [0u8; 32];
    key.copy_from_slice(&hash);
    key
}

/// Encrypt config data using AES-256-GCM
/// Returns base64-encoded ciphertext (includes nonce)
fn encrypt_config(config: &ServiceConfig) -> Result<String, String> {
    let key = derive_encryption_key();
    let cipher = Aes256Gcm::new(&key.into());
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    let json = serde_json::to_string(config)
        .map_err(|e| format!("Failed to serialize config: {}", e))?;

    let ciphertext = cipher
        .encrypt(&nonce, json.as_bytes())
        .map_err(|e| format!("Encryption failed: {}", e))?;

    // Combine nonce + ciphertext and encode as base64
    let mut combined = nonce.to_vec();
    combined.extend_from_slice(&ciphertext);
    Ok(base64::engine::general_purpose::STANDARD.encode(combined))
}

/// Decrypt config data from base64-encoded ciphertext
fn decrypt_config(encoded: &str) -> Result<ServiceConfig, String> {
    let key = derive_encryption_key();
    let cipher = Aes256Gcm::new(&key.into());

    let combined = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .map_err(|e| format!("Failed to decode config: {}", e))?;

    if combined.len() < 12 {
        // AES-GCM nonce is 12 bytes
        return Err("Invalid config file: too short".to_string());
    }

    // Split nonce and ciphertext
    let (nonce_bytes, ciphertext) = combined.split_at(12);
    let nonce = Nonce::from_slice(nonce_bytes);

    let plaintext = cipher
        .decrypt(nonce, ciphertext)
        .map_err(|_| "Decryption failed (wrong machine or corrupted file)".to_string())?;

    let json_str = String::from_utf8(plaintext)
        .map_err(|e| format!("Invalid UTF-8 in decrypted data: {}", e))?;

    serde_json::from_str(&json_str)
        .map_err(|e| format!("Failed to parse decrypted config: {}", e))
}

/// Save config to disk (encrypted in config.json)
pub fn save_config(config: &ServiceConfig) -> Result<(), String> {
    let config_dir = get_config_dir();
    fs::create_dir_all(&config_dir)
        .map_err(|e| format!("Failed to create config directory: {}", e))?;

    // Encrypt the config
    let encrypted = encrypt_config(config)?;

    // Save to config.json (encrypted content - user can't read directly)
    fs::write(get_config_path(), &encrypted)
        .map_err(|e| format!("Failed to write config file: {}", e))?;

    Ok(())
}

/// Load config from disk (decrypt from config.json)
pub fn load_config() -> Option<ServiceConfig> {
    let path = get_config_path();
    let encrypted_content = fs::read_to_string(path).ok()?;

    decrypt_config(&encrypted_content).ok()
}

/// Delete config from disk (config.json)
pub fn delete_config() -> Result<(), String> {
    let path = get_config_path();
    if path.exists() {
        fs::remove_file(&path)
            .map_err(|e| format!("Failed to delete config file: {}", e))?;
    }

    // Also delete old service.txt if exists
    let old_service_txt = get_config_dir().join("service.txt");
    if old_service_txt.exists() {
        let _ = fs::remove_file(&old_service_txt);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_decode_token_string_values() {
        // {"user_id":"3","organization_id":"1"}
        let token = "eyJ1c2VyX2lkIjoiMyIsIm9yZ2FuaXphdGlvbl9pZCI6IjEifQ==";
        let (uid, oid) = decode_token(token).unwrap();
        assert_eq!(uid, 3);
        assert_eq!(oid, 1);
    }

    #[test]
    fn test_decode_token_number_values() {
        // {"user_id":3,"organization_id":1}
        let token = base64::engine::general_purpose::STANDARD
            .encode(r#"{"user_id":3,"organization_id":1}"#);
        let (uid, oid) = decode_token(&token).unwrap();
        assert_eq!(uid, 3);
        assert_eq!(oid, 1);
    }

    #[test]
    fn test_encryption_decryption() {
        let config = ServiceConfig {
            user_id: 123,
            organization_id: 456,
            api_base_url: "https://example.com".to_string(),
        };

        // Encrypt
        let encrypted = encrypt_config(&config).unwrap();
        assert!(!encrypted.is_empty());

        // Decrypt
        let decrypted = decrypt_config(&encrypted).unwrap();
        assert_eq!(decrypted.user_id, config.user_id);
        assert_eq!(decrypted.organization_id, config.organization_id);
        assert_eq!(decrypted.api_base_url, config.api_base_url);
    }
}
