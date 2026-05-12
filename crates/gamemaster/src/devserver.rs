//! Simple HTTP dev server that replaces Supabase for local development.
//! Serves game state as JSON and accepts route updates.
//! Runs alongside the Game Master tick loop.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// serde default for `DevPlayerState.adventure_id` — matches
/// `adventure::DEFAULT_ADVENTURE_ID`. Existing save files without the
/// field will land on this adventure (the original Frost Lord story).
fn default_adventure_id() -> String {
    crate::adventure::DEFAULT_ADVENTURE_ID.to_string()
}

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
    /// Shop event ids the player has discovered — either by visiting or
    /// (Phase B, later) by an NPC-revealed outcome. Populates the shop
    /// markers the client draws over the map when TAB is held.
    #[serde(default)]
    pub revealed_shops: Vec<String>,
    /// Forge upgrade level per item id. 0 = not upgraded. Max 5 per item
    /// (enforced at /forge_upgrade). +1 atk/def/hp or +1 % speed per level
    /// depending on the item's equipment slot — see questlib::items.
    #[serde(default)]
    pub item_upgrades: std::collections::HashMap<String, u8>,
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
    /// Permanent meta-progression boons earned across adventures.
    /// Boon ids only — definitions live in `questlib::boons::catalog()`.
    /// Survives adventure resets (level / gold / inventory all wipe; this
    /// list does not).
    #[serde(default)]
    pub boons: Vec<String>,
    /// `total_distance_m` when the current play session started — i.e.,
    /// when the player went from a long idle gap (>60 s) to walking
    /// again. Used for the Sprint boon's "first 1 km of session" check.
    /// Resets to current `total_distance_m` on each resume so the
    /// boost actually re-applies between play sessions.
    #[serde(default)]
    pub session_start_distance_m: f64,
    /// Last time the player was actively walking (unix seconds). Used to
    /// detect "long idle" → resume transitions for `session_start_distance_m`.
    #[serde(default)]
    pub last_walking_unix: u64,
    /// Which adventure this player is currently in. The server has
    /// multiple AdventureBundles loaded; tick + endpoints route by
    /// this id. Existing saves default to "frost_quest" via the
    /// serde-default so no migration is needed. Switched by
    /// `POST /start_new_adventure`.
    #[serde(default = "default_adventure_id")]
    pub adventure_id: String,
    /// Adventure-scoped boons: keyed by adventure_id, each value is
    /// the list of boon ids earned IN that adventure that only apply
    /// WHILE the player is in that adventure. Used by mid-tier boss
    /// drops in the chaos arc — small power-ups (e.g. "Frostproof:
    /// ice tiles -50%") that the player keeps for the rest of this
    /// adventure but don't follow them back to frost_quest or the
    /// next chapter. Permanent cross-adventure boons live in `boons`.
    #[serde(default)]
    pub adventure_boons: std::collections::HashMap<String, Vec<String>>,
    /// Pending boon choice from a recent climactic-quest victory.
    /// Cleared by `POST /select_boon`. The 3 IDs are deterministic per
    /// `(player_id, event_id)` so a refresh / re-poll won't re-roll
    /// the offer. Client polls this and pops the picker modal when
    /// it's `Some`.
    #[serde(default)]
    pub pending_boon_choice: Option<PendingBoonChoice>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PendingBoonChoice {
    /// Event the boon was earned from (so the client can show "for
    /// defeating X" context, and so `pick_choices` can be re-derived
    /// deterministically without trusting the saved `choices`).
    pub event_id: String,
    /// The 3 (or fewer) boon ids on offer. Empty would mean every
    /// boon already owned — server clears the pending in that case.
    pub choices: Vec<String>,
}

impl DevPlayerState {
    /// All boon ids that apply RIGHT NOW for effect calculations:
    /// permanent boons (cross-adventure) + adventure-scoped boons
    /// from the player's current adventure. Adventure-scoped boons
    /// from OTHER adventures the player has been in stay parked in
    /// `adventure_boons` and only become active again when the
    /// player switches back.
    pub fn effective_boons(&self) -> Vec<String> {
        let mut out = self.boons.clone();
        if let Some(adv) = self.adventure_boons.get(&self.adventure_id) {
            out.extend(adv.iter().cloned());
        }
        out
    }
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

pub async fn start_dev_server(state: SharedState, events: SharedEvents, notifs: SharedNotifs, world: Arc<questlib::mapgen::WorldMap>, combat: crate::combat::SharedCombat, tick_signal: SharedTickSignal, bridged_players: crate::walker_bridge::BridgedPlayers, interiors: crate::interior::SharedInteriors, entity_defs: crate::mobile_entity::SharedEntityDefs, entity_states: crate::mobile_entity::SharedEntityStates, bundles: Arc<std::collections::HashMap<String, crate::adventure::AdventureBundle>>) -> Result<()> {
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
        let entity_defs = entity_defs.clone();
        let entity_states = entity_states.clone();
        let bundles = bundles.clone();

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
                handle_request(&request, &state, &events, &notifs, &world, &combat, &interiors, &entity_defs, &entity_states, &bundles)
            };

