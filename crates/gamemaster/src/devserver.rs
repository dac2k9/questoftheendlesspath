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
    pub total_distance_m: f64,
    pub is_walking: bool,
    pub map_tile_x: i32,
    pub map_tile_y: i32,
    pub gold: i32,
    pub revealed_tiles: String,
    pub planned_route: String,
    pub route_meters_walked: f64,
    /// Direction the character is facing along the route.
    #[serde(default)]
    pub facing: questlib::route::Facing,
    /// Interpolation target: client should animate toward this meters value.
    #[serde(default)]
    pub interp_meters_target: f64,
    /// How many seconds the client should take to reach the target (matches tick interval).
    #[serde(default)]
    pub interp_duration_secs: f32,
    #[serde(default)]
    pub current_incline: f32,
    #[serde(default)]
    pub debug_walking: bool,
    #[serde(default)]
    pub inventory: Vec<questlib::items::InventorySlot>,
    #[serde(default)]
    pub equipment: questlib::items::EquipmentLoadout,
    #[serde(default)]
    pub opened_chests: Vec<String>,
    #[serde(default)]
    pub defeated_monsters: Vec<String>,
    #[serde(default)]
    pub walker_uuid: Option<String>,
    /// Events this player has personally completed (for per-player quest triggers).
    #[serde(default)]
    pub completed_events: Vec<String>,
    /// Previous tile before entering current tile (for retreat).
    #[serde(default)]
    pub prev_tile: Option<(i32, i32)>,
    /// Character sprite the player chose on the title screen.
    #[serde(default)]
    pub champion: String,
    /// Temporary buffs from consumed potions. Pruned each tick when expired.
    #[serde(default)]
    pub active_buffs: Vec<questlib::items::ActiveBuff>,
    /// Where the player currently is (overworld or an interior).
    #[serde(default)]
    pub location: questlib::interior::Location,
    /// When inside an interior, the overworld tile to drop back to on exit.
    #[serde(default)]
    pub overworld_return: Option<(i32, i32)>,
    /// Fog of war per interior the player has visited. key = interior id.
    #[serde(default)]
    pub interior_fog: std::collections::HashMap<String, String>,
}

pub type SharedState = Arc<Mutex<HashMap<String, DevPlayerState>>>;

use crate::SharedEvents;

/// Start the dev HTTP server on port 3001.
pub type SharedNotifs = Arc<Mutex<HashMap<String, Vec<String>>>>;

/// Tick signal: generation counter + notify. The counter avoids the race where
/// a notification fires between client disconnect and reconnect.
pub struct TickSignal {
    pub generation: std::sync::atomic::AtomicU64,
    pub notify: tokio::sync::Notify,
}

pub type SharedTickSignal = Arc<TickSignal>;

pub fn new_tick_signal() -> SharedTickSignal {
    Arc::new(TickSignal {
        generation: std::sync::atomic::AtomicU64::new(0),
        notify: tokio::sync::Notify::new(),
    })
}

impl TickSignal {
    /// Called after each tick: bump generation, wake all waiters.
    pub fn tick(&self) {
        self.generation.fetch_add(1, std::sync::atomic::Ordering::Release);
        self.notify.notify_waiters();
    }
}

