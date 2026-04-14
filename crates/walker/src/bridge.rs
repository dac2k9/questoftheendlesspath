//! Walker Bridge — connects to walker.akerud.se WebSocket and forwards
//! walking data to the local game server at localhost:3001.
//!
//! Usage: cargo run --bin walker-bridge
//!
//! Env vars (from .env):
//!   PLAYER_ID — game player UUID (default: a0000000-...)
//!   WALKER_USER_ID — walker.akerud.se user UUID
//!   WALKER_COOKIE — authentication cookie value

use std::time::Instant;

use anyhow::{Context, Result};
use futures::StreamExt;
use tokio_tungstenite::tungstenite;
use tracing::{error, info, warn};

const GAME_SERVER: &str = "http://localhost:3001/walker_update";

#[derive(serde::Deserialize)]
struct WsMessage {
    segment: Option<Segment>,
}

#[derive(serde::Deserialize)]
struct Segment {
    moving: bool,
    speed_kmh: f32,
    distance_m: f32,
    duration_s: f32,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Install rustls crypto provider before any TLS usage
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "walker_bridge=info".parse().expect("valid filter")),
        )
        .init();

    dotenvy::dotenv().ok();

    let player_id = std::env::var("PLAYER_ID")
        .unwrap_or_else(|_| "a0000000-0000-0000-0000-000000000001".to_string());
    let walker_user_id = std::env::var("WALKER_USER_ID")
        .context("WALKER_USER_ID not set — set it in .env")?;
    let walker_cookie = format!("walker_id={}", walker_user_id);

    info!("Walker Bridge starting");
    info!("  Game player: {}", player_id);
    info!("  Walker user: {}", walker_user_id);
    info!("  Game server: {}", GAME_SERVER);

    loop {
        if let Err(e) = run_bridge(&player_id, &walker_user_id, &walker_cookie).await {
            error!("Bridge error: {:#}. Reconnecting in 5s...", e);
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        }
    }
}

async fn run_bridge(player_id: &str, walker_user_id: &str, cookie: &str) -> Result<()> {
    let url = format!("wss://walker.akerud.se/ws/live/{}", walker_user_id);
    info!("Connecting to {}", url);

    let request = tungstenite::http::Request::builder()
        .uri(&url)
        .header("Cookie", cookie)
        .header("Host", "walker.akerud.se")
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", tungstenite::handshake::client::generate_key())
        .body(())?;

    let (ws_stream, _) = tokio_tungstenite::connect_async(request).await?;
    info!("Connected to Walker WebSocket");

    let (_, mut read) = ws_stream.split();
    let mut last_distance: Option<f32> = None;
    let mut last_update = Instant::now();
    let http = reqwest::Client::new();

    while let Some(msg) = read.next().await {
        let msg = msg?;
        let tungstenite::Message::Text(text) = msg else { continue };

        let Ok(data) = serde_json::from_str::<WsMessage>(&text) else {
            warn!("Failed to parse: {}", &text[..text.len().min(100)]);
            continue;
        };

        let Some(seg) = &data.segment else {
            // Segment closed (belt stopped) — send idle update
            info!("STOPPED | Segment closed");
            last_distance = None;
            let body = serde_json::json!({
                "player_id": player_id,
                "speed": 0.0,
                "distance": 0.0,
                "steps": 0,
                "actually_walking": false,
            });
            let _ = http.post(GAME_SERVER).json(&body).send().await;
            continue;
        };

        // Compute distance delta since last message
        let distance_delta = match last_distance {
            Some(prev) => (seg.distance_m - prev).max(0.0) as f64,
            None => 0.0,
        };
        last_distance = Some(seg.distance_m);

        // Rate limit: send at most every 2 seconds
        if last_update.elapsed().as_secs_f32() < 2.0 && distance_delta < 1.0 {
            continue;
        }
        last_update = Instant::now();

        let speed = if seg.moving { seg.speed_kmh } else { 0.0 };
        let walking_str = if seg.moving { "WALKING" } else { "IDLE" };
        info!(
            "{} | Speed: {:.1} km/h | Delta: {:.1}m | Total: {:.0}m | {:.0}s",
            walking_str, speed, distance_delta, seg.distance_m, seg.duration_s
        );

        let body = serde_json::json!({
            "player_id": player_id,
            "speed": speed,
            "distance": distance_delta,
            "steps": 0,
            "actually_walking": seg.moving,
        });

        if let Err(e) = http
            .post(GAME_SERVER)
            .json(&body)
            .send()
            .await
        {
            warn!("Failed to send update to game server: {}", e);
        }
    }

    Err(anyhow::anyhow!("WebSocket stream ended"))
}
