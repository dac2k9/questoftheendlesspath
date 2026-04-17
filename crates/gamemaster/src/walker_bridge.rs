//! Integrated Walker bridge — connects to walker.akerud.se WebSocket
//! and feeds walking data into the game server's player state.
//!
//! Spawns one WebSocket connection per player that has a walker_user_id configured.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_util::StreamExt;
use tokio_tungstenite::tungstenite;
use tracing::{error, info};

use crate::devserver::{DevPlayerState, SharedState};

/// Tracks which players already have a bridge running.
pub type BridgedPlayers = Arc<Mutex<std::collections::HashSet<String>>>;

/// Walker user ID mapping: game player_id -> walker user_id
pub type WalkerConfig = Arc<HashMap<String, String>>;

#[derive(serde::Deserialize)]
struct WsMessage {
    segment: Option<Segment>,
}

#[derive(serde::Deserialize)]
struct Segment {
    moving: bool,
    speed_kmh: f32,
    distance_m: f32,
}

/// Build the walker config from environment variables.
/// Looks for WALKER_USER_ID (single player) or WALKER_USER_ID_<N> (multi).
pub fn build_config() -> HashMap<String, String> {
    let mut config = HashMap::new();

    // Single player: WALKER_USER_ID maps to PLAYER_ID
    if let Ok(walker_id) = std::env::var("WALKER_USER_ID") {
        let player_id = std::env::var("PLAYER_ID")
            .unwrap_or_else(|_| "a0000000-0000-0000-0000-000000000001".to_string());
        config.insert(player_id, walker_id);
    }

    // Second player: WALKER_USER_ID_2 maps to PLAYER_ID_2
    if let Ok(walker_id) = std::env::var("WALKER_USER_ID_2") {
        let player_id = std::env::var("PLAYER_ID_2")
            .unwrap_or_else(|_| "b0000000-0000-0000-0000-000000000002".to_string());
        config.insert(player_id, walker_id);
    }

    config
}

/// Spawn Walker WebSocket bridges for all configured players.
pub fn spawn_bridges(state: SharedState, config: WalkerConfig) {
    for (player_id, walker_user_id) in config.iter() {
        let state = state.clone();
        let pid = player_id.clone();
        let wid = walker_user_id.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = run_bridge(&state, &pid, &wid).await {
                    error!("[Walker bridge {}] Error: {:#}. Reconnecting in 5s...", pid, e);
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        });
    }
}

async fn run_bridge(state: &SharedState, player_id: &str, walker_user_id: &str) -> anyhow::Result<()> {
    let url = format!("wss://walker.akerud.se/ws/live/{}", walker_user_id);
    let cookie = format!("walker_id={}", walker_user_id);
    info!("[Walker bridge {}] Connecting to {}", player_id, url);

    let request = tungstenite::http::Request::builder()
        .uri(&url)
        .header("Cookie", &cookie)
        .header("Host", "walker.akerud.se")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
        .body(())?;

    let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
    info!("[Walker bridge {}] Connected", player_id);

    let (_, mut read) = ws_stream.split();
    let mut last_distance: Option<f32> = None;
    let mut last_update = Instant::now();
    let mut last_movement = Instant::now(); // last time distance actually changed

    while let Some(msg) = read.next().await {
        let msg = msg?;
        let tungstenite::Message::Text(text) = msg else { continue };

        let Ok(data) = serde_json::from_str::<WsMessage>(&text) else { continue };

        let Some(seg) = &data.segment else {
            // Segment closed (belt stopped)
            info!("[Walker bridge {}] STOPPED", player_id);
            last_distance = None;
            if let Ok(mut lock) = state.lock() {
                if let Some(player) = lock.get_mut(player_id) {
                    player.current_speed_kmh = 0.0;
                    player.is_walking = false;
                }
            }
            continue;
        };

        let distance_delta = match last_distance {
            Some(prev) => (seg.distance_m - prev).max(0.0) as f64,
            None => 0.0,
        };
        last_distance = Some(seg.distance_m);

        // Rate limit: every 2 seconds
        if last_update.elapsed().as_secs_f32() < 2.0 && distance_delta < 1.0 {
            continue;
        }
        last_update = Instant::now();

        // Track actual movement — if no distance change for 10s, consider idle
        if distance_delta > 0.1 {
            last_movement = Instant::now();
        }
        let actually_moving = seg.moving && last_movement.elapsed().as_secs() < 10;
        let speed = if actually_moving { seg.speed_kmh } else { 0.0 };

        if let Ok(mut lock) = state.lock() {
            if let Some(player) = lock.get_mut(player_id) {
                if !player.debug_walking {
                    player.current_speed_kmh = speed;
                    player.total_distance_m += distance_delta;
                    player.is_walking = actually_moving;
                }
            }
        }
    }

    Err(anyhow::anyhow!("WebSocket stream ended"))
}

/// Spawn a bridge for a player if not already running.
pub fn ensure_bridge(state: SharedState, bridged: BridgedPlayers, player_id: &str, walker_user_id: &str) {
    {
        let mut lock = bridged.lock().unwrap();
        if lock.contains(player_id) {
            return; // already running
        }
        lock.insert(player_id.to_string());
    }

    let pid = player_id.to_string();
    let wid = walker_user_id.to_string();
    let bridged_clone = bridged.clone();
    tokio::spawn(async move {
        // Count ONLY rapid successive failures as "bad" — a run that lasted long
        // enough to actually exchange messages is treated as a normal disconnect
        // (Walker periodically closes idle sockets), not a retry-to-give-up event.
        let mut consecutive_bad_attempts = 0u32;
        loop {
            let started = std::time::Instant::now();
            let result = run_bridge(&state, &pid, &wid).await;
            let ran_for = started.elapsed();

            if ran_for > std::time::Duration::from_secs(30) {
                consecutive_bad_attempts = 0;
            } else {
                consecutive_bad_attempts += 1;
            }

            if let Err(e) = result {
                error!("[Walker bridge {}] Disconnected after {:.1}s (#{} short-run): {:#}. Reconnecting in 5s...",
                    pid, ran_for.as_secs_f32(), consecutive_bad_attempts, e);
            }

            // Only give up if we've failed many times back-to-back *without*
            // a successful session in between — i.e., we genuinely can't connect.
            if consecutive_bad_attempts >= 120 {
                error!("[Walker bridge {}] Giving up: {} short runs in a row", pid, consecutive_bad_attempts);
                if let Ok(mut lock) = bridged_clone.lock() {
                    lock.remove(&pid);
                }
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    });
    info!("Walker bridge started for {} -> {}", player_id, walker_user_id);
}