pub async fn start_dev_server(state: SharedState, events: SharedEvents, notifs: SharedNotifs, world: Arc<questlib::mapgen::WorldMap>, combat: crate::combat::SharedCombat, tick_signal: SharedTickSignal, bridged_players: crate::walker_bridge::BridgedPlayers, interiors: crate::interior::SharedInteriors) -> Result<()> {
    let port = std::env::var("PORT").unwrap_or_else(|_| "3001".to_string());
    let listener = TcpListener::bind(format!("0.0.0.0:{}", port)).await?;
    tracing::info!("Dev server listening on http://127.0.0.1:{}", port);

    loop {
        let (mut stream, _) = listener.accept().await?;
        let state = state.clone();
        let events = events.clone();
        let notifs = notifs.clone();
        let world = world.clone();
        let combat = combat.clone();
        let tick_signal = tick_signal.clone();
        let bridged_players = bridged_players.clone();
        let interiors = interiors.clone();

        tokio::spawn(async move {
            // Read headers + body (may arrive in separate TCP packets)
            let mut buf = vec![0u8; 32768];
            let mut total = 0;
            loop {
                let n = stream.read(&mut buf[total..]).await.unwrap_or(0);
                if n == 0 { break; }
                total += n;
                // Check if we have the full request (headers + body)
                let s = String::from_utf8_lossy(&buf[..total]);
                if let Some(header_end) = s.find("\r\n\r\n") {
                    // Parse Content-Length to know if body is complete
                    let content_len: usize = s[..header_end].lines()
                        .find(|l| l.to_lowercase().starts_with("content-length:"))
                        .and_then(|l| l.split(':').nth(1))
                        .and_then(|v| v.trim().parse().ok())
                        .unwrap_or(0);
                    let body_start = header_end + 4;
                    if total >= body_start + content_len { break; }
                }
                if total >= buf.len() { break; }
            }
            let request = String::from_utf8_lossy(&buf[..total]);
            let first_line = request.lines().next().unwrap_or("");

            // Long-poll: GET /players/poll?after=N waits until tick generation > N
            let (status, body) = if first_line.starts_with("GET /players/poll") {
                // Parse ?after=N from the URL
                let client_gen: u64 = first_line
                    .split('?')
                    .nth(1)
                    .and_then(|qs| qs.split('&').find(|p| p.starts_with("after=")))
                    .and_then(|p| p.strip_prefix("after="))
                    .and_then(|v| v.split_whitespace().next()) // trim " HTTP/1.1"
                    .and_then(|v| v.parse().ok())
                    .unwrap_or(0);

                let current_gen = tick_signal.generation.load(std::sync::atomic::Ordering::Acquire);

                // If server already ahead, respond immediately (no missed tick)
                if current_gen <= client_gen {
                    // Wait for next tick (with 30s timeout)
                    let _ = tokio::time::timeout(
                        std::time::Duration::from_secs(30),
                        tick_signal.notify.notified(),
                    ).await;
                }

                let gen = tick_signal.generation.load(std::sync::atomic::Ordering::Acquire);
                let lock = state.lock().unwrap();
                let players: Vec<&DevPlayerState> = lock.values().collect();
                let json = serde_json::to_string(&players).unwrap_or_default();
                // Include tick generation so client can send it back
                let body = format!(r#"{{"tick":{},"players":{}}}"#, gen, json);
                ("200 OK", body)
            } else if first_line.starts_with("GET /leaderboard") {
                // Proxy Walker leaderboard to avoid CORS
                match reqwest::Client::new()
                    .get("https://walker.akerud.se/api/leaderboard")
                    .timeout(std::time::Duration::from_secs(10))
                    .send().await
                {
                    Ok(resp) => {
                        if let Ok(text) = resp.text().await {
                            ("200 OK", text)
                        } else {
                            ("502 Bad Gateway", r#"{"error":"failed to read leaderboard"}"#.to_string())
                        }
                    }
                    Err(e) => {
                        tracing::warn!("[leaderboard proxy] Failed: {}", e);
                        ("502 Bad Gateway", format!(r#"{{"error":"{}"}}"#, e))
                    }
                }
            } else if first_line.starts_with("POST /join") {
                // Handle join asynchronously (needs Walker API lookup)
                handle_join(&request, &state, &world, &bridged_players).await
            } else if first_line.starts_with("POST /admin/respawn_bridge") {
                // Force-respawn a player's Walker bridge. Needed when the bridge
                // hits its retry cap or silently dies — avoids having to redeploy.
                // Gated on ADMIN_TOKEN.
                let expected = std::env::var("ADMIN_TOKEN").unwrap_or_default();
                let got = request.lines()
                    .find(|l| l.to_lowercase().starts_with("x-admin-token:"))
                    .and_then(|l| l.splitn(2, ':').nth(1))
                    .map(|s| s.trim())
                    .unwrap_or("");
                if expected.is_empty() {
                    ("403 Forbidden", r#"{"error":"ADMIN_TOKEN unset"}"#.to_string())
                } else if got != expected {
                    ("401 Unauthorized", r#"{"error":"bad admin token"}"#.to_string())
                } else {
                    let body = request.find("\r\n\r\n").map(|i| &request[i + 4..]).unwrap_or("");
                    let pid = serde_json::from_str::<serde_json::Value>(body).ok()
                        .and_then(|v| v.get("player_id")?.as_str().map(|s| s.to_string()))
                        .unwrap_or_default();
                    if pid.is_empty() {
                        ("400 Bad Request", r#"{"error":"player_id required"}"#.to_string())
                    } else {
                        // Remove from bridged set so ensure_bridge will spawn fresh.
                        if let Ok(mut lock) = bridged_players.lock() {
                            lock.remove(&pid);
                        }
                        // Look up walker_uuid and respawn.
                        let wid = state.lock().ok()
                            .and_then(|s| s.get(&pid).and_then(|p| p.walker_uuid.clone()));
                        match wid {
                            Some(w) => {
                                crate::walker_bridge::ensure_bridge(state.clone(), bridged_players.clone(), &pid, &w);
                                tracing::info!("[admin] respawned Walker bridge for {}", pid);
                                ("200 OK", r#"{"ok":true}"#.to_string())
                            }
                            None => ("404 Not Found", r#"{"error":"player has no walker_uuid"}"#.to_string()),
                        }
                    }
                }
            } else {
                handle_request(&request, &state, &events, &notifs, &world, &combat, &interiors)
            };

            // Serve static files for the game client
            let path = first_line.split_whitespace().nth(1).unwrap_or("/");
            if first_line.starts_with("GET /") && !path.starts_with("/api")
                && !path.starts_with("/players") && !path.starts_with("/events")
                && !path.starts_with("/combat") && !path.starts_with("/set_route")
                && !path.starts_with("/walker_update") && !path.starts_with("/debug_walk")
                && !path.starts_with("/buy_item") && !path.starts_with("/sell_item")
                && !path.starts_with("/use_item") && !path.starts_with("/equip_item")
                && !path.starts_with("/unequip_item") && !path.starts_with("/heartbeat")
                && !path.starts_with("/notifications")
                && !path.starts_with("/interior")
                && status == "404 Not Found"
            {
                let clean_path = path.split('?').next().unwrap_or(path);
                // Reject path traversal attempts — no "..", no absolute paths, no null bytes
                let file_path = if path == "/" {
                    "crates/gameclient/index.html".to_string()
                } else if clean_path.contains("..") || clean_path.contains('\0') || clean_path.contains('\\') {
                    String::new() // will fail to read, falls through to 404
                } else {
                    format!("crates/gameclient{}", clean_path)
                };
                if let Ok(contents) = tokio::fs::read(&file_path).await {
                    let content_type = if file_path.ends_with(".html") { "text/html" }
                        else if file_path.ends_with(".js") { "application/javascript" }
                        else if file_path.ends_with(".wasm") { "application/wasm" }
                        else if file_path.ends_with(".mp3") { "audio/mpeg" }
                        else if file_path.ends_with(".png") { "image/png" }
                        else if file_path.ends_with(".json") { "application/json" }
                        else { "application/octet-stream" };
                    let header = format!(
                        "HTTP/1.1 200 OK\r\n\
                         Content-Type: {}\r\n\
                         Content-Length: {}\r\n\
                         Access-Control-Allow-Origin: *\r\n\
                         Cache-Control: no-cache\r\n\
                         \r\n",
                        content_type, contents.len()
                    );
                    let _ = stream.write_all(header.as_bytes()).await;
                    let _ = stream.write_all(&contents).await;
                    return;
                }
            }

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

/// Look up a name on the Walker leaderboard, return their UUID if found.
async fn lookup_walker_uuid(name: &str) -> Option<String> {
    let client = reqwest::Client::new();
    let resp = match client.get("https://walker.akerud.se/api/leaderboard")
        .timeout(std::time::Duration::from_secs(5))
        .send().await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("[walker lookup] Failed to reach walker.akerud.se: {}", e);
            return None;
        }
    };
    let data: serde_json::Value = match resp.json().await {
        Ok(d) => d,
        Err(e) => {
            tracing::warn!("[walker lookup] Failed to parse leaderboard: {}", e);
            return None;
        }
    };
    // Search all time periods
    for period in ["today", "weekly", "all_time"] {
        if let Some(entries) = data.get(period).and_then(|v| v.as_array()) {
            for entry in entries {
                let entry_name = entry.get("name").and_then(|v| v.as_str()).unwrap_or("");
                if entry_name.eq_ignore_ascii_case(name) {
                    return entry.get("id").and_then(|v| v.as_str()).map(|s| s.to_string());
                }
            }
        }
    }
    None
}

/// Handle POST /join — async because it may call Walker API.
async fn handle_join(
    request: &str,
    state: &SharedState,
    world: &questlib::mapgen::WorldMap,
    bridged_players: &crate::walker_bridge::BridgedPlayers,
) -> (&'static str, String) {
    let body = match request.find("\r\n\r\n") {
        Some(i) => &request[i + 4..],
        None => return ("400 Bad Request", r#"{"error":"no body"}"#.to_string()),
    };
    let data: serde_json::Value = match serde_json::from_str(body) {
        Ok(d) => d,
        Err(_) => return ("400 Bad Request", r#"{"error":"invalid json"}"#.to_string()),
    };

    let name = data.get("name").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();
    tracing::info!("[join] Request from: '{}'", name);
    if name.is_empty() {
        tracing::warn!("[join] Empty name rejected");
        return ("400 Bad Request", r#"{"error":"name required"}"#.to_string());
    }

    // Try to auto-detect Walker UUID from the leaderboard by name
    let explicit_uuid = data.get("walker_uuid").and_then(|v| v.as_str()).map(|s| s.to_string());
    let walker_uuid = if let Some(uuid) = explicit_uuid.filter(|s| !s.is_empty()) {
        tracing::info!("[join] Explicit Walker UUID provided: {}", uuid);
        Some(uuid)
    } else {
        tracing::info!("[join] Looking up '{}' on Walker leaderboard...", name);
        match lookup_walker_uuid(&name).await {
            Some(uuid) => {
                tracing::info!("[join] Found Walker UUID: {}", uuid);
                Some(uuid)
            }
            None => {
                tracing::info!("[join] No Walker account found for '{}'", name);
                None
            }
        }
    };

    // Require Walker account — no treadmill, no game
    if walker_uuid.is_none() {
        return ("403 Forbidden", r#"{"error":"No Walker account found. Use your walker.akerud.se display name."}"#.to_string());
    }

    let chosen_champion = data.get("champion").and_then(|v| v.as_str()).unwrap_or("").trim().to_string();

    let (player_id, player_name) = {
        let mut lock = state.lock().unwrap();

        // Check by walker_uuid first (if we have one)
        let existing = walker_uuid.as_ref().and_then(|wid| {
            lock.values()
                .find(|p| p.walker_uuid.as_deref() == Some(wid))
                .map(|p| (p.id.clone(), p.name.clone()))
        });

        if let Some((pid, pname)) = existing {
            tracing::info!("[join] Found existing player by Walker UUID: {} ({})", pname, pid);
            if let Some(p) = lock.get_mut(&pid) {
                if !name.eq_ignore_ascii_case(&pname) {
                    p.name = name.clone();
                }
                if !chosen_champion.is_empty() {
                    p.champion = chosen_champion.clone();
                }
            }
            (pid, name)
        } else {
            // Check by name
            let by_name = lock.values()
                .find(|p| p.name.eq_ignore_ascii_case(&name))
                .map(|p| (p.id.clone(), p.name.clone()));

            if let Some((pid, _pname)) = by_name {
                if let Some(p) = lock.get_mut(&pid) {
                    if let Some(wid) = walker_uuid.as_ref() {
                        p.walker_uuid = Some(wid.clone());
                    }
                    if !chosen_champion.is_empty() {
                        p.champion = chosen_champion.clone();
                    }
                }
                (pid, name)
            } else {
                // Create new player
                let player_id = uuid::Uuid::new_v4().to_string();
                let start = world.pois.iter()
                    .find(|p| matches!(p.poi_type, questlib::mapgen::PoiType::Town | questlib::mapgen::PoiType::Village))
                    .map(|p| (p.x as i32, p.y as i32))
                    .unwrap_or((50, 40));
                lock.insert(player_id.clone(), DevPlayerState {
                    id: player_id.clone(),
                    name: name.clone(),
                    map_tile_x: start.0,
                    map_tile_y: start.1,
                    walker_uuid: walker_uuid.clone(),
                    champion: chosen_champion.clone(),
                    ..Default::default()
                });
                tracing::info!("New player joined: {} ({}) as {}", name, player_id, chosen_champion);
                (player_id, name)
            }
        }
    };

    // Spawn Walker bridge if we have a walker_uuid
    if let Some(ref wid) = walker_uuid {
        crate::walker_bridge::ensure_bridge(state.clone(), bridged_players.clone(), &player_id, wid);
    }

    // Look up the player's stored champion (may have been set on an earlier join).
    let champion = state.lock().ok()
        .and_then(|s| s.get(&player_id).map(|p| p.champion.clone()))
        .unwrap_or_default();
    ("200 OK", format!(
        r#"{{"player_id":"{}","name":"{}","champion":"{}"}}"#,
        player_id, player_name, champion
    ))
}

fn handle_request(request: &str, state: &SharedState, events: &SharedEvents, notifs: &SharedNotifs, world: &questlib::mapgen::WorldMap, combat: &crate::combat::SharedCombat, interiors: &crate::interior::SharedInteriors) -> (&'static str, String) {
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
                /// When extending a route, the client sends current meters so
                /// already-walked progress is preserved. Omit or 0 for fresh routes.
                #[serde(default)]
                meters: Option<f64>,
            }
            if let Ok(req) = serde_json::from_str::<RouteReq>(body) {
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    player.planned_route = req.route;
                    player.route_meters_walked = req.meters.unwrap_or(0.0);
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
                distance: f64,
                #[serde(default)]
                incline: f32,
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
                        player.current_incline = req.incline;
                        player.is_walking = req.actually_walking;
                    }
                    return ("200 OK", r#"{"ok":true}"#.to_string());
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // POST /buy_item — buy an item from a shop
    if first_line.starts_with("POST /buy_item") {
        if let Some(body_start) = request.find("\r\n\r\n") {
            let body = &request[body_start + 4..];
            #[derive(Deserialize)]
            struct BuyReq {
                player_id: String,
                item_id: String,
                cost: i32,
            }
            if let Ok(req) = serde_json::from_str::<BuyReq>(body) {
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    if player.gold < req.cost {
                        return ("400 Bad Request", r#"{"error":"not enough gold"}"#.to_string());
                    }
                    let catalog = Some(crate::item_catalog());
                    // Can't buy equipment that's already equipped
                    if player.equipment.has_equipped(&req.item_id) {
                        return ("400 Bad Request", r#"{"error":"already equipped"}"#.to_string());
                    }
                    if questlib::items::add_item(&mut player.inventory, &req.item_id, catalog) {
                        player.gold -= req.cost;
                        return ("200 OK", r#"{"ok":true}"#.to_string());
                    } else {
                        return ("400 Bad Request", r#"{"error":"inventory full or already owned"}"#.to_string());
                    }
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // POST /sell_item — sell an item for gold
    if first_line.starts_with("POST /sell_item") {
        if let Some(body_start) = request.find("\r\n\r\n") {
            let body = &request[body_start + 4..];
            #[derive(Deserialize)]
            struct SellReq { player_id: String, item_id: String }
            if let Ok(req) = serde_json::from_str::<SellReq>(body) {
                let catalog = Some(crate::item_catalog());
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    // Can't sell key items
                    let is_key = catalog
                        .and_then(|c| c.get(&req.item_id))
                        .map_or(false, |d| d.category == questlib::items::ItemCategory::KeyItem);
                    if is_key {
                        return ("400 Bad Request", r#"{"error":"can't sell key items"}"#.to_string());
                    }
                    if !questlib::items::has_item(&player.inventory, &req.item_id) {
                        return ("400 Bad Request", r#"{"error":"item not found"}"#.to_string());
                    }
                    // Sell price = half buy cost
                    let price = sell_price(&req.item_id);
                    questlib::items::remove_item(&mut player.inventory, &req.item_id);
                    player.gold += price;
                    if let Ok(mut n) = notifs.lock() {
                        let name = catalog.and_then(|c| c.get(&req.item_id)).map(|d| d.display_name.as_str()).unwrap_or(&req.item_id);
                        crate::push_notif(&mut n, &req.player_id, format!("Sold {} for {} gold", name, price));
                    }
                    return ("200 OK", r#"{"ok":true}"#.to_string());
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // POST /use_item — use a consumable item
    if first_line.starts_with("POST /use_item") {
        if let Some(body_start) = request.find("\r\n\r\n") {
            let body = &request[body_start + 4..];
            #[derive(Deserialize)]
            struct UseReq { player_id: String, item_id: String }
            if let Ok(req) = serde_json::from_str::<UseReq>(body) {
                let catalog = Some(crate::item_catalog());
                let cat = catalog;
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    if !questlib::items::has_item(&player.inventory, &req.item_id) {
                        return ("400 Bad Request", r#"{"error":"item not found"}"#.to_string());
                    }
                    let def = cat.and_then(|c| c.get(&req.item_id));
                    if def.map_or(true, |d| d.category != questlib::items::ItemCategory::Consumable) {
                        return ("400 Bad Request", r#"{"error":"not consumable"}"#.to_string());
                    }
                    let effects = def.map(|d| d.effects.clone()).unwrap_or_default();
                    questlib::items::remove_item(&mut player.inventory, &req.item_id);

                    // Apply effects
                    let mut messages = Vec::new();
                    for effect in &effects {
                        match effect {
                            questlib::items::ItemEffect::Heal { amount } => {
                                // Apply heal to active combat if any
                                if let Ok(mut combat_lock) = combat.lock() {
                                    if let Some(cs) = combat_lock.values_mut().find(|c| c.player_id == req.player_id) {
                                        let old_hp = cs.player_hp;
                                        cs.player_hp = (cs.player_hp + amount).min(cs.player_max_hp);
                                        let healed = cs.player_hp - old_hp;
                                        cs.turn_log.push(questlib::combat::CombatLogEntry {
                                            actor: "Player".to_string(),
                                            action: "heal".to_string(),
                                            damage: 0,
                                            message: format!("Used {}! +{} HP", def.map(|d| d.display_name.as_str()).unwrap_or("potion"), healed),
                                        });
                                        messages.push(format!("+{} HP", healed));
                                    }
                                }
                            }
                            questlib::items::ItemEffect::StatBonus { stat, amount } => {
                                // Apply to active combat as a temporary boost
                                if let Ok(mut combat_lock) = combat.lock() {
                                    if let Some(cs) = combat_lock.values_mut().find(|c| c.player_id == req.player_id) {
                                        match stat {
                                            questlib::items::StatType::Attack => { cs.player_attack += amount; messages.push(format!("+{} ATK", amount)); }
                                            questlib::items::StatType::Defense => { cs.player_defense += amount; messages.push(format!("+{} DEF", amount)); }
                                            questlib::items::StatType::MaxHp => { cs.player_max_hp += amount; cs.player_hp += amount; messages.push(format!("+{} HP", amount)); }
                                        }
                                    }
                                }
                            }
                            questlib::items::ItemEffect::RevealFog { radius } => {
                                let px = player.map_tile_x as usize;
                                let py = player.map_tile_y as usize;
                                // Fog reveal handled via notification — tick loop will pick it up
                                messages.push(format!("Revealed area (radius {})", radius));
                                // Store for fog update
                                drop(lock);
                                // Can't easily access fog here — push a notification instead
                                if let Ok(mut n) = notifs.lock() {
                                    crate::push_notif(&mut n, &req.player_id, format!("Used {}! Area revealed.", def.map(|d| d.display_name.as_str()).unwrap_or("item")));
                                }
                                return ("200 OK", serde_json::to_string(&serde_json::json!({"ok": true, "reveal_fog": {"x": px, "y": py, "radius": radius}})).unwrap());
                            }
                            // Equipment-only passive effect — ignored on consume.
                            questlib::items::ItemEffect::SpeedMultiplier { .. } => {}
                            questlib::items::ItemEffect::BuffSpeed { multiplier, duration_secs } => {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs()).unwrap_or(0);
                                let buff = questlib::items::ActiveBuff {
                                    kind: "speed".to_string(),
                                    multiplier: *multiplier,
                                    expires_unix: now + *duration_secs as u64,
                                    source_item: req.item_id.clone(),
                                };
                                player.active_buffs.push(buff);
                                let pct = ((multiplier - 1.0) * 100.0).round() as i32;
                                let mins = duration_secs / 60;
                                messages.push(format!("+{}% speed for {} min", pct, mins));
                            }
                        }
                    }
                    if let Ok(mut n) = notifs.lock() {
                        let item_name = def.map(|d| d.display_name.as_str()).unwrap_or(&req.item_id);
                        crate::push_notif(&mut n, &req.player_id, format!("Used {}! {}", item_name, messages.join(", ")));
                    }
                    return ("200 OK", r#"{"ok":true}"#.to_string());
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // POST /equip_item — equip a piece of gear
    if first_line.starts_with("POST /equip_item") {
        if let Some(body_start) = request.find("\r\n\r\n") {
            let body = &request[body_start + 4..];
            #[derive(Deserialize)]
            struct EquipReq { player_id: String, item_id: String }
            if let Ok(req) = serde_json::from_str::<EquipReq>(body) {
                let catalog = crate::item_catalog();
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    match questlib::items::equip_item(&mut player.equipment, &mut player.inventory, &req.item_id, &catalog) {
                        Ok(_old) => {
                            if let Ok(mut n) = notifs.lock() {
                                let name = catalog.get(&req.item_id).map(|d| d.display_name.as_str()).unwrap_or(&req.item_id);
                                crate::push_notif(&mut n, &req.player_id, format!("Equipped {}!", name));
                            }
                            return ("200 OK", r#"{"ok":true}"#.to_string());
                        }
                        Err(e) => return ("400 Bad Request", format!(r#"{{"error":"{}"}}"#, e)),
                    }
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // POST /unequip_item — unequip gear from a slot
    if first_line.starts_with("POST /unequip_item") {
        if let Some(body_start) = request.find("\r\n\r\n") {
            let body = &request[body_start + 4..];
            #[derive(Deserialize)]
            struct UnequipReq { player_id: String, slot: questlib::items::EquipmentSlot }
            if let Ok(req) = serde_json::from_str::<UnequipReq>(body) {
                let catalog = crate::item_catalog();
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    if questlib::items::unequip_item(&mut player.equipment, &mut player.inventory, req.slot, &catalog) {
                        return ("200 OK", r#"{"ok":true}"#.to_string());
                    }
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // GET /events/active?player_id=X — active events visible to THIS player.
    // Events already completed by this player (personally) are excluded, so
    // one player re-triggering a quest can't leak its dialog onto others.
    if first_line.starts_with("GET /events/active") {
        let player_id = first_line.split('?').nth(1)
            .and_then(|qs| qs.split('&').find(|p| p.starts_with("player_id=")))
            .and_then(|p| p.strip_prefix("player_id="))
            .and_then(|v| v.split_whitespace().next())
            .unwrap_or("");
        let completed: Vec<String> = if !player_id.is_empty() {
            state.lock().ok()
                .and_then(|s| s.get(player_id).map(|p| p.completed_events.clone()))
                .unwrap_or_default()
        } else { Vec::new() };

        let lock = events.lock().unwrap();
        let mut result: Vec<_> = lock.active_events().into_iter()
            .filter(|e| !completed.contains(&e.id))
            .cloned().collect();
        // Repeatable events (shops, wells, etc.) — permanent POI features
        // independent of per-player completion, always visible.
        for event in &lock.events {
            if event.repeatable && event.status == questlib::events::EventStatus::Pending {
                result.push(event.clone());
            }
        }
        let json = serde_json::to_string(&result).unwrap_or_default();
        return ("200 OK", json);
    }

    // `GET /events` (whole catalog) was removed — clients now use
    // `/events/active?player_id=X` which filters by per-player completion.
    // Leaving the whole catalog reachable leaked other players' completion state.

    // POST /events/{id}/complete — mark event as completed and apply outcomes
    if first_line.contains("/events/") && first_line.contains("/complete") {
        let body_player_id = request.find("\r\n\r\n")
            .and_then(|i| serde_json::from_str::<serde_json::Value>(&request[i + 4..]).ok())
            .and_then(|v| v.get("player_id")?.as_str().map(|s| s.to_string()));

        // Require player_id. Previously this fell back to `state.values_mut().next()`
        // which dumped gold/items into whoever hashed first — a hard-to-debug leak
        // if a client ever omitted the field.
        let Some(body_player_id) = body_player_id else {
            return ("400 Bad Request", r#"{"error":"player_id required"}"#.to_string());
        };

        // Extract event id from URL: POST /events/some_id/complete
        let parts: Vec<&str> = first_line.split('/').collect();
        if parts.len() >= 3 {
            let event_id = parts.iter()
                .position(|&p| p == "events")
                .and_then(|i| parts.get(i + 1))
                .map(|s| s.split_whitespace().next().unwrap_or(s));

            if let Some(event_id) = event_id {
                // Hold events_lock AND state_lock together so a tick between the
                // event-status flip and the player.completed_events write can't
                // observe an inconsistent pair and re-trigger the quest. Lock order
                // (events then state) matches run_tick_dev, so no deadlock.
                let mut events_lock = events.lock().unwrap();
                if let Some(event) = events_lock.get_mut(event_id) {
                    if event.transition(questlib::events::EventStatus::Completed).is_ok() {
                        let outcomes = event.outcomes.clone();
                        let repeatable = event.repeatable;
                        if repeatable {
                            event.force_status(questlib::events::EventStatus::Pending);
                        }
                        let mut state_lock = state.lock().unwrap();
                        let player = state_lock.get_mut(&body_player_id);
                        if let Some(player) = player {
                            if !player.completed_events.contains(&event_id.to_string()) {
                                player.completed_events.push(event_id.to_string());
                            }
                            for outcome in &outcomes {
                                match outcome {
                                    questlib::events::EventOutcome::Gold { amount } => {
                                        player.gold += amount;
                                    }
                                    questlib::events::EventOutcome::Item { name } => {
                                        let cat = Some(crate::item_catalog());
                                        questlib::items::add_item(&mut player.inventory, name, cat);
                                    }
                                    questlib::events::EventOutcome::RevealFog { x, y, radius } => {
                                        let mut fog = if !player.revealed_tiles.is_empty() {
                                            questlib::fog::FogBitfield::from_base64(&player.revealed_tiles).unwrap_or_default()
                                        } else {
                                            questlib::fog::FogBitfield::new()
                                        };
                                        fog.reveal_radius(*x, *y, *radius);
                                        player.revealed_tiles = fog.to_base64();
                                    }
                                    questlib::events::EventOutcome::Notification { text } => {
                                        if let Ok(mut n) = notifs.lock() {
                                            crate::push_notif(&mut n, &body_player_id, text.clone());
                                        }
                                    }
                                    _ => {}
                                }
                            }
                        }
                        return ("200 OK", r#"{"ok":true}"#.to_string());
                    }
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"invalid event"}"#.to_string());
    }

    // GET /notifications?player_id=X — fetch and clear pending notifications for a player
    if first_line.starts_with("GET /notifications") {
        let player_id = first_line.split('?').nth(1)
            .and_then(|qs| qs.split('&').find(|p| p.starts_with("player_id=")))
            .and_then(|p| p.strip_prefix("player_id="))
            .and_then(|v| v.split_whitespace().next())
            .unwrap_or("");
        // Validate player_id exists in state — prevents arbitrary enumeration.
        // (Not real auth: anyone who can guess/learn an ID can read notifications.)
        let known = state.lock().map(|s| s.contains_key(player_id)).unwrap_or(false);
        if !known {
            return ("200 OK", "[]".to_string());
        }
        let msgs = notifs.lock()
            .map(|mut n| n.remove(player_id).unwrap_or_default())
            .unwrap_or_default();
        let json = serde_json::to_string(&msgs).unwrap_or_default();
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
                    // Just set speed + flags. The tick loop computes the actual
                    // distance from speed when debug_walking is true (avoids
                    // integer truncation at slow walking speeds).
                    player.current_speed_kmh = req.speed;
                    player.is_walking = true;
                    player.debug_walking = true;
                    return ("200 OK", r#"{"ok":true}"#.to_string());
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // POST /combat/flee — player runs away (check before GET /combat)
    if first_line.starts_with("POST /combat/flee") {
        // Extract player_id from body
        let body_player_id = request.find("\r\n\r\n")
            .and_then(|i| serde_json::from_str::<serde_json::Value>(&request[i + 4..]).ok())
            .and_then(|v| v.get("player_id")?.as_str().map(|s| s.to_string()));
        let pid = body_player_id.unwrap_or_default();
        let event_id = crate::combat::get_combat_for_player(combat, &pid).map(|c| c.event_id);
        if let Some(eid) = event_id {
            if let Some(updated) = crate::combat::flee(combat, &eid) {
                let json = serde_json::to_string(&updated).unwrap_or_default();
                return ("200 OK", json);
            }
        }
        return ("400 Bad Request", r#"{"error":"no active combat"}"#.to_string());
    }

    // GET /combat?player_id=<uuid> — combat state for a specific player (or null)
    if first_line.starts_with("GET /combat") {
        let player_id = first_line.split('?').nth(1)
            .and_then(|qs| qs.split('&').find(|p| p.starts_with("player_id=")))
            .and_then(|p| p.strip_prefix("player_id="))
            .and_then(|v| v.split_whitespace().next())
            .unwrap_or("");
        if let Some(state) = crate::combat::get_combat_for_player(combat, player_id) {
            let json = serde_json::to_string(&state).unwrap_or_default();
            return ("200 OK", json);
        }
        return ("200 OK", "null".to_string());
    }

    // GET /login?name=X — find player by name, return player_id (legacy)
    if first_line.starts_with("GET /login") {
        let name = first_line.split('?').nth(1)
            .and_then(|qs| qs.split('&').find(|p| p.starts_with("name=")))
            .and_then(|p| p.strip_prefix("name="))
            .and_then(|v| v.split_whitespace().next())
            .map(|s| urlencoding(s))
            .unwrap_or_default();
        let lock = state.lock().unwrap();
        if let Some(player) = lock.values().find(|p| p.name.eq_ignore_ascii_case(&name)) {
            return ("200 OK", format!(r#"{{"player_id":"{}","name":"{}"}}"#, player.id, player.name));
        }
        return ("404 Not Found", r#"{"error":"player not found"}"#.to_string());
    }

    // POST /heartbeat — mark player browser as open (no-op for dev)
    if first_line.starts_with("POST /heartbeat") {
        return ("200 OK", r#"{"ok":true}"#.to_string());
    }

    // ── Interior (caves / dungeons / castles) ─────────────

    // GET /interior?id=X — return the interior's map data (tiles + portals + chests)
    if first_line.starts_with("GET /interior") {
        let id = first_line.split('?').nth(1)
            .and_then(|qs| qs.split('&').find(|p| p.starts_with("id=")))
            .and_then(|p| p.strip_prefix("id="))
            .and_then(|v| v.split_whitespace().next())
            .unwrap_or("");
        let Some(interior) = interiors.get(id) else {
            return ("404 Not Found", r#"{"error":"unknown interior"}"#.to_string());
        };
        return ("200 OK", serde_json::to_string(interior).unwrap_or_default());
    }

    // POST /enter_interior — body: {player_id, interior_id, spawn: [x,y]}
    // Phase 1 integration hook: the client (or admin) tells us which interior
    // to enter and where to drop the player. Phase 2 ties this to a POI event.
    if first_line.starts_with("POST /enter_interior") {
        let body = request.find("\r\n\r\n").map(|i| &request[i + 4..]).unwrap_or("");
        let Ok(v) = serde_json::from_str::<serde_json::Value>(body) else {
            return ("400 Bad Request", r#"{"error":"invalid json"}"#.to_string());
        };
        let pid = v.get("player_id").and_then(|v| v.as_str()).unwrap_or("");
        let iid = v.get("interior_id").and_then(|v| v.as_str()).unwrap_or("");
        let spawn = v.get("spawn").and_then(|v| v.as_array())
            .and_then(|arr| {
                let x = arr.get(0)?.as_u64()? as usize;
                let y = arr.get(1)?.as_u64()? as usize;
                Some((x, y))
            });
        let Some(spawn) = spawn else {
            return ("400 Bad Request", r#"{"error":"spawn: [x,y] required"}"#.to_string());
        };
        if pid.is_empty() || iid.is_empty() {
            return ("400 Bad Request", r#"{"error":"player_id + interior_id required"}"#.to_string());
        }
        use crate::interior::{enter_interior, PortalTransitionResult};
        match enter_interior(interiors, state, pid, iid, spawn) {
            PortalTransitionResult::Moved { tile, .. } => {
                return ("200 OK", format!(r#"{{"ok":true,"tile":[{},{}]}}"#, tile.0, tile.1));
            }
            PortalTransitionResult::UnknownInterior => return ("404 Not Found", r#"{"error":"unknown interior"}"#.to_string()),
            PortalTransitionResult::UnknownPlayer => return ("404 Not Found", r#"{"error":"unknown player"}"#.to_string()),
            PortalTransitionResult::NotOnPortal => return ("400 Bad Request", r#"{"error":"spawn tile not walkable"}"#.to_string()),
        }
    }

    // POST /use_portal — body: {player_id}. Uses the portal at the player's
    // current interior tile; falls back to overworld_return if none. No-op
    // if the player is on the overworld.
    if first_line.starts_with("POST /use_portal") {
        let body = request.find("\r\n\r\n").map(|i| &request[i + 4..]).unwrap_or("");
        let pid = serde_json::from_str::<serde_json::Value>(body)
            .ok()
            .and_then(|v| v.get("player_id")?.as_str().map(|s| s.to_string()))
            .unwrap_or_default();
        if pid.is_empty() {
            return ("400 Bad Request", r#"{"error":"player_id required"}"#.to_string());
        }
        use crate::interior::{use_portal, PortalTransitionResult};
        match use_portal(interiors, state, &pid) {
            PortalTransitionResult::Moved { new_location, tile } => {
                let loc_json = serde_json::to_string(&new_location).unwrap_or_default();
                return ("200 OK", format!(r#"{{"ok":true,"location":{},"tile":[{},{}]}}"#, loc_json, tile.0, tile.1));
            }
            PortalTransitionResult::NotOnPortal => return ("400 Bad Request", r#"{"error":"not on a portal"}"#.to_string()),
            PortalTransitionResult::UnknownInterior => return ("404 Not Found", r#"{"error":"unknown interior"}"#.to_string()),
            PortalTransitionResult::UnknownPlayer => return ("404 Not Found", r#"{"error":"unknown player"}"#.to_string()),
        }
    }

    // ── Admin endpoints ────────────────────────────────────
    // Gated by a shared secret in the ADMIN_TOKEN env var AND an
    // X-Admin-Token header. If the env var is unset or empty, admin
    // endpoints are disabled entirely (403). Not a full auth system —
    // it's a "don't let randoms poke state" safeguard.
    if first_line.starts_with("POST /admin/") {
        let expected = std::env::var("ADMIN_TOKEN").unwrap_or_default();
        if expected.is_empty() {
            return ("403 Forbidden", r#"{"error":"admin endpoints disabled (ADMIN_TOKEN unset)"}"#.to_string());
        }
        let got = request.lines()
            .find(|l| l.to_lowercase().starts_with("x-admin-token:"))
            .and_then(|l| l.splitn(2, ':').nth(1))
            .map(|s| s.trim())
            .unwrap_or("");
        if got != expected {
            return ("401 Unauthorized", r#"{"error":"bad admin token"}"#.to_string());
        }

        let body = request.find("\r\n\r\n").map(|i| &request[i + 4..]).unwrap_or("");
        let data: serde_json::Value = match serde_json::from_str(body) {
            Ok(v) => v,
            Err(_) => return ("400 Bad Request", r#"{"error":"invalid json"}"#.to_string()),
        };

        // POST /admin/give_item — { player_id, item_id, quantity? }
        if first_line.starts_with("POST /admin/give_item") {
            let pid = data.get("player_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let item_id = data.get("item_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let quantity = data.get("quantity").and_then(|v| v.as_u64()).unwrap_or(1) as u32;
            if pid.is_empty() || item_id.is_empty() {
                return ("400 Bad Request", r#"{"error":"player_id and item_id required"}"#.to_string());
            }
            let catalog = crate::item_catalog();
            if catalog.get(&item_id).is_none() {
                return ("400 Bad Request", format!(r#"{{"error":"unknown item: {}"}}"#, item_id));
            }
            let mut lock = state.lock().unwrap();
            let Some(p) = lock.get_mut(&pid) else {
                return ("404 Not Found", r#"{"error":"player not found"}"#.to_string());
            };
            for _ in 0..quantity {
                questlib::items::add_item(&mut p.inventory, &item_id, Some(catalog));
            }
            tracing::info!("[admin] give_item player={} item={} qty={}", p.name, item_id, quantity);
            return ("200 OK", r#"{"ok":true}"#.to_string());
        }

        // POST /admin/reset_event — { event_id, status } — sets global event status
        if first_line.starts_with("POST /admin/reset_event") {
            let event_id = data.get("event_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let status_str = data.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
            if event_id.is_empty() {
                return ("400 Bad Request", r#"{"error":"event_id required"}"#.to_string());
            }
            let new_status = match status_str {
                "pending"   => questlib::events::EventStatus::Pending,
                "active"    => questlib::events::EventStatus::Active,
                "completed" => questlib::events::EventStatus::Completed,
                "dismissed" => questlib::events::EventStatus::Dismissed,
                _ => return ("400 Bad Request", r#"{"error":"status must be pending|active|completed|dismissed"}"#.to_string()),
            };
            let mut events_lock = events.lock().unwrap();
            let Some(event) = events_lock.get_mut(&event_id) else {
                return ("404 Not Found", r#"{"error":"event not found"}"#.to_string());
            };
            event.force_status(new_status);
            tracing::info!("[admin] reset_event {} -> {:?}", event_id, new_status);
            return ("200 OK", r#"{"ok":true}"#.to_string());
        }

        // POST /admin/grant_completion — { player_id, event_id }
        if first_line.starts_with("POST /admin/grant_completion") {
            let pid = data.get("player_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let event_id = data.get("event_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if pid.is_empty() || event_id.is_empty() {
                return ("400 Bad Request", r#"{"error":"player_id and event_id required"}"#.to_string());
            }
            let mut lock = state.lock().unwrap();
            let Some(p) = lock.get_mut(&pid) else {
                return ("404 Not Found", r#"{"error":"player not found"}"#.to_string());
            };
            if !p.completed_events.contains(&event_id) {
                p.completed_events.push(event_id.clone());
            }
            tracing::info!("[admin] grant_completion player={} event={}", p.name, event_id);
            return ("200 OK", r#"{"ok":true}"#.to_string());
        }

        // POST /admin/revoke_completion — { player_id, event_id } — lets a player re-experience a quest
        if first_line.starts_with("POST /admin/revoke_completion") {
            let pid = data.get("player_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let event_id = data.get("event_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if pid.is_empty() || event_id.is_empty() {
                return ("400 Bad Request", r#"{"error":"player_id and event_id required"}"#.to_string());
            }
            let mut lock = state.lock().unwrap();
            let Some(p) = lock.get_mut(&pid) else {
                return ("404 Not Found", r#"{"error":"player not found"}"#.to_string());
            };
            p.completed_events.retain(|id| id != &event_id);
            tracing::info!("[admin] revoke_completion player={} event={}", p.name, event_id);
            return ("200 OK", r#"{"ok":true}"#.to_string());
        }

        return ("404 Not Found", r#"{"error":"unknown admin endpoint"}"#.to_string());
    }

    ("404 Not Found", r#"{"error":"not found"}"#.to_string())
}

fn urlencoding(s: &str) -> String {
    s.replace("%20", " ").replace("+", " ")
}

fn sell_price(item_id: &str) -> i32 {
    let base = match item_id {
        "wooden_club" => 40, "iron_sword" => 120, "fire_blade" => 200, "frost_axe" => 180,
        "leather_vest" => 50, "chainmail" => 150, "dragonscale_armor" => 300,
        "warm_cloak" => 60, "bog_charm" => 60, "ring_of_vigor" => 400, "berserker_pendant" => 500,
        "health_potion" => 30, "greater_health_potion" => 60, "speed_potion" => 80,
        "mystery_potion" => 40, "battle_elixir" => 120,
        "torch" => 20, "compass" => 60, "explorers_map" => 180,
        // Legendary gear
        "dragonslayer" => 2500, "stormbringer" => 1500,
        "mythril_plate" => 1800, "phoenix_robe" => 2000,
        "amulet_of_kings" => 1200, "soulreaper_pendant" => 1000,
        _ => 20,
    };
    (base / 2).max(5)
}
