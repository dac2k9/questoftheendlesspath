use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use questlib::ftms::parse_treadmill_data;
use tokio::time;
use tracing::{error, info, warn};
use uuid::Uuid;

/// FTMS Treadmill Data characteristic
const TREADMILL_DATA: Uuid = Uuid::from_u128(0x00002acd_0000_1000_8000_00805f9b34fb);

/// UREVO proprietary characteristics
const UREVO_NOTIFY: Uuid = Uuid::from_u128(0x0000_fff1_0000_1000_8000_0080_5f9b_34fb);
const UREVO_WRITE: Uuid = Uuid::from_u128(0x0000_fff2_0000_1000_8000_0080_5f9b_34fb);
const UREVO_ACTIVATE_CMD: &[u8] = &[0x02, 0x51, 0x0B, 0x03];

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "walker=info".parse().expect("valid filter")),
        )
        .init();

    dotenvy::dotenv().ok();

    let player_id = std::env::var("PLAYER_ID").context("PLAYER_ID not set")?;
    let device_name = std::env::var("DEVICE_NAME").unwrap_or_else(|_| "URTM051".to_string());

    info!("Walker starting for player {player_id}, looking for device {device_name}");

    loop {
        match run_session(&player_id, &device_name).await {
            Ok(()) => info!("Session ended"),
            Err(e) => error!("Session error: {e:#}"),
        }

        // Mark as not walking
        send_update(&player_id, 0.0, 0, 0, false).await;

        info!("Reconnecting in 5 seconds...");
        time::sleep(Duration::from_secs(5)).await;
    }
}

/// Tracks steps from UREVO proprietary protocol.
struct StepTracker {
    prev_raw: Option<u16>,
    total: u64,
}

impl StepTracker {
    fn new() -> Self { Self { prev_raw: None, total: 0 } }

    fn update(&mut self, raw_steps: u16) -> u64 {
        match self.prev_raw {
            Some(prev) if raw_steps >= prev => {
                self.total += (raw_steps - prev) as u64;
            }
            Some(prev) => {
                // Wrap: assume 10000 counter
                self.total += 10000 - prev as u64 + raw_steps as u64;
                warn!("Step counter wrapped: {} → {}", prev, raw_steps);
            }
            None => {
                // First reading — don't count as steps
                info!("Step baseline: {}", raw_steps);
            }
        }
        self.prev_raw = Some(raw_steps);
        self.total
    }
}

/// Detects if user is actually walking (steps increasing) vs belt just running.
struct ActivityTracker {
    last_step_total: u64,
    last_step_time: Option<Instant>,
    moving: bool,
    has_step_data: bool, // true once we've seen any step increase
}

const IDLE_TIMEOUT_SECS: f64 = 5.0;

impl ActivityTracker {
    fn new() -> Self {
        Self { last_step_total: 0, last_step_time: None, moving: false, has_step_data: false }
    }

    fn update(&mut self, total_steps: u64, treadmill_running: bool, has_urevo: bool) -> bool {
        let now = Instant::now();

        if total_steps > self.last_step_total {
            self.moving = true;
            self.has_step_data = true;
            self.last_step_time = Some(now);
            self.last_step_total = total_steps;
        } else if self.has_step_data || has_urevo {
            // We have step tracking — if no steps for timeout, mark idle
            if let Some(last) = self.last_step_time {
                if now.duration_since(last).as_secs_f64() >= IDLE_TIMEOUT_SECS {
                    if self.moving {
                        info!("No steps for {:.0}s — idle", IDLE_TIMEOUT_SECS);
                    }
                    self.moving = false;
                }
            } else {
                // UREVO active but no steps ever — belt running without walking
                self.moving = false;
            }
        } else {
            // No step data at all (FTMS-only device) — fall back to belt status
            self.moving = treadmill_running;
        }

        self.moving
    }
}

