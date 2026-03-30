//! Simple HTTP dev server that replaces Supabase for local development.
//! Serves game state as JSON and accepts route updates.
//! Runs alongside the Game Master tick loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DevPlayerState {
    pub id: String,
    pub name: String,
    pub current_speed_kmh: f32,
    pub total_distance_m: i32,
    pub is_walking: bool,
    pub map_tile_x: i32,
    pub map_tile_y: i32,
    pub gold: i32,
    pub revealed_tiles: String,
    pub planned_route: String,
    pub route_meters_walked: f64,
}

pub type SharedState = Arc<Mutex<HashMap<String, DevPlayerState>>>;

/// Start the dev HTTP server on port 3001.
pub async fn start_dev_server(state: SharedState) -> Result<()> {
    let listener = TcpListener::bind("127.0.0.1:3001").await?;
    tracing::info!("Dev server listening on http://127.0.0.1:3001");

    loop {
        let (mut stream, _) = listener.accept().await?;
        let state = state.clone();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);

            let (status, body) = handle_request(&request, &state);

            let response = format!(
                "HTTP/1.1 {}\r\n\
                 Content-Type: application/json\r\n\
                 Access-Control-Allow-Origin: *\r\n\
                 Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
                 Access-Control-Allow-Headers: Content-Type\r\n\
                 Content-Length: {}\r\n\
                 \r\n\
                 {}",
                status,
                body.len(),
                body
            );

            let _ = stream.write_all(response.as_bytes()).await;
        });
    }
}

fn handle_request(request: &str, state: &SharedState) -> (&'static str, String) {
    let first_line = request.lines().next().unwrap_or("");

    // CORS preflight
    if first_line.starts_with("OPTIONS") {
        return ("200 OK", "{}".to_string());
    }

    // GET /players — return all player states
    if first_line.starts_with("GET /players") {
        let lock = state.lock().unwrap();
        let players: Vec<&DevPlayerState> = lock.values().collect();
        let json = serde_json::to_string(&players).unwrap_or_default();
        return ("200 OK", json);
    }

    // POST /set_route — set planned route for a player
    // Body: {"player_id": "...", "route": "[[x,y],...]"}
    if first_line.starts_with("POST /set_route") {
        // Extract body after \r\n\r\n
        if let Some(body_start) = request.find("\r\n\r\n") {
            let body = &request[body_start + 4..];
            #[derive(Deserialize)]
            struct RouteReq {
                player_id: String,
                route: String,
            }
            if let Ok(req) = serde_json::from_str::<RouteReq>(body) {
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    player.planned_route = req.route;
                    player.route_meters_walked = 0.0;
                    return ("200 OK", r#"{"ok":true}"#.to_string());
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // POST /walker_update — walker writes treadmill data
    // Body: {"player_id": "...", "speed": 1.5, "distance": 200, "incline": 0.0}
    if first_line.starts_with("POST /walker_update") {
        if let Some(body_start) = request.find("\r\n\r\n") {
            let body = &request[body_start + 4..];
            #[derive(Deserialize)]
            struct WalkerReq {
                player_id: String,
                speed: f32,
                distance: i32,
                #[serde(default)]
                incline: f32,
            }
            if let Ok(req) = serde_json::from_str::<WalkerReq>(body) {
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    player.current_speed_kmh = req.speed;
                    player.total_distance_m = req.distance;
                    player.is_walking = req.speed > 0.1;
                    return ("200 OK", r#"{"ok":true}"#.to_string());
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // POST /heartbeat — mark player browser as open (no-op for dev)
    if first_line.starts_with("POST /heartbeat") {
        return ("200 OK", r#"{"ok":true}"#.to_string());
    }

    ("404 Not Found", r#"{"error":"not found"}"#.to_string())
}
