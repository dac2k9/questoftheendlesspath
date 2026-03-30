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
    #[serde(default)]
    pub debug_walking: bool,
}

pub type SharedState = Arc<Mutex<HashMap<String, DevPlayerState>>>;

use crate::SharedEvents;

/// Start the dev HTTP server on port 3001.
pub type SharedNotifs = Arc<Mutex<Vec<String>>>;

pub async fn start_dev_server(state: SharedState, events: SharedEvents, notifs: SharedNotifs) -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:3001").await?;
    tracing::info!("Dev server listening on http://127.0.0.1:3001");

    loop {
        let (mut stream, _) = listener.accept().await?;
        let state = state.clone();
        let events = events.clone();
        let notifs = notifs.clone();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 16384];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);

            let (status, body) = handle_request(&request, &state, &events, &notifs);

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

fn handle_request(request: &str, state: &SharedState, events: &SharedEvents, notifs: &SharedNotifs) -> (&'static str, String) {
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
                steps: u64,
                #[serde(default)]
                actually_walking: bool,
            }
            if let Ok(req) = serde_json::from_str::<WalkerReq>(body) {
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    // Don't override if debug_walk is active
                    if !player.debug_walking {
                        player.current_speed_kmh = req.speed;
                        player.total_distance_m += req.distance;
                        player.is_walking = req.actually_walking;
                    }
                    return ("200 OK", r#"{"ok":true}"#.to_string());
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // GET /events — all events
    if first_line.starts_with("GET /events/active") {
        let lock = events.lock().unwrap();
        let active: Vec<_> = lock.active_events();
        let json = serde_json::to_string(&active).unwrap_or_default();
        return ("200 OK", json);
    }

    if first_line.starts_with("GET /events") {
        let lock = events.lock().unwrap();
        let json = lock.to_json();
        return ("200 OK", json);
    }

    // POST /events/{id}/complete — mark event as completed
    if first_line.contains("/events/") && first_line.contains("/complete") {
        // Extract event id from URL: POST /events/some_id/complete
        let parts: Vec<&str> = first_line.split('/').collect();
        if parts.len() >= 3 {
            // parts: ["POST ", "events", "some_id", "complete", ...]
            let event_id = parts.iter()
                .position(|&p| p == "events")
                .and_then(|i| parts.get(i + 1))
                .map(|s| s.split_whitespace().next().unwrap_or(s));

            if let Some(event_id) = event_id {
                let mut lock = events.lock().unwrap();
                if let Some(event) = lock.get_mut(event_id) {
                    if event.transition(questlib::events::EventStatus::Completed).is_ok() {
                        return ("200 OK", r#"{"ok":true}"#.to_string());
                    }
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"invalid event"}"#.to_string());
    }

    // GET /notifications — fetch and clear pending notifications
    if first_line.starts_with("GET /notifications") {
        let mut lock = notifs.lock().unwrap();
        let json = serde_json::to_string(&*lock).unwrap_or_default();
        lock.clear();
        return ("200 OK", json);
    }

    // POST /debug_walk — simulate walking at a given speed (for testing)
    // Body: {"player_id": "...", "speed": 3.0}
    if first_line.starts_with("POST /debug_walk") {
        if let Some(body_start) = request.find("\r\n\r\n") {
            let body = &request[body_start + 4..];
            #[derive(Deserialize)]
            struct DebugReq { player_id: String, speed: f32 }
            if let Ok(req) = serde_json::from_str::<DebugReq>(body) {
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    if req.speed <= 0.0 {
                        // Stop debug walking
                        player.debug_walking = false;
                        player.is_walking = false;
                        player.current_speed_kmh = 0.0;
                        return ("200 OK", r#"{"ok":true,"stopped":true}"#.to_string());
                    }
                    // Speed in m/s * tick interval (3s) * 5x multiplier for faster testing
                    let delta = (req.speed / 3.6 * 3.0 * 5.0) as i32;
                    player.current_speed_kmh = req.speed;
                    player.total_distance_m += delta;
                    player.is_walking = true;
                    player.debug_walking = true;
                    return ("200 OK", format!("{{\"ok\":true,\"delta\":{}}}", delta));
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
