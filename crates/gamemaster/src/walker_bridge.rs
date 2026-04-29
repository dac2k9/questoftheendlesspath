//! Integrated Walker bridge — connects to walker.akerud.se WebSocket
//! and feeds walking data into the game server's player state.
//!
//! Spawns one WebSocket connection per player that has a walker_user_id configured.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
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

    let (mut write, mut read) = ws_stream.split();
    let mut last_distance: Option<f32> = None;
    let mut last_update = Instant::now();
    let mut last_movement = Instant::now(); // last time distance actually changed
    // Flip on per-bridge message tracing by setting WALKER_BRIDGE_TRACE=1.
    // Logs every incoming WS message + our computed actually_moving/delta so
    // we can see exactly what the bridge sees vs what we push to state.
    let trace = std::env::var("WALKER_BRIDGE_TRACE").map(|v| v == "1").unwrap_or(false);

    // Keepalive: we PING every PING_INTERVAL and disconnect if no inbound
    // frame (any kind — text, binary, pong, ping) arrives for READ_TIMEOUT.
    // This catches the half-dead WebSocket case (Walker's end gone, our TCP
    // socket still thinks it's alive) instead of blocking forever on
    // `read.next().await`. The retry loop in `ensure_bridge` reconnects.
    const PING_INTERVAL: Duration = Duration::from_secs(30);
    const READ_TIMEOUT: Duration = Duration::from_secs(60);
    let mut ping_interval = tokio::time::interval(PING_INTERVAL);
    // Consume the immediate first tick so we don't ping right after connect.
    ping_interval.tick().await;

    let mut msg_count: u64 = 0;
    let mut push_count: u64 = 0;
    loop {
        let msg_result = tokio::select! {
            biased;
            msg = tokio::time::timeout(READ_TIMEOUT, read.next()) => msg,
            _ = ping_interval.tick() => {
                if let Err(e) = write.send(tungstenite::Message::Ping(Default::default())).await {
                    return Err(anyhow::anyhow!("[{}] ping send failed: {e}", player_id));
                }
                if trace {
                    info!("[Walker bridge {}] sent keepalive ping", player_id);
                }
                continue;
            }
        };

        let msg = match msg_result {
            Err(_elapsed) => {
                // No inbound frames for READ_TIMEOUT. Treat as dead connection
                // so the outer retry loop can reconnect fresh.
                return Err(anyhow::anyhow!(
                    "[{}] no WS frames for {}s — assuming half-dead",
                    player_id, READ_TIMEOUT.as_secs()
                ));
            }
            Ok(None) => return Err(anyhow::anyhow!("[{}] WebSocket stream ended", player_id)),
            Ok(Some(Err(e))) => return Err(anyhow::anyhow!("[{}] WebSocket error: {e}", player_id)),
            Ok(Some(Ok(m))) => m,
        };

        // Non-text frames (ping/pong/close/binary) count as liveness — the
        // select!/timeout resets naturally — but we don't process them for
        // game state. We reply to server pings automatically via tungstenite,
        // so we don't need to do it manually.
        let text = match msg {
            tungstenite::Message::Text(t) => t,
            tungstenite::Message::Close(_) => {
                return Err(anyhow::anyhow!("[{}] server closed WebSocket", player_id));
            }
            _ => continue,
        };
        msg_count += 1;

        let data = match serde_json::from_str::<WsMessage>(&text) {
            Ok(d) => d,
            Err(e) => {
                if trace {
                    info!("[Walker bridge {}] msg #{}: parse fail ({}), raw={}",
                        player_id, msg_count, e, &text[..text.len().min(200)]);
                }
                continue;
            }
        };

        let Some(seg) = &data.segment else {
            // Segment closed (belt stopped)
            info!("[Walker bridge {}] STOPPED (msg #{})", player_id, msg_count);
            last_distance = None;
            if let Ok(mut lock) = state.lock() {
                if let Some(player) = lock.get_mut(player_id) {
                    player.current_speed_kmh = 0.0;
                    player.is_walking = false;
                }
            }
            continue;
        };

        let raw_delta = match last_distance {
            Some(prev) => (seg.distance_m - prev).max(0.0) as f64,
            None => 0.0,
        };
        last_distance = Some(seg.distance_m);
        // Sanity cap: max realistic real-world per-message delta is
        // ~33 m (12 km/h × 10 s message gap). 50 m gives margin
        // without ever rejecting legit data; anything beyond was a
        // glitch in the upstream walker feed (occasionally spikes in
        // tens of km, which inflated player.total_distance_m and
        // jumped levels). Log when we drop a spike so we can tell.
        const MAX_SANE_DELTA_M: f64 = 50.0;
        let distance_delta = if raw_delta > MAX_SANE_DELTA_M {
            tracing::warn!(
                "[Walker bridge {}] dropping spike delta {:.1}m → {:.1}m (msg #{})",
                player_id, raw_delta, MAX_SANE_DELTA_M, msg_count
            );
            MAX_SANE_DELTA_M
        } else {
            raw_delta
        };

        // Rate limit: every 2 seconds
        if last_update.elapsed().as_secs_f32() < 2.0 && distance_delta < 1.0 {
            if trace {
                info!("[Walker bridge {}] msg #{}: rate-limited (elapsed {:.1}s, delta {:.2}m)",
                    player_id, msg_count, last_update.elapsed().as_secs_f32(), distance_delta);
            }
            continue;
        }
        last_update = Instant::now();

        // Track actual movement — if no distance change for 10s, consider idle
        if distance_delta > 0.1 {
            last_movement = Instant::now();
        }
        let actually_moving = seg.moving && last_movement.elapsed().as_secs() < 10;
        let speed = if actually_moving { seg.speed_kmh } else { 0.0 };

        let mut wrote = false;
        if let Ok(mut lock) = state.lock() {
            if let Some(player) = lock.get_mut(player_id) {
                if !player.debug_walking {
                    player.current_speed_kmh = speed;
                    player.total_distance_m += distance_delta;
                    player.is_walking = actually_moving;
                    wrote = true;
                    push_count += 1;
                }
            }
        }

        if trace || push_count <= 3 || push_count % 30 == 0 {
            // Always log the first few pushes so a cold start is observable,
            // plus every 30th push thereafter; full firehose behind the env var.
            info!("[Walker bridge {}] msg #{} push #{}: seg.moving={} seg.dist={:.1}m delta={:.2}m since_move={:.1}s → is_walking={} speed={:.1}km/h (wrote={})",
                player_id, msg_count, push_count, seg.moving, seg.distance_m, distance_delta,
                last_movement.elapsed().as_secs_f32(), actually_moving, speed, wrote);
        }
    }
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