            // Serve static files for the game client
            let path = first_line.split_whitespace().nth(1).unwrap_or("/");
            if first_line.starts_with("GET /") && !path.starts_with("/api")
                && !path.starts_with("/players") && !path.starts_with("/events")
                && !path.starts_with("/combat") && !path.starts_with("/set_route")
                && !path.starts_with("/debug_walk")
                && !path.starts_with("/buy_item") && !path.starts_with("/sell_item")
                && !path.starts_with("/use_item") && !path.starts_with("/equip_item")
                && !path.starts_with("/unequip_item") && !path.starts_with("/heartbeat")
                && !path.starts_with("/notifications")
                && !path.starts_with("/interior")
                && !path.starts_with("/entities")
                && !path.starts_with("/world")
                && !path.starts_with("/adventures")
                && !path.starts_with("/shops")
                && !path.starts_with("/journal")
                && !path.starts_with("/daynight")
                && !path.starts_with("/version")
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
                    // Start every new player on the default adventure
                    // (frost_quest). They can switch later via the
                    // title screen's "New Adventure" button.
                    adventure_id: default_adventure_id(),
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
    // Resolve the player's adventure → preset → seed + dims so the
    // client can build the matching WorldGrid. For new players this
    // is always frost_quest's defaults; for returning players who've
    // switched to chaos, this returns chaos's 200×160 + seed 99999.
    let (map_seed, map_width, map_height) = {
        let adv_id = state.lock().ok()
            .and_then(|s| s.get(&player_id).map(|p| p.adventure_id.clone()))
            .unwrap_or_default();
        crate::adventure::presets()
            .into_iter()
            .find(|p| p.id == adv_id)
            .map(|p| (p.map_seed, p.map_width, p.map_height))
            .unwrap_or((12345, questlib::mapgen::MAP_W, questlib::mapgen::MAP_H))
    };
    ("200 OK", format!(
        r#"{{"player_id":"{}","name":"{}","champion":"{}","map_seed":{},"map_width":{},"map_height":{}}}"#,
        player_id, player_name, champion, map_seed, map_width, map_height
    ))
}