async fn run_session(player_id: &str, device_name: &str) -> Result<()> {
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let adapter = adapters.into_iter().next().context("no Bluetooth adapter")?;

    let peripheral = find_device(&adapter, device_name).await?;
    info!("Connecting to {}...", device_name);
    peripheral.connect().await?;
    peripheral.discover_services().await?;
    info!("Connected and services discovered");

    // Subscribe to all notify characteristics
    let chars = peripheral.characteristics();
    let mut has_ftms = false;
    let mut has_urevo = false;

    for ch in &chars {
        if ch.uuid == TREADMILL_DATA {
            peripheral.subscribe(ch).await?;
            has_ftms = true;
            info!("Subscribed to FTMS Treadmill Data");
        }
        if ch.uuid == UREVO_NOTIFY {
            peripheral.subscribe(ch).await?;
            has_urevo = true;
            info!("Subscribed to UREVO proprietary notifications");
        }
    }

    // Activate UREVO data stream
    if has_urevo {
        if let Some(write_ch) = chars.iter().find(|c| c.uuid == UREVO_WRITE) {
            info!("Activating UREVO data stream...");
            peripheral.write(write_ch, UREVO_ACTIVATE_CMD, WriteType::WithoutResponse).await?;
            info!("UREVO data stream activated");
        }
    }

    if !has_ftms && !has_urevo {
        anyhow::bail!("No supported characteristics found (need FTMS or UREVO)");
    }

    let mut notification_stream = peripheral.notifications().await?;
    let mut last_write = Instant::now();
    let mut last_sent_distance: Option<i32> = None;
    let mut step_tracker = StepTracker::new();
    let mut activity = ActivityTracker::new();

    // Track best data from either source
    let mut current_speed_kmh: f32 = 0.0;
    let mut current_steps: u64 = 0;
    let mut treadmill_running = false;

    use futures::StreamExt;
    while let Some(notification) = notification_stream.next().await {
        // Parse FTMS data
        if notification.uuid == TREADMILL_DATA {
            if let Some(data) = parse_treadmill_data(&notification.value) {
                current_speed_kmh = data.speed_kmh;
                treadmill_running = data.speed_kmh > 0.1;
            }
        }

        // Parse UREVO proprietary data
        if notification.uuid == UREVO_NOTIFY {
            let d = &notification.value;
            if d.len() == 19 && d[0] == 0x02 && d[1] == 0x51 {
                let speed_mph = d[3] as f32 * 0.1;
                current_speed_kmh = speed_mph * 1.60934;
                let raw_steps = u16::from_le_bytes([d[11], d[12]]);
                current_steps = step_tracker.update(raw_steps);
                treadmill_running = d[2] == 0x03; // Running status
            }
        }

        // Rate-limit writes to every 2 seconds
        if last_write.elapsed() < Duration::from_millis(2000) {
            continue;
        }
        last_write = Instant::now();

        // Check if user is actually walking (steps increasing)
        let actually_walking = activity.update(current_steps, treadmill_running, has_urevo);

        // Distance from FTMS
        let raw_distance = parse_treadmill_data(&notification.value)
            .and_then(|d| d.total_distance_m)
            .map(|d| d as i32)
            .unwrap_or(0);

        // First reading — set baseline
        if last_sent_distance.is_none() {
            last_sent_distance = Some(raw_distance);
            info!("Distance baseline: {}m | Steps: {} | UREVO: {}", raw_distance, current_steps, has_urevo);
            continue;
        }

        // Send delta
        let delta = (raw_distance - last_sent_distance.unwrap_or(0)).max(0);
        last_sent_distance = Some(raw_distance);

        send_update(
            player_id,
            current_speed_kmh,
            delta,
            current_steps,
            actually_walking,
        ).await;

        let walking_str = if actually_walking { "WALKING" } else { "IDLE (belt only)" };
        info!(
            "Speed: {:.1} km/h | Delta: {}m | Steps: {} | {}",
            current_speed_kmh, delta, current_steps, walking_str
        );
    }

    Ok(())
}

async fn send_update(player_id: &str, speed: f32, distance_delta: i32, steps: u64, actually_walking: bool) {
    let body = serde_json::json!({
        "player_id": player_id,
        "speed": speed,
        "distance": distance_delta,
        "steps": steps,
        "actually_walking": actually_walking,
    });

    let client = reqwest::Client::new();
    let _ = client
        .post("http://localhost:3001/walker_update")
        .header("Content-Type", "application/json")
        .body(body.to_string())
        .send()
        .await;
}

async fn find_device(adapter: &Adapter, name: &str) -> Result<Peripheral> {
    info!("Scanning for BLE devices...");
    adapter.start_scan(ScanFilter::default()).await?;
    time::sleep(Duration::from_secs(5)).await;

    let peripherals = adapter.peripherals().await?;
    for p in &peripherals {
        if let Some(props) = p.properties().await? {
            if let Some(ref local_name) = props.local_name {
                if local_name.contains(name) {
                    info!("Found device: {local_name}");
                    adapter.stop_scan().await?;
                    return Ok(p.clone());
                }
            }
        }
    }

    adapter.stop_scan().await?;
    anyhow::bail!("Device '{name}' not found")
}
