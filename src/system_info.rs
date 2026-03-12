use serde::{Deserialize, Serialize};
use sysinfo::System;
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemInfo {
    pub machineid: String,
    pub macaddress: String,
    /// All physical adapter MACs (Ethernet + Wi-Fi). macaddress is always index 0.
    #[serde(default)]
    pub mac_addresses: Vec<String>,
    pub ipaddress: String,
    pub hostname: String,
    pub operatingsystem: String,
    pub os_version: String,
    pub cpu_model: String,
    pub cpu_core: u32,
    pub totalram: String,
    pub screenresolution: String,
}

impl SystemInfo {
    pub fn collect() -> Self {
        let mut sys = System::new_all();
        sys.refresh_all();

        // Give sysinfo time to collect data
        std::thread::sleep(std::time::Duration::from_millis(500));
        sys.refresh_all();

        let mac_addresses = get_all_mac_addresses();
        let macaddress = mac_addresses.first().cloned().unwrap_or_else(get_mac_address);

        SystemInfo {
            machineid: get_machine_id(),
            macaddress,
            mac_addresses,
            ipaddress: get_ip_address(),
            hostname: get_hostname(),
            operatingsystem: get_os_name(),
            os_version: get_os_version(),
            cpu_model: get_cpu_model(&sys),
            cpu_core: get_cpu_cores(&sys),
            totalram: get_total_ram(&sys),
            screenresolution: get_screen_resolution(),
        }
    }
}

