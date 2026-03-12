use log::{info, warn};
use reqwest::Client;
use serde::Serialize;
use serde_json::json;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::time::sleep;

#[derive(Debug, Clone, Serialize)]
pub struct ActivityEntry {
    pub activity_type: String,
    pub application: Option<String>,
    pub active_window: Option<String>,
    pub mouse_clicks: Option<u32>,
    pub mouse_distance: Option<u64>,
    pub idle_seconds: Option<u64>,
    pub time_spent_seconds: Option<u64>,
    pub captured_at: u64,
}

pub struct ActivityTracker {
    client: Client,
    api_base_url: String,
    macaddress: String,
    organization_id: u32,
    activity_tracking: bool,
    app_usage: bool,
}

impl ActivityTracker {
    pub fn new(
        client: Client,
        api_base_url: String,
        macaddress: String,
        organization_id: u32,
        activity_tracking: bool,
        app_usage: bool,
    ) -> Self {
        Self {
            client,
            api_base_url,
            macaddress,
            organization_id,
            activity_tracking,
            app_usage,
        }
    }

    pub async fn run(self) {
        let activities: Arc<Mutex<Vec<ActivityEntry>>> = Arc::new(Mutex::new(Vec::new()));
        let acts_clone = activities.clone();
        let activity_tracking = self.activity_tracking;
        let app_usage = self.app_usage;

        // Background polling thread
        std::thread::spawn(move || {
            #[cfg(target_os = "windows")]
            {
                let mut last_window = String::new();
                let mut last_app = String::new();
                let mut window_start = SystemTime::now();
                let mut last_mouse = (0i32, 0i32);
                let mut mouse_dist: u64 = 0;
                let mut mouse_clicks: u32 = 0;
                let mut left_was_down = false;
                let mut last_active = SystemTime::now();
                let mut idle_secs: u64 = 0;

                loop {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;

                    let (window_title, app_name) = crate::input_logger::get_foreground_info();
                    let mouse_pos = get_cursor_pos();
                    let left_down = is_left_button_down();

                    // Track mouse movement
                    let dx = (mouse_pos.0 - last_mouse.0).abs() as u64;
                    let dy = (mouse_pos.1 - last_mouse.1).abs() as u64;
                    let dist = ((dx * dx + dy * dy) as f64).sqrt() as u64;
                    if dist > 2 {
                        mouse_dist += dist;
                        last_mouse = mouse_pos;
                        last_active = SystemTime::now();
                        idle_secs = 0;
                    }

                    // Track mouse clicks
                    if left_down && !left_was_down {
                        mouse_clicks += 1;
                        last_active = SystemTime::now();
                        idle_secs = 0;
                    }
                    left_was_down = left_down;

                    // Update idle time
                    if let Ok(elapsed) = last_active.elapsed() {
                        idle_secs = elapsed.as_secs();
                    }

                    // When foreground window changes — log previous window's activity
                    let window_changed =
                        !window_title.is_empty() && window_title != last_window;

                    if window_changed && !last_window.is_empty() && (activity_tracking || app_usage) {
                        let time_spent = window_start.elapsed().unwrap_or_default().as_secs();

                        if time_spent >= 2 {
                            if let Ok(mut acts) = acts_clone.lock() {
                                acts.push(ActivityEntry {
                                    activity_type: "app_usage".to_string(),
                                    application: Some(last_app.clone()),
                                    active_window: Some(last_window.clone()),
                                    mouse_clicks: Some(mouse_clicks),
                                    mouse_distance: Some(mouse_dist),
                                    idle_seconds: if idle_secs > 5 { Some(idle_secs) } else { None },
                                    time_spent_seconds: Some(time_spent),
                                    captured_at: now_ms,
                                });
                            }
                        }

                        // Reset for new window
                        mouse_clicks = 0;
                        mouse_dist = 0;
                        window_start = SystemTime::now();
                    }

                    if window_changed || last_window.is_empty() {
                        last_window = window_title;
                        last_app = app_name;
                    }

                    std::thread::sleep(Duration::from_millis(1000));
                }
            }

            #[cfg(any(target_os = "macos", target_os = "linux"))]
            {
                // Mouse state comes from the shared rdev listener in input_logger.
                // Window info is polled via osascript every second.
                let mut last_window = String::new();
                let mut last_app = String::new();
                let mut window_start = SystemTime::now();
                let mut last_mouse = (0i32, 0i32);
                let mut mouse_dist: u64 = 0;
                let mut mouse_clicks: u32 = 0;
                let mut last_active = SystemTime::now();
                let mut idle_secs: u64 = 0;
                let mut left_was_down = false;

                loop {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;

                    let (window_title, app_name) = crate::input_logger::get_foreground_info();
                    let mouse_pos = get_cursor_pos();
                    let left_down = is_left_button_down();

                    // Track mouse movement
                    let dx = (mouse_pos.0 - last_mouse.0).abs() as u64;
                    let dy = (mouse_pos.1 - last_mouse.1).abs() as u64;
                    let dist = ((dx * dx + dy * dy) as f64).sqrt() as u64;
                    if dist > 2 {
                        mouse_dist += dist;
                        last_mouse = mouse_pos;
                        last_active = SystemTime::now();
                        idle_secs = 0;
                    }

                    // Track mouse clicks
                    if left_down && !left_was_down {
                        mouse_clicks += 1;
                        last_active = SystemTime::now();
                        idle_secs = 0;
                    }
                    left_was_down = left_down;

                    if let Ok(elapsed) = last_active.elapsed() {
                        idle_secs = elapsed.as_secs();
                    }

                    let window_changed =
                        !window_title.is_empty() && window_title != last_window;

                    if window_changed && !last_window.is_empty() && (activity_tracking || app_usage)
                    {
                        let time_spent = window_start.elapsed().unwrap_or_default().as_secs();
                        if time_spent >= 2 {
                            if let Ok(mut acts) = acts_clone.lock() {
                                acts.push(ActivityEntry {
                                    activity_type: "app_usage".to_string(),
                                    application: Some(last_app.clone()),
                                    active_window: Some(last_window.clone()),
                                    mouse_clicks: Some(mouse_clicks),
                                    mouse_distance: Some(mouse_dist),
                                    idle_seconds: if idle_secs > 5 {
                                        Some(idle_secs)
                                    } else {
                                        None
                                    },
                                    time_spent_seconds: Some(time_spent),
                                    captured_at: now_ms,
                                });
                            }
                        }
                        mouse_clicks = 0;
                        mouse_dist = 0;
                        window_start = SystemTime::now();
                    }

                    if window_changed || last_window.is_empty() {
                        last_window = window_title;
                        last_app = app_name;
                    }

                    std::thread::sleep(Duration::from_millis(1000));
                }
            }

            #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
            {
                loop {
                    std::thread::sleep(Duration::from_secs(60));
                }
            }
        });

        info!(
            "Activity tracker started (activity={}, app_usage={})",
            activity_tracking, app_usage
        );

        // Upload loop — every 60 seconds
        loop {
            sleep(Duration::from_secs(60)).await;

            // Re-fetch settings each cycle — picks up admin enable/disable without restart
            let mon_settings = crate::monitoring_settings::fetch_monitoring_settings(
                &self.client, &self.api_base_url, self.organization_id,
            ).await;

            let batch = {
                if let Ok(mut acts) = activities.lock() {
                    std::mem::take(&mut *acts)
                } else {
                    Vec::new()
                }
            };

            // Skip upload if both tracking features are disabled
            if !mon_settings.activity_tracking && !mon_settings.app_usage {
                continue;
            }

            if batch.is_empty() {
                continue;
            }

            let url = format!("{}/activity-logs/upload", self.api_base_url);
            let body = json!({
                "macaddress": self.macaddress,
                "organization_id": self.organization_id,
                "activities": batch,
            });

            let upload_ok = match tokio::time::timeout(
                Duration::from_secs(30),
                self.client.post(&url).json(&body).send(),
            )
            .await
            {
                Ok(Ok(resp)) if resp.status().is_success() => {
                    info!("Activity logs uploaded: {} entries", batch.len());
                    true
                }
                Ok(Ok(resp)) => {
                    warn!("Activity logs upload failed: HTTP {} (will retry next cycle)", resp.status());
                    false
                }
                Ok(Err(e)) => {
                    warn!("Activity logs upload failed: {} (will retry next cycle)", e);
                    false
                }
                Err(_) => {
                    warn!("Activity logs upload timed out (will retry next cycle)");
                    false
                }
            };

            // On failure, put data back to front of queue so next cycle retries
            if !upload_ok {
                if let Ok(mut acts) = activities.lock() {
                    let mut recovered = batch;
                    recovered.extend(acts.drain(..));
                    *acts = recovered;
                }
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn get_cursor_pos() -> (i32, i32) {
    use windows::Win32::Foundation::POINT;
    use windows::Win32::UI::WindowsAndMessaging::GetCursorPos;
    unsafe {
        let mut p = POINT { x: 0, y: 0 };
        let _ = GetCursorPos(&mut p);
        (p.x, p.y)
    }
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn get_cursor_pos() -> (i32, i32) {
    crate::input_logger::unix_state::get_mouse()
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn get_cursor_pos() -> (i32, i32) {
    (0, 0)
}

#[cfg(target_os = "windows")]
fn is_left_button_down() -> bool {
    use windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;
    unsafe { (GetAsyncKeyState(0x01) as u16 & 0x8000) != 0 } // VK_LBUTTON = 0x01
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn is_left_button_down() -> bool {
    crate::input_logger::unix_state::get_left_down()
}

#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
fn is_left_button_down() -> bool {
    false
}