fn handle_request(request: &str, state: &SharedState, events: &SharedEvents, notifs: &SharedNotifs, world: &questlib::mapgen::WorldMap, combat: &crate::combat::SharedCombat, interiors: &crate::interior::SharedInteriors, entity_defs: &crate::mobile_entity::SharedEntityDefs, entity_states: &crate::mobile_entity::SharedEntityStates, bundles: &Arc<HashMap<String, crate::adventure::AdventureBundle>>) -> (&'static str, String) {
    // Look up the calling player's bundle, falling back to the
    // default. Used by per-adventure endpoint routing below.
    let bundle_for_player = |pid: &str| -> Option<&crate::adventure::AdventureBundle> {
        let adv_id = state.lock().ok()
            .and_then(|s| s.get(pid).map(|p| p.adventure_id.clone()))?;
        bundles.get(&adv_id)
    };
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

    // GET /entities?player_id=X — all alive mobile entities. The
    // earlier 20-tile Chebyshev viewport filter was a bandwidth
    // optimization for adventures with hundreds of entities; until
    // we get there, ship everything so testers never wonder "why is
    // it invisible from here". Easy to re-enable when we need it.
    if first_line.starts_with("GET /entities") {
        const VIEW_RADIUS: i32 = i32::MAX;
        let player_id = first_line.split('?').nth(1)
            .and_then(|qs| qs.split('&').find(|p| p.starts_with("player_id=")))
            .and_then(|p| p.strip_prefix("player_id="))
            .and_then(|v| v.split_whitespace().next())
            .unwrap_or("");
        let player_xy = if player_id.is_empty() {
            None
        } else {
            state.lock().ok().and_then(|s| s.get(player_id).map(|p| (p.map_tile_x, p.map_tile_y)))
        };
        // Route to the player's bundle's entity defs / states so a
        // chaos player doesn't see frost_quest's wolves (and vice
        // versa).
        let bundle = bundle_for_player(player_id);
        let defs_ref   = bundle.map(|b| b.entity_defs.as_ref()).unwrap_or(entity_defs);
        let states_ref = bundle.map(|b| &b.entity_states).unwrap_or(entity_states);
        let mut payload: Vec<serde_json::Value> = Vec::new();
        if let (Some((px, py)), Ok(states_lock)) = (player_xy, states_ref.lock()) {
            for (id, def) in defs_ref.iter() {
                let Some(s) = states_lock.get(id) else { continue };
                if !s.alive { continue; }
                let dx = (s.current.0 as i32 - px).abs();
                let dy = (s.current.1 as i32 - py).abs();
                if dx.max(dy) > VIEW_RADIUS { continue; }
                let facing = match s.facing {
                    questlib::mobile_entity::Facing::Up => "up",
                    questlib::mobile_entity::Facing::Down => "down",
                    questlib::mobile_entity::Facing::Left => "left",
                    questlib::mobile_entity::Facing::Right => "right",
                };
                payload.push(serde_json::json!({
                    "id": id,
                    "sprite": def.sprite,
                    "x": s.current.0,
                    "y": s.current.1,
                    "facing": facing,
                }));
            }
        }
        let json = serde_json::to_string(&serde_json::json!({"entities": payload}))
            .unwrap_or_else(|_| r#"{"entities":[]}"#.to_string());
        return ("200 OK", json);
    }

    // POST /set_route — set planned route for a player.
    // Body: {"player_id": "...", "route": "[[x,y],...]"}
    //
    // Trust boundary: the client submits only geometry. The server owns the
    // player's position along the route (route_meters_walked). We compute it
    // by finding the player's current tile in the new route and accumulating
    // tile costs up to that index — no client-supplied "how far along" values.
    if first_line.starts_with("POST /set_route") {
        if let Some(body_start) = request.find("\r\n\r\n") {
            let body = &request[body_start + 4..];
            #[derive(Deserialize)]
            struct RouteReq {
                player_id: String,
                route: String,
            }
            if let Ok(req) = serde_json::from_str::<RouteReq>(body) {
                let parsed = questlib::route::parse_route_json(&req.route).unwrap_or_default();
                // Route to the player's bundle's world for tile-cost
                // calculations — chaos and frost_quest now have
                // different seeds so their terrain doesn't match.
                let bundle = bundle_for_player(&req.player_id);
                let world_ref = bundle.map(|b| b.world.as_ref()).unwrap_or(world);
                let interiors_ref = bundle.map(|b| b.interiors.as_ref()).unwrap_or(interiors.as_ref());
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    let current = (player.map_tile_x as usize, player.map_tile_y as usize);
                    let interior_per_tile: Option<f64> = player.location.interior_id()
                        .map(|id| interiors_ref.get(id).map(|i| i.floor_cost_m).unwrap_or(40) as f64);
                    // Sum tile costs from start of `route` up to (but not
                    // including) `idx`. Closure so we can call it for
                    // both the OLD route (to recover partial progress)
                    // and the NEW route (to compute the new base).
                    let cost_to_idx = |route: &[(usize, usize)], idx: usize| -> f64 {
                        match interior_per_tile {
                            Some(per) => idx as f64 * per,
                            None => route[..idx].iter()
                                .map(|&(x, y)| questlib::route::tile_cost(
                                    world_ref.biome_at(x, y),
                                    world_ref.has_road_at(x, y),
                                ) as f64)
                                .sum(),
                        }
                    };
                    // Recover sub-tile progress from the OLD route: meters
                    // walked since the last full tile boundary. Without
                    // this, re-routing while mid-tile snaps the player
                    // back to the last confirmed tile center — what
                    // looked like "pushed back to the last tile" in
                    // testing.
                    let old_parsed = questlib::route::parse_route_json(&player.planned_route)
                        .unwrap_or_default();
                    let partial = old_parsed.iter().position(|&t| t == current)
                        .map(|i| (player.route_meters_walked - cost_to_idx(&old_parsed, i)).max(0.0))
                        .unwrap_or(0.0);
                    // New route_meters = costs up to current tile in the
                    // new route + retained partial progress. If the
                    // current tile isn't in the new route at all (rare —
                    // client always starts the new route from the
                    // current tile), fall back to 0.
                    let new_meters = parsed.iter().position(|&t| t == current)
                        .map(|idx| cost_to_idx(&parsed, idx) + partial)
                        .unwrap_or(0.0);
                    player.planned_route = req.route;
                    player.route_meters_walked = new_meters;
                    return ("200 OK", r#"{"ok":true}"#.to_string());
                }
            }
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // `/walker_update` was removed. Treadmill data flows through the
    // WebSocket bridge (`gamemaster::walker_bridge`) directly into
    // DevPlayerState; no client-supplied distance.

    // POST /select_boon — body: {"player_id": "...", "boon_id": "swift_boots"}.
    // Consumes the player's pending boon choice (set by a climactic
    // quest victory). Validates that boon_id is one of the offered
    // three, that the player doesn't already own it, and that a
    // pending choice actually exists.
    if first_line.starts_with("POST /select_boon") {
        if let Some(body_start) = request.find("\r\n\r\n") {
            #[derive(Deserialize)]
            struct Req { player_id: String, boon_id: String }
            let Ok(req) = serde_json::from_str::<Req>(&request[body_start + 4..]) else {
                return ("400 Bad Request", r#"{"error":"bad body"}"#.to_string());
            };
            let mut lock = state.lock().unwrap();
            let Some(p) = lock.get_mut(&req.player_id) else {
                return ("404 Not Found", r#"{"error":"player not found"}"#.to_string());
            };
            let Some(pending) = p.pending_boon_choice.clone() else {
                return ("400 Bad Request", r#"{"error":"no pending boon choice"}"#.to_string());
            };
            if !pending.choices.iter().any(|c| c == &req.boon_id) {
                return ("400 Bad Request", r#"{"error":"boon_id not in offered choices"}"#.to_string());
            }
            if questlib::boons::lookup(&req.boon_id).is_none() {
                return ("400 Bad Request", r#"{"error":"unknown boon_id"}"#.to_string());
            }
            if p.boons.iter().any(|b| b == &req.boon_id) {
                return ("400 Bad Request", r#"{"error":"already owned"}"#.to_string());
            }
            p.boons.push(req.boon_id.clone());
            p.pending_boon_choice = None;
            tracing::info!("[boons] {} selected '{}' (now owns {})", p.name, req.boon_id, p.boons.len());
            return ("200 OK", format!(r#"{{"ok":true,"selected":"{}"}}"#, req.boon_id));
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // POST /start_new_adventure — body: {"player_id": "...", "adventure_id": "chaos"}.
    // Resets level / gold / inventory / equipment / route / fog /
    // opened_chests / defeated_monsters / completed_events / etc.
    // and sets `adventure_id` to the new preset. **Boons survive**
    // (the whole point of the meta-progression system), as do
    // name / champion / walker_uuid / player_id itself so the title
    // screen recognizes the player on rejoin.
    if first_line.starts_with("POST /start_new_adventure") {
        if let Some(body_start) = request.find("\r\n\r\n") {
            #[derive(Deserialize)]
            struct Req { player_id: String, adventure_id: String }
            let Ok(req) = serde_json::from_str::<Req>(&request[body_start + 4..]) else {
                return ("400 Bad Request", r#"{"error":"bad body"}"#.to_string());
            };
            // Validate the target adventure is registered.
            let target_preset = crate::adventure::presets()
                .into_iter()
                .find(|p| p.id == req.adventure_id);
            let Some(target_preset) = target_preset else {
                return ("400 Bad Request", format!(
                    r#"{{"error":"unknown adventure_id '{}'"}}"#, req.adventure_id
                ).to_string());
            };
            // Spawn at the centre of the target world so existing
            // (50, 40) doesn't end up in the top-left quadrant of a
            // bigger map like chaos's 200×160. Each adventure's
            // Survivors' Camp / starting POI lives at this center
            // (see seed{N}_pois.json).
            let spawn_x = (target_preset.map_width / 2) as i32;
            let spawn_y = (target_preset.map_height / 2) as i32;
            let mut lock = state.lock().unwrap();
            let Some(p) = lock.get_mut(&req.player_id) else {
                return ("404 Not Found", r#"{"error":"player not found"}"#.to_string());
            };
            // Reset to a fresh-start state, preserving meta-progression
            // (permanent boons + per-adventure boon stashes) + identity
            // (name, champion, walker_uuid). Adventure-scoped boons
            // earned in past adventures stay parked under their
            // adventure_id key — they re-activate if the player ever
            // switches back to that adventure.
            let id = p.id.clone();
            let name = p.name.clone();
            let champion = p.champion.clone();
            let walker_uuid = p.walker_uuid.clone();
            let boons = p.boons.clone();
            let adventure_boons = p.adventure_boons.clone();
            let prev_adv = p.adventure_id.clone();
            *p = DevPlayerState {
                id,
                name,
                champion,
                walker_uuid,
                boons,
                adventure_boons,
                adventure_id: req.adventure_id.clone(),
                map_tile_x: spawn_x,
                map_tile_y: spawn_y,
                ..Default::default()
            };
            tracing::info!(
                "[adv] {} switched: {} → {} (keeping {} boon{})",
                p.name, prev_adv, req.adventure_id, p.boons.len(),
                if p.boons.len() == 1 { "" } else { "s" },
            );
            return ("200 OK", format!(
                r#"{{"ok":true,"adventure_id":"{}"}}"#, req.adventure_id
            ));
        }
        return ("400 Bad Request", r#"{"error":"bad request"}"#.to_string());
    }

    // POST /forge_upgrade — spend gold to add a stat level to an equipped
    // item. Body: {"player_id": "...", "item_id": "iron_sword"}.
    // Server enforces: item must be equipped, current level < MAX_FORGE_LEVEL,
    // player has enough gold. Cost scales: 500 × (level + 1).
    if first_line.starts_with("POST /forge_upgrade") {
        const MAX_FORGE_LEVEL: u8 = 5;
        const BASE_COST: i32 = 500;
        if let Some(body_start) = request.find("\r\n\r\n") {
            #[derive(Deserialize)]
            struct Req { player_id: String, item_id: String }
            let Ok(req) = serde_json::from_str::<Req>(&request[body_start + 4..]) else {
                return ("400 Bad Request", r#"{"error":"bad body"}"#.to_string());
            };
            let mut lock = state.lock().unwrap();
            let Some(p) = lock.get_mut(&req.player_id) else {
                return ("404 Not Found", r#"{"error":"player"}"#.to_string());
            };
            if !p.equipment.has_equipped(&req.item_id) {
                return ("400 Bad Request", r#"{"error":"item not equipped"}"#.to_string());
            }
            let cur = p.item_upgrades.get(&req.item_id).copied().unwrap_or(0);
            if cur >= MAX_FORGE_LEVEL {
                return ("400 Bad Request", r#"{"error":"already at max level"}"#.to_string());
            }
            // Forge Discount boon (and any future stacking forge mults)
            // applies here; round to nearest int and floor at 1 so the
            // upgrade still costs *something* even with deep discounts.
            let raw_cost = BASE_COST * (cur as i32 + 1);
            let cost = ((raw_cost as f32) * questlib::boons::forge_cost_multiplier(&p.effective_boons()))
                .round()
                .max(1.0) as i32;
            if p.gold < cost {
                return ("400 Bad Request", format!(r#"{{"error":"need {} gold"}}"#, cost));
            }
            p.gold -= cost;
            p.item_upgrades.insert(req.item_id.clone(), cur + 1);
            return ("200 OK", format!(
                r#"{{"ok":true,"new_level":{},"paid":{}}}"#, cur + 1, cost
            ));
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
                                // Update the player's fog bitfield in place — same
                                // pattern used by EventOutcome::RevealFog elsewhere
                                // in this file. Previously this branch dropped the
                                // lock, returned early, and never actually revealed
                                // anything (Explorer's Map looked silent to the
                                // user).
                                // Size the bitfield to the player's bundle's world
                                // dims so chaos players' SE-quadrant reveals
                                // actually land (the 100×80 default cap silently
                                // dropped anything past tile (99, 79)).
                                let (bw, bh) = bundles.get(&player.adventure_id)
                                    .map(|b| (b.world.width, b.world.height))
                                    .unwrap_or((questlib::mapgen::MAP_W, questlib::mapgen::MAP_H));
                                let px = player.map_tile_x as usize;
                                let py = player.map_tile_y as usize;
                                let mut fog = if !player.revealed_tiles.is_empty() {
                                    questlib::fog::FogBitfield::from_base64_sized(&player.revealed_tiles, bw, bh)
                                        .unwrap_or_else(|| questlib::fog::FogBitfield::new_sized(bw, bh))
                                } else {
                                    questlib::fog::FogBitfield::new_sized(bw, bh)
                                };
                                fog.reveal_radius(px, py, *radius as usize);
                                player.revealed_tiles = fog.to_base64();
                                messages.push(format!("revealed area (radius {})", radius));
                            }
                            // Equipment-only passive effect — ignored on consume.
                            questlib::items::ItemEffect::SpeedMultiplier { .. } => {}
                            questlib::items::ItemEffect::BuffSpeed { multiplier, duration_secs } => {
                                let now = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_secs()).unwrap_or(0);
                                // Replace any existing speed buff rather than stacking.
                                // Drinking a second haste draught refreshes the timer +
                                // swaps to the new multiplier, but the tick loop's
                                // .product() over active speed buffs must never see
                                // more than one at a time.
                                player.active_buffs.retain(|b| b.kind != "speed");
                                player.active_buffs.push(questlib::items::ActiveBuff {
                                    kind: "speed".to_string(),
                                    multiplier: *multiplier,
                                    expires_unix: now + *duration_secs as u64,
                                    source_item: req.item_id.clone(),
                                });
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

        // Route to the player's adventure bundle's events. Falls back
        // to the default `events` arg (frost_quest) when the player
        // can't be resolved.
        let bundle = bundle_for_player(player_id);
        let events_ref = bundle.map(|b| &b.events).unwrap_or(events);
        let lock = events_ref.lock().unwrap();
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

    // GET /version — returns the cache-bust number baked into index.html
    // (`?v=N` in the import line). Clients poll this and compare against
    // their own compiled-in CLIENT_VERSION to detect a stale bundle after
    // a redeploy. Re-read on every request: the file is tiny, the
    // endpoint is cold, and caching it caused stale-server bugs during
    // local dev (long-running gamemaster keeps reporting the version it
    // saw at boot even after index.html is bumped on disk).
    if first_line.starts_with("GET /version") {
        let v = std::fs::read_to_string("crates/gameclient/index.html")
            .ok()
            .and_then(|s| s.find("gameclient.js?v=").map(|i| (s, i)))
            .and_then(|(s, i)| {
                let tail = &s[i + "gameclient.js?v=".len()..];
                let n: String = tail.chars().take_while(|c| c.is_ascii_digit()).collect();
                n.parse::<u32>().ok()
            })
            .unwrap_or(0);
        return ("200 OK", format!(r#"{{"version":{}}}"#, v));
    }

    // GET /daynight — server-authoritative day/night cycle position so
    // every connected client sees the same sun. Stateless: derived
    // purely from UNIX time, so restarts / deploys don't jump the
    // cycle. Clients poll periodically and resync local time_s.
    if first_line.starts_with("GET /daynight") {
        const CYCLE_SECONDS: f32 = 300.0;
        let now_f64 = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        let time_s = now_f64.rem_euclid(CYCLE_SECONDS as f64) as f32;
        return (
            "200 OK",
            format!(r#"{{"time_s":{:.3},"cycle_seconds":{}}}"#, time_s, CYCLE_SECONDS),
        );
    }

    // GET /world/pois?adventure_id=X — full POI list (procedural + authored)
    // for the named adventure. Client uses this to overlay the authored
    // chaos POIs (castles, gates, camp, spire) on top of its local
    // procedural world — without this the chaos players see only the
    // 23 procedural POIs, not the 10 authored landmarks the quest
    // chain pivots around.
    if first_line.starts_with("GET /world/pois") {
        let adv_id = first_line.split("adventure_id=").nth(1)
            .and_then(|s| s.split(|c: char| c == ' ' || c == '&').next())
            .unwrap_or("frost_quest");
        let Some(bundle) = bundles.get(adv_id) else {
            return ("404 Not Found", format!(
                r#"{{"error":"unknown adventure_id '{}'"}}"#, adv_id
            ));
        };
        // Serialize the POI list. Each POI is small (id, type, x, y,
        // biome, has_road, name, description); even 33 of them comes
        // in under a few KB.
        let body = serde_json::to_string(&bundle.world.pois)
            .unwrap_or_else(|_| "[]".to_string());
        return ("200 OK", body);
    }

    // GET /adventures?player_id=X — list adventures the player can
    // advance INTO from their current one. One-way trip: the switch
    // button only offers an adventure once the player has completed
    // their current adventure's `completion_event_id`. Returns
    // `{ current, current_completed, available: [{id, display_name}] }`.
    if first_line.starts_with("GET /adventures") {
        let pid = first_line.split("player_id=").nth(1)
            .and_then(|s| s.split(|c: char| c == ' ' || c == '&').next())
            .unwrap_or("");
        let (current_id, current_completed) = {
            let lock = state.lock().unwrap();
            let Some(p) = lock.get(pid) else {
                return ("404 Not Found", r#"{"error":"player not found"}"#.to_string());
            };
            let current = if p.adventure_id.is_empty() {
                crate::adventure::DEFAULT_ADVENTURE_ID.to_string()
            } else {
                p.adventure_id.clone()
            };
            let presets = crate::adventure::presets();
            let completed = presets.iter()
                .find(|preset| preset.id == current)
                .and_then(|preset| preset.completion_event_id.as_ref())
                .map(|eid| p.completed_events.contains(eid))
                .unwrap_or(false);
            (current, completed)
        };
        // Build the list of available adventures: anything that isn't
        // the player's current adventure AND that they haven't already
        // completed. Filtering by current_completed = false collapses
        // the list to empty so the client hides the switcher entirely
        // until the player earns the right to advance.
        let mut available_entries = Vec::new();
        if current_completed {
            let lock = state.lock().unwrap();
            let p = lock.get(pid).unwrap();
            let already_completed: std::collections::HashSet<String> = crate::adventure::presets()
                .into_iter()
                .filter(|preset| {
                    preset.completion_event_id.as_ref()
                        .map(|eid| p.completed_events.contains(eid))
                        .unwrap_or(false)
                })
                .map(|preset| preset.id)
                .collect();
            for preset in crate::adventure::presets() {
                if preset.id == current_id { continue; }
                if already_completed.contains(&preset.id) { continue; }
                available_entries.push(format!(
                    r#"{{"id":"{}","display_name":"{}"}}"#,
                    preset.id, preset.display_name
                ));
            }
        }
        return ("200 OK", format!(
            r#"{{"current":"{}","current_completed":{},"available":[{}]}}"#,
            current_id, current_completed, available_entries.join(",")
        ));
    }

    // GET /shops?player_id=X — shops the player has already discovered
    // (i.e. in their revealed_shops list). Phase A: a shop lands here
    // after the player visits it once. Phase B (future): NPCs can also
    // reveal shops via a RevealShop outcome.
    if first_line.starts_with("GET /shops") {
        let player_id = first_line.split('?').nth(1)
            .and_then(|qs| qs.split('&').find(|p| p.starts_with("player_id=")))
            .and_then(|p| p.strip_prefix("player_id="))
            .and_then(|v| v.split_whitespace().next())
            .unwrap_or("");
        if player_id.is_empty() {
            return ("400 Bad Request", r#"{"error":"player_id required"}"#.to_string());
        }
        let revealed: Vec<String> = state.lock().ok()
            .and_then(|s| s.get(player_id).map(|p| p.revealed_shops.clone()))
            .unwrap_or_default();

        // Route to the player's bundle's events.
        let bundle = bundle_for_player(player_id);
        let events_ref = bundle.map(|b| &b.events).unwrap_or(events);
        let world_ref = bundle.map(|b| b.world.as_ref()).unwrap_or(world);
        let lock = events_ref.lock().unwrap();
        #[derive(serde::Serialize)]
        struct ShopMarker<'a> {
            id: &'a str,
            name: &'a str,
            tile_x: i32,
            tile_y: i32,
        }
        let markers: Vec<ShopMarker> = lock.events.iter()
            .filter(|e| revealed.contains(&e.id))
            .filter_map(|e| {
                use questlib::events::kind::EventKind;
                use questlib::events::trigger::TriggerCondition;
                let merchant = match &e.kind {
                    EventKind::Shop { merchant_name, .. } => merchant_name.as_str(),
                    _ => return None,
                };
                // Resolve the trigger to a (tile_x, tile_y). Shop triggers
                // in the current dataset are AtPoi or AtTile; anything else
                // we quietly skip (can't place a map marker without coords).
                let (x, y) = match &e.trigger {
                    TriggerCondition::AtTile { x, y } => (*x as i32, *y as i32),
                    TriggerCondition::AtPoi { poi_id } => {
                        let p = world_ref.pois.iter().find(|p| p.id == *poi_id)?;
                        (p.x as i32, p.y as i32)
                    }
                    _ => return None,
                };
                Some(ShopMarker { id: &e.id, name: merchant, tile_x: x, tile_y: y })
            })
            .collect();
        return ("200 OK", serde_json::to_string(&markers).unwrap_or_default());
    }

    // `GET /events` (whole catalog) was removed — clients now use
    // `/events/active?player_id=X` which filters by per-player completion.
    // Leaving the whole catalog reachable leaked other players' completion state.

    // GET /journal?player_id=X — "story so far" view of completed events for
    // a specific player. Returns id/name/description for each completed
    // event, preserving completion order. Shops and environmental effects
    // are filtered out — they're not story achievements.
    if first_line.starts_with("GET /journal") {
        let player_id = first_line.split('?').nth(1)
            .and_then(|qs| qs.split('&').find(|p| p.starts_with("player_id=")))
            .and_then(|p| p.strip_prefix("player_id="))
            .and_then(|v| v.split_whitespace().next())
            .unwrap_or("");
        if player_id.is_empty() {
            return ("400 Bad Request", r#"{"error":"player_id required"}"#.to_string());
        }
        let completed: Vec<String> = state.lock().ok()
            .and_then(|s| s.get(player_id).map(|p| p.completed_events.clone()))
            .unwrap_or_default();

        // Route to the player's bundle's events.
        let bundle = bundle_for_player(player_id);
        let events_ref = bundle.map(|b| &b.events).unwrap_or(events);
        let lock = events_ref.lock().unwrap();
        #[derive(serde::Serialize)]
        struct JournalEntry<'a> {
            id: &'a str,
            name: &'a str,
            description: &'a str,
            kind: &'static str,
            /// Full dialogue / story text for kinds that carry it. Empty for
            /// treasure / encounter / cave / boss (those don't have lines).
            /// Client shows this expanded when a journal entry is clicked.
            lines: Vec<String>,
        }
        let entries: Vec<JournalEntry> = completed.iter()
            .filter_map(|id| lock.events.iter().find(|e| &e.id == id))
            .filter(|e| {
                use questlib::events::kind::EventKind::*;
                !matches!(e.kind, Shop { .. } | Forge { .. } | EnvironmentalEffect { .. })
            })
            .map(|e| {
                use questlib::events::kind::EventKind::*;
                let (kind, lines) = match &e.kind {
                    NpcDialogue { lines, .. } => ("dialogue", lines.clone()),
                    Treasure { .. } => ("treasure", Vec::new()),
                    RandomEncounter { .. } => ("encounter", Vec::new()),
                    Quest { description, .. } => ("quest", vec![description.clone()]),
                    Boss { dialogue_intro, .. } => ("boss", dialogue_intro.clone()),
                    StoryBeat { lines } => ("story", lines.clone()),
                    CaveEntrance { flavor, .. } => {
                        ("cave", if flavor.is_empty() { Vec::new() } else { vec![flavor.clone()] })
                    }
                    Shop { .. } | Forge { .. } | EnvironmentalEffect { .. } => ("misc", Vec::new()),
                };
                JournalEntry { id: &e.id, name: &e.name, description: &e.description, kind, lines }
            })
            .collect();
        return ("200 OK", serde_json::to_string(&entries).unwrap_or_default());
    }

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
                //
                // Route to the player's bundle's events. Otherwise a chaos
                // player completing a chaos event would mutate frost_quest's
                // catalog (or vice versa).
                let bundle = bundle_for_player(&body_player_id);
                let events_ref = bundle.map(|b| &b.events).unwrap_or(events);
                let mut events_lock = events_ref.lock().unwrap();
                if let Some(event) = events_lock.get_mut(event_id) {
                    if event.transition(questlib::events::EventStatus::Completed).is_ok() {
                        let outcomes = event.outcomes.clone();
                        let repeatable = event.repeatable;
                        let is_shop = matches!(event.kind, questlib::events::EventKind::Shop { .. });
                        if repeatable {
                            event.force_status(questlib::events::EventStatus::Pending);
                        }
                        let mut state_lock = state.lock().unwrap();
                        let player = state_lock.get_mut(&body_player_id);
                        if let Some(player) = player {
                            if !player.completed_events.contains(&event_id.to_string()) {
                                player.completed_events.push(event_id.to_string());
                            }
                            // First time visiting a shop → pin it on the map.
                            if is_shop && !player.revealed_shops.contains(&event_id.to_string()) {
                                player.revealed_shops.push(event_id.to_string());
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
                                        // Size to bundle world dims — see /use_item
                                        // for the same fix rationale.
                                        let (bw, bh) = bundles.get(&player.adventure_id)
                                            .map(|b| (b.world.width, b.world.height))
                                            .unwrap_or((questlib::mapgen::MAP_W, questlib::mapgen::MAP_H));
                                        let mut fog = if !player.revealed_tiles.is_empty() {
                                            questlib::fog::FogBitfield::from_base64_sized(&player.revealed_tiles, bw, bh)
                                                .unwrap_or_else(|| questlib::fog::FogBitfield::new_sized(bw, bh))
                                        } else {
                                            questlib::fog::FogBitfield::new_sized(bw, bh)
                                        };
                                        fog.reveal_radius(*x, *y, *radius);
                                        player.revealed_tiles = fog.to_base64();
                                    }
                                    questlib::events::EventOutcome::Notification { text } => {
                                        if let Ok(mut n) = notifs.lock() {
                                            crate::push_notif(&mut n, &body_player_id, text.clone());
                                        }
                                    }
                                    questlib::events::EventOutcome::RevealShop { shop_event_id } => {
                                        if !player.revealed_shops.contains(shop_event_id) {
                                            player.revealed_shops.push(shop_event_id.clone());
                                        }
                                    }
                                    questlib::events::EventOutcome::AdventureBoon { boon_id } => {
                                        // Validate the boon exists; silently drop
                                        // typos rather than letting bad ids creep
                                        // into save state. Push to the player's
                                        // CURRENT adventure bucket — these boons
                                        // only apply while in this adventure.
                                        if questlib::boons::lookup(boon_id).is_some() {
                                            let adv = player.adventure_id.clone();
                                            let bucket = player.adventure_boons
                                                .entry(adv)
                                                .or_default();
                                            if !bucket.contains(boon_id) {
                                                bucket.push(boon_id.clone());
                                                tracing::info!(
                                                    "[boons] {} earned adventure-boon '{}' in '{}'",
                                                    player.name, boon_id, player.adventure_id,
                                                );
                                            }
                                        } else {
                                            tracing::warn!(
                                                "[boons] event-outcome references unknown boon '{}'", boon_id
                                            );
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
            // enter_interior never returns Locked (no unlock gate on entry);
            // kept exhaustive for the compiler.
            PortalTransitionResult::Locked { .. } => return ("500 Internal Server Error", r#"{"error":"unexpected locked on enter"}"#.to_string()),
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
            PortalTransitionResult::Locked { label } => return ("403 Forbidden", format!(r#"{{"error":"locked","label":"{}"}}"#, label.replace('"', "\\\""))),
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

        // POST /admin/reset_event — { event_id, status, player_id? }
        // Sets the global (bundle-scoped) event status. When `player_id`
        // is provided, routes to that player's adventure bundle so
        // chaos-only events can be reset without touching the
        // frost_quest catalog. Without it, falls back to the default
        // catalog for backward compatibility.
        if first_line.starts_with("POST /admin/reset_event") {
            let event_id = data.get("event_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let status_str = data.get("status").and_then(|v| v.as_str()).unwrap_or("pending");
            let pid = data.get("player_id").and_then(|v| v.as_str()).map(|s| s.to_string());
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
            let events_ref = pid
                .as_deref()
                .and_then(|p| bundle_for_player(p))
                .map(|b| &b.events)
                .unwrap_or(events);
            let mut events_lock = events_ref.lock().unwrap();
            let Some(event) = events_lock.get_mut(&event_id) else {
                return ("404 Not Found", r#"{"error":"event not found"}"#.to_string());
            };
            event.force_status(new_status);
            tracing::info!("[admin] reset_event {} -> {:?}", event_id, new_status);
            return ("200 OK", r#"{"ok":true}"#.to_string());
        }

        // POST /admin/grant_boon_choice — { player_id, event_id? }
        // Queue a boon picker for a player who's already past a
        // climactic event (e.g. existing save where the player beat
        // the boss before grants_boon was added to that event). The
        // 3 offered ids are deterministic on (player_id, event_id),
        // so re-grants without a select in between yield the same set.
        if first_line.starts_with("POST /admin/grant_boon_choice") {
            let pid = data.get("player_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let event_id = data.get("event_id").and_then(|v| v.as_str()).unwrap_or("manual_grant").to_string();
            if pid.is_empty() {
                return ("400 Bad Request", r#"{"error":"player_id required"}"#.to_string());
            }
            let mut lock = state.lock().unwrap();
            let Some(p) = lock.get_mut(&pid) else {
                return ("404 Not Found", r#"{"error":"player not found"}"#.to_string());
            };
            let seed = questlib::boons::choice_seed(&p.id, &event_id);
            let choices: Vec<String> = questlib::boons::pick_choices(seed, 3, &p.boons)
                .into_iter().map(String::from).collect();
            if choices.is_empty() {
                return ("400 Bad Request", r#"{"error":"player already owns every boon"}"#.to_string());
            }
            p.pending_boon_choice = Some(PendingBoonChoice {
                event_id: event_id.clone(),
                choices: choices.clone(),
            });
            tracing::info!("[admin] grant_boon_choice player={} event={} choices={:?}", p.name, event_id, choices);
            let json = serde_json::json!({ "ok": true, "choices": choices }).to_string();
            return ("200 OK", json);
        }

        // POST /admin/clear_boon_choice — { player_id } — drop the
        // player's pending boon picker without consuming it. Use to
        // reverse a misdirected grant_boon_choice.
        if first_line.starts_with("POST /admin/clear_boon_choice") {
            let pid = data.get("player_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            if pid.is_empty() {
                return ("400 Bad Request", r#"{"error":"player_id required"}"#.to_string());
            }
            let mut lock = state.lock().unwrap();
            let Some(p) = lock.get_mut(&pid) else {
                return ("404 Not Found", r#"{"error":"player not found"}"#.to_string());
            };
            let had = p.pending_boon_choice.is_some();
            p.pending_boon_choice = None;
            tracing::info!("[admin] clear_boon_choice player={} (had pending: {})", p.name, had);
            return ("200 OK", format!(r#"{{"ok":true,"had_pending":{}}}"#, had));
        }

        // POST /admin/teleport — { player_id, x, y }
        // Directly sets a player's tile, no route required. Used by
        // tools/chaos_smoketest.rb to drive a player through the
        // chaos quest chain without making them walk on a treadmill.
        if first_line.starts_with("POST /admin/teleport") {
            let pid = data.get("player_id").and_then(|v| v.as_str()).unwrap_or("").to_string();
            let x = data.get("x").and_then(|v| v.as_i64()).unwrap_or(-1);
            let y = data.get("y").and_then(|v| v.as_i64()).unwrap_or(-1);
            if pid.is_empty() || x < 0 || y < 0 {
                return ("400 Bad Request", r#"{"error":"player_id, x>=0, y>=0 required"}"#.to_string());
            }
            let mut lock = state.lock().unwrap();
            let Some(p) = lock.get_mut(&pid) else {
                return ("404 Not Found", r#"{"error":"player not found"}"#.to_string());
            };
            p.prev_tile = Some((p.map_tile_x, p.map_tile_y));
            p.map_tile_x = x as i32;
            p.map_tile_y = y as i32;
            p.planned_route = String::new();
            p.route_meters_walked = 0.0;
            // Always teleport TO the overworld. CaveEntrance events
            // would otherwise leave the player flagged as
            // `Location::Interior` from a prior step and the next
            // tick would short-circuit overworld trigger eval.
            p.location = questlib::interior::Location::Overworld;
            tracing::info!("[admin] teleport {} to ({}, {})", p.name, x, y);
            return ("200 OK", format!(r#"{{"ok":true,"x":{},"y":{}}}"#, x, y));
        }

        // POST /admin/dump_combat — snapshot of in-memory shared_combat
        // (keys + brief status). Diagnostic only — use this when a player
        // sits on a monster and combat won't start; a non-empty list with
        // a `mobile_monster:<id>` key for the entity in question means a
        // previous fight didn't get cleaned up.
        if first_line.starts_with("POST /admin/dump_combat") {
            let lock = combat.lock().unwrap();
            let keys: Vec<serde_json::Value> = lock.iter().map(|(k, c)| {
                serde_json::json!({
                    "event_id": k,
                    "player_id": c.player_id,
                    "status": format!("{:?}", c.status),
                    "player_hp": c.player_hp,
                    "enemy_hp": c.enemy_hp,
                })
            }).collect();
            return ("200 OK", serde_json::to_string(&keys).unwrap_or_else(|_| "[]".into()));
        }

        // POST /admin/clear_combat — drain shared_combat. Recovery hatch
        // for the stuck-entry case where the contact-skip log fires but
        // there's no live fight. Optional `event_id` clears just one.
        if first_line.starts_with("POST /admin/clear_combat") {
            let target = data.get("event_id").and_then(|v| v.as_str()).map(|s| s.to_string());
            let mut lock = combat.lock().unwrap();
            let removed: Vec<String> = if let Some(ref id) = target {
                if lock.remove(id).is_some() { vec![id.clone()] } else { vec![] }
            } else {
                let keys: Vec<String> = lock.keys().cloned().collect();
                lock.clear();
                keys
            };
            tracing::info!("[admin] clear_combat removed {:?}", removed);
            return ("200 OK", serde_json::to_string(&removed).unwrap_or_else(|_| "[]".into()));
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