pub fn get_machine_id() -> String {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        if let Ok(output) = Command::new("wmic")
            .args(["csproduct", "get", "UUID"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let result = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = result.lines().collect();
            if lines.len() > 1 {
                let uuid = lines[1].trim();
                if !uuid.is_empty() && uuid != "UUID" {
                    return uuid.to_string();
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(id) = fs::read_to_string("/etc/machine-id") {
            return id.trim().to_string();
        }
        if let Ok(id) = fs::read_to_string("/var/lib/dbus/machine-id") {
            return id.trim().to_string();
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("ioreg")
            .args(["-rd1", "-c", "IOPlatformExpertDevice"])
            .output()
        {
            let result = String::from_utf8_lossy(&output.stdout);
            for line in result.lines() {
                if line.contains("IOPlatformUUID") {
                    if let Some(uuid) = line.split('"').nth(3) {
                        return uuid.to_string();
                    }
                }
            }
        }
    }

    // Fallback: generate and store a UUID in ProgramData (shared between all accounts)
    #[cfg(target_os = "windows")]
    let machine_id_dir = PathBuf::from(r"C:\ProgramData\DawellService");

    #[cfg(not(target_os = "windows"))]
    let machine_id_dir = get_app_data_dir();

    let machine_id_file = machine_id_dir.join("machine_id");

    if let Ok(id) = fs::read_to_string(&machine_id_file) {
        return id.trim().to_string();
    }

    let new_id = uuid::Uuid::new_v4().to_string();
    let _ = fs::create_dir_all(&machine_id_dir);
    let _ = fs::write(&machine_id_file, &new_id);
    new_id
}

#[cfg(not(target_os = "windows"))]
fn get_app_data_dir() -> PathBuf {
    #[cfg(target_os = "macos")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join("Library/Application Support/DawellService");
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(home) = std::env::var("HOME") {
            return PathBuf::from(home).join(".config/dawellservice");
        }
    }

    PathBuf::from(".dawellservice")
}

pub fn get_mac_address() -> String {
    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        let ps_command = r#"
            Get-NetAdapter | Where-Object {
                $_.Status -eq 'Up' -and
                $_.Virtual -eq $false -and
                $_.MacAddress -ne $null -and
                $_.MacAddress -ne '00-00-00-00-00-00'
            } | Sort-Object -Property @{
                Expression = {
                    switch -Wildcard ($_.InterfaceDescription) {
                        '*Ethernet*' { 1 }
                        '*Realtek*' { 2 }
                        '*Intel*Ethernet*' { 2 }
                        '*Wi-Fi*' { 3 }
                        '*Wireless*' { 3 }
                        '*WiFi*' { 3 }
                        default { 4 }
                    }
                }
            } | Select-Object -First 1 -ExpandProperty MacAddress
        "#;

        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-Command", ps_command])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !result.is_empty() && result.contains('-') {
                return result.replace('-', ":").to_uppercase();
            }
        }

        // Fallback to getmac command
        if let Ok(output) = Command::new("getmac")
            .args(["/fo", "csv", "/v", "/nh"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let result = String::from_utf8_lossy(&output.stdout);
            let mut ethernet_macs: Vec<String> = Vec::new();
            let mut wifi_macs: Vec<String> = Vec::new();
            let mut other_macs: Vec<String> = Vec::new();

            for line in result.lines() {
                let parts: Vec<&str> = line.split(',').collect();
                if parts.len() >= 4 {
                    let adapter_name = parts[0].trim().trim_matches('"').to_lowercase();
                    let mac = parts[2].trim().trim_matches('"');
                    let transport = parts[3].trim().trim_matches('"').to_lowercase();

                    if (mac.contains(':') || mac.contains('-'))
                        && !transport.contains("virtual")
                        && !transport.contains("vpn")
                        && !transport.contains("loopback")
                        && !transport.contains("bluetooth")
                        && !adapter_name.contains("virtual")
                        && !adapter_name.contains("vmware")
                        && !adapter_name.contains("hyper-v")
                    {
                        let normalized_mac = mac.replace('-', ":").to_uppercase();
                        if !normalized_mac.starts_with("00:00:00") {
                            if adapter_name.contains("ethernet") || adapter_name.contains("realtek") {
                                ethernet_macs.push(normalized_mac);
                            } else if adapter_name.contains("wi-fi")
                                || adapter_name.contains("wireless")
                                || adapter_name.contains("wifi")
                            {
                                wifi_macs.push(normalized_mac);
                            } else {
                                other_macs.push(normalized_mac);
                            }
                        }
                    }
                }
            }

            ethernet_macs.sort();
            wifi_macs.sort();
            other_macs.sort();

            if let Some(mac) = ethernet_macs.first() {
                return mac.clone();
            }
            if let Some(mac) = wifi_macs.first() {
                return mac.clone();
            }
            if let Some(mac) = other_macs.first() {
                return mac.clone();
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if let Ok(output) = Command::new("networksetup")
            .args(["-getmacaddress", "en0"])
            .output()
        {
            let result = String::from_utf8_lossy(&output.stdout);
            if let Some(mac) = result.split_whitespace().find(|s| s.matches(':').count() == 5) {
                return mac.to_uppercase();
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = fs::read_dir("/sys/class/net") {
            let mut interfaces: Vec<(String, String)> = Vec::new();

            for entry in entries.flatten() {
                let iface_name = entry.file_name().to_string_lossy().to_string();

                if iface_name == "lo"
                    || iface_name.starts_with("veth")
                    || iface_name.starts_with("docker")
                    || iface_name.starts_with("br-")
                {
                    continue;
                }

                let mac_path = entry.path().join("address");
                if let Ok(mac) = fs::read_to_string(&mac_path) {
                    let mac = mac.trim().to_uppercase();
                    if !mac.is_empty() && mac != "00:00:00:00:00:00" {
                        interfaces.push((iface_name, mac));
                    }
                }
            }

            interfaces.sort_by(|a, b| {
                let priority_a = if a.0.starts_with("eth") || a.0.starts_with("enp") { 0 } else { 1 };
                let priority_b = if b.0.starts_with("eth") || b.0.starts_with("enp") { 0 } else { 1 };
                priority_a.cmp(&priority_b)
            });

            if let Some((_, mac)) = interfaces.first() {
                return mac.clone();
            }
        }
    }

    // Fallback to mac_address crate
    match mac_address::get_mac_address() {
        Ok(Some(ma)) => ma.to_string().to_uppercase(),
        _ => "00:00:00:00:00:00".to_string(),
    }
}

/// Collect ALL physical adapter MAC addresses (Ethernet + Wi-Fi + others).
/// The primary MAC (highest priority adapter) is placed at index 0.
/// This ensures the relay can find the desktop no matter which adapter MAC
/// was previously stored in the database.
pub fn get_all_mac_addresses() -> Vec<String> {
    let mut macs: Vec<String> = Vec::new();

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        // Get ALL active physical adapters (no -First 1 so Ethernet + Wi-Fi are both returned)
        let ps_command = r#"
            Get-NetAdapter | Where-Object {
                $_.Status -eq 'Up' -and
                $_.Virtual -eq $false -and
                $_.MacAddress -ne $null -and
                $_.MacAddress -ne '00-00-00-00-00-00'
            } | Sort-Object -Property @{
                Expression = {
                    switch -Wildcard ($_.InterfaceDescription) {
                        '*Ethernet*'       { 1 }
                        '*Realtek*'        { 2 }
                        '*Intel*Ethernet*' { 2 }
                        '*Wi-Fi*'          { 3 }
                        '*Wireless*'       { 3 }
                        '*WiFi*'           { 3 }
                        default            { 4 }
                    }
                }
            } | Select-Object -ExpandProperty MacAddress
        "#;

        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-Command", ps_command])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let result = String::from_utf8_lossy(&output.stdout);
            for line in result.lines() {
                let mac = line.trim();
                if !mac.is_empty() && (mac.contains('-') || mac.contains(':')) {
                    let normalized = mac.replace('-', ":").to_uppercase();
                    if !normalized.starts_with("00:00:00") && !macs.contains(&normalized) {
                        macs.push(normalized);
                    }
                }
            }
        }

        // Fallback: getmac — collect all non-virtual MACs
        if macs.is_empty() {
            if let Ok(output) = Command::new("getmac")
                .args(["/fo", "csv", "/v", "/nh"])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
            {
                let result = String::from_utf8_lossy(&output.stdout);
                let mut ethernet_macs: Vec<String> = Vec::new();
                let mut wifi_macs: Vec<String> = Vec::new();
                let mut other_macs: Vec<String> = Vec::new();

                for line in result.lines() {
                    let parts: Vec<&str> = line.split(',').collect();
                    if parts.len() >= 4 {
                        let adapter_name = parts[0].trim().trim_matches('"').to_lowercase();
                        let mac = parts[2].trim().trim_matches('"');
                        let transport = parts[3].trim().trim_matches('"').to_lowercase();

                        if (mac.contains(':') || mac.contains('-'))
                            && !transport.contains("virtual")
                            && !transport.contains("vpn")
                            && !transport.contains("loopback")
                            && !transport.contains("bluetooth")
                            && !adapter_name.contains("virtual")
                            && !adapter_name.contains("vmware")
                            && !adapter_name.contains("hyper-v")
                        {
                            let normalized = mac.replace('-', ":").to_uppercase();
                            if !normalized.starts_with("00:00:00") {
                                if adapter_name.contains("ethernet") || adapter_name.contains("realtek") {
                                    if !ethernet_macs.contains(&normalized) { ethernet_macs.push(normalized); }
                                } else if adapter_name.contains("wi-fi") || adapter_name.contains("wireless") || adapter_name.contains("wifi") {
                                    if !wifi_macs.contains(&normalized) { wifi_macs.push(normalized); }
                                } else if !other_macs.contains(&normalized) {
                                    other_macs.push(normalized);
                                }
                            }
                        }
                    }
                }

                macs.extend(ethernet_macs);
                macs.extend(wifi_macs);
                macs.extend(other_macs);
            }
        }
    }

    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        for iface in &["en0", "en1", "en2", "en3"] {
            if let Ok(output) = Command::new("networksetup")
                .args(["-getmacaddress", iface])
                .output()
            {
                let result = String::from_utf8_lossy(&output.stdout);
                if let Some(mac) = result.split_whitespace().find(|s| s.matches(':').count() == 5) {
                    let normalized = mac.to_uppercase();
                    if !normalized.starts_with("00:00:00") && !macs.contains(&normalized) {
                        macs.push(normalized);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = fs::read_dir("/sys/class/net") {
            let mut interfaces: Vec<(String, String)> = Vec::new();
            for entry in entries.flatten() {
                let iface_name = entry.file_name().to_string_lossy().to_string();
                if iface_name == "lo"
                    || iface_name.starts_with("veth")
                    || iface_name.starts_with("docker")
                    || iface_name.starts_with("br-")
                {
                    continue;
                }
                let mac_path = entry.path().join("address");
                if let Ok(mac) = fs::read_to_string(&mac_path) {
                    let mac = mac.trim().to_uppercase();
                    if !mac.is_empty() && mac != "00:00:00:00:00:00" {
                        interfaces.push((iface_name, mac));
                    }
                }
            }
            // Sort: eth/enp first, then wlan/wlp, then rest
            interfaces.sort_by(|a, b| {
                let prio = |n: &str| {
                    if n.starts_with("eth") || n.starts_with("enp") { 0 }
                    else if n.starts_with("wlan") || n.starts_with("wlp") { 1 }
                    else { 2 }
                };
                prio(&a.0).cmp(&prio(&b.0))
            });
            for (_, mac) in interfaces {
                if !macs.contains(&mac) { macs.push(mac); }
            }
        }
    }

    // If nothing found, fall back to the crate
    if macs.is_empty() {
        if let Ok(Some(ma)) = mac_address::get_mac_address() {
            macs.push(ma.to_string().to_uppercase());
        }
    }

    macs
}

fn get_ip_address() -> String {
    match local_ip_address::local_ip() {
        Ok(ip) => ip.to_string(),
        Err(_) => "127.0.0.1".to_string(),
    }
}

fn get_hostname() -> String {
    hostname::get()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

fn get_os_name() -> String {
    System::name().unwrap_or_else(|| "Unknown".to_string())
}

fn get_os_version() -> String {
    // Try sysinfo first
    if let Some(version) = System::os_version() {
        if !version.is_empty() && version != "Unknown" {
            return version;
        }
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        // Get Windows version using PowerShell
        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-Command", "(Get-CimInstance Win32_OperatingSystem).Version"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let version = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !version.is_empty() {
                return version;
            }
        }
    }

    "Unknown".to_string()
}

fn get_cpu_model(sys: &System) -> String {
    // Try sysinfo first
    if let Some(cpu) = sys.cpus().first() {
        let brand = cpu.brand().to_string();
        if !brand.is_empty() && brand != "Unknown" {
            return brand;
        }
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        // Get CPU name using PowerShell
        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-Command", "(Get-CimInstance Win32_Processor).Name"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let model = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !model.is_empty() {
                return model;
            }
        }
    }

    "Unknown".to_string()
}

fn get_cpu_cores(sys: &System) -> u32 {
    let cores = sys.cpus().len() as u32;
    if cores > 0 {
        return cores;
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        // Get CPU cores using PowerShell
        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-Command", "(Get-CimInstance Win32_Processor).NumberOfLogicalProcessors"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let cores_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Ok(c) = cores_str.parse::<u32>() {
                return c;
            }
        }
    }

    1 // Default fallback
}

fn get_total_ram(sys: &System) -> String {
    let total_bytes = sys.total_memory();
    if total_bytes > 0 {
        let total_gb = total_bytes as f64 / (1024.0 * 1024.0 * 1024.0);
        return format!("{:.2} GB", total_gb);
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        // Get total RAM using PowerShell
        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-Command", "(Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let ram_str = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if let Ok(bytes) = ram_str.parse::<u64>() {
                let total_gb = bytes as f64 / (1024.0 * 1024.0 * 1024.0);
                return format!("{:.2} GB", total_gb);
            }
        }
    }

    "Unknown".to_string()
}

fn get_screen_resolution() -> String {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if let Ok(out) = Command::new("system_profiler")
            .args(["SPDisplaysDataType"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            for line in text.lines() {
                let trimmed = line.trim();
                if trimmed.starts_with("Resolution:") {
                    // e.g. "Resolution: 2560 x 1600 Retina"
                    let rest = trimmed.trim_start_matches("Resolution:").trim();
                    let parts: Vec<&str> = rest.split_whitespace().collect();
                    if parts.len() >= 3 && parts[1] == "x" {
                        return format!("{}x{}", parts[0], parts[2]);
                    }
                }
            }
        }
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x08000000;

        // Try PowerShell first (more reliable)
        let ps_command = r#"
            Add-Type -AssemblyName System.Windows.Forms
            $screen = [System.Windows.Forms.Screen]::PrimaryScreen.Bounds
            "$($screen.Width)x$($screen.Height)"
        "#;

        if let Ok(output) = Command::new("powershell")
            .args(["-NoProfile", "-Command", ps_command])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let result = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if result.contains('x') && !result.is_empty() {
                return result;
            }
        }

        // Fallback to WMIC
        if let Ok(output) = Command::new("wmic")
            .args(["desktopmonitor", "get", "screenheight,screenwidth"])
            .creation_flags(CREATE_NO_WINDOW)
            .output()
        {
            let result = String::from_utf8_lossy(&output.stdout);
            let lines: Vec<&str> = result.lines().filter(|l| !l.trim().is_empty()).collect();
            if lines.len() > 1 {
                let parts: Vec<&str> = lines[1].split_whitespace().collect();
                if parts.len() >= 2 {
                    return format!("{}x{}", parts[1], parts[0]);
                }
            }
        }
    }

    "1920x1080".to_string()
}
