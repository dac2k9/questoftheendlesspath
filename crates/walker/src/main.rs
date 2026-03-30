use std::time::Duration;

use anyhow::{Context, Result};
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::{Adapter, Manager, Peripheral};
use questlib::ftms::parse_treadmill_data;
use questlib::supabase::SupabaseClient;
use questlib::types::PlayerUpdate;
use tokio::time;
use tracing::{error, info, warn};
use uuid::Uuid;

/// FTMS Service UUID
const FTMS_SERVICE: Uuid = Uuid::from_u128(0x00001826_0000_1000_8000_00805f9b34fb);
/// Treadmill Data characteristic UUID
const TREADMILL_DATA: Uuid = Uuid::from_u128(0x00002acd_0000_1000_8000_00805f9b34fb);

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "walker=info".parse().expect("valid filter")),
        )
        .init();

    dotenvy::dotenv().ok();

    let supabase = SupabaseClient::from_env()?;
    let player_id = std::env::var("PLAYER_ID").context("PLAYER_ID not set")?;
    let device_name = std::env::var("DEVICE_NAME").unwrap_or_else(|_| "URTM051".to_string());

    info!("Walker starting for player {player_id}, looking for device {device_name}");

    loop {
        match run_session(&supabase, &player_id, &device_name).await {
            Ok(()) => info!("Session ended"),
            Err(e) => error!("Session error: {e:#}"),
        }

        // Mark as not walking
        let update = PlayerUpdate {
            is_walking: Some(false),
            current_speed_kmh: Some(0.0),
            ..Default::default()
        };
        if let Err(e) = supabase.upsert_player(&player_id, &update).await {
            warn!("Failed to mark offline: {e}");
        }

        info!("Reconnecting in 5 seconds...");
        time::sleep(Duration::from_secs(5)).await;
    }
}

async fn run_session(
    supabase: &SupabaseClient,
    player_id: &str,
    device_name: &str,
) -> Result<()> {
    let manager = Manager::new().await?;
    let adapters = manager.adapters().await?;
    let adapter = adapters.into_iter().next().context("no Bluetooth adapter")?;

    let peripheral = find_device(&adapter, device_name).await?;
    info!("Connecting to {}...", device_name);
    peripheral.connect().await?;
    peripheral.discover_services().await?;
    info!("Connected and services discovered");

    // Find the treadmill data characteristic
    let chars = peripheral.characteristics();
    let treadmill_char = chars
        .iter()
        .find(|c| c.uuid == TREADMILL_DATA)
        .context("Treadmill Data characteristic not found")?
        .clone();

    // Subscribe to notifications
    peripheral.subscribe(&treadmill_char).await?;
    info!("Subscribed to Treadmill Data notifications");

    let mut notification_stream = peripheral.notifications().await?;
    let mut last_write = tokio::time::Instant::now();
    let mut last_sent_distance: i32 = 0; // track what we last reported

    use futures::StreamExt;
    while let Some(notification) = notification_stream.next().await {
        if notification.uuid != TREADMILL_DATA {
            continue;
        }

        // Log raw bytes once for debugging
        if last_write.elapsed() > Duration::from_secs(5) {
            tracing::debug!("Raw FTMS data ({} bytes): {:02x?}", notification.value.len(), &notification.value);
        }

        let Some(data) = parse_treadmill_data(&notification.value) else {
            continue;
        };

        // Rate-limit writes
        if last_write.elapsed() < Duration::from_millis(2000) {
            continue;
        }
        last_write = tokio::time::Instant::now();

        let raw_distance = data.total_distance_m.map(|d| d as i32).unwrap_or(0);

        // Send delta since last report (not cumulative)
        let delta = (raw_distance - last_sent_distance).max(0);
        last_sent_distance = raw_distance;

        let speed = data.speed_kmh;
        let dist = delta; // server adds this to player's total
        let incline = data.incline_pct.unwrap_or(0.0);

        // Write to dev server
        let dev_url = format!("http://127.0.0.1:3001/walker_update");
        let body = serde_json::json!({
            "player_id": player_id,
            "speed": speed,
            "distance": dist,
            "incline": incline,
        });

        let client = reqwest::Client::new();
        match client
            .post(&dev_url)
            .header("Content-Type", "application/json")
            .body(body.to_string())
            .send()
            .await
        {
            Ok(_) => {
                info!(
                    "Speed: {:.2} km/h | Distance: {}m | Incline: {:.1}%",
                    speed, dist, incline,
                );
            }
            Err(e) => {
                warn!("Failed to write to dev server: {e}");
            }
        }
    }

    Ok(())
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
