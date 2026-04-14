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
}

pub type SharedState = Arc<Mutex<HashMap<String, DevPlayerState>>>;

use crate::SharedEvents;

/// Start the dev HTTP server on port 3001.
pub type SharedNotifs = Arc<Mutex<Vec<String>>>;

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

pub async fn start_dev_server(state: SharedState, events: SharedEvents, notifs: SharedNotifs, world: Arc<questlib::mapgen::WorldMap>, combat: crate::combat::SharedCombat, tick_signal: SharedTickSignal) -> Result<()> {
    let listener = TcpListener::bind("0.0.0.0:3001").await?;
    tracing::info!("Dev server listening on http://127.0.0.1:3001");

    loop {
        let (mut stream, _) = listener.accept().await?;
        let state = state.clone();
        let events = events.clone();
        let notifs = notifs.clone();
        let world = world.clone();
        let combat = combat.clone();
        let tick_signal = tick_signal.clone();

        tokio::spawn(async move {
            let mut buf = vec![0u8; 16384];
            let n = stream.read(&mut buf).await.unwrap_or(0);
            let request = String::from_utf8_lossy(&buf[..n]);
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
            } else {
                handle_request(&request, &state, &events, &notifs, &world, &combat)
            };

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

fn handle_request(request: &str, state: &SharedState, events: &SharedEvents, notifs: &SharedNotifs, world: &questlib::mapgen::WorldMap, combat: &crate::combat::SharedCombat) -> (&'static str, String) {
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
                    let catalog = questlib::items::ItemCatalog::from_json(
                        include_str!("../../../adventures/items.json")
                    ).ok();
                    // Can't buy equipment that's already equipped
                    if player.equipment.has_equipped(&req.item_id) {
                        return ("400 Bad Request", r#"{"error":"already equipped"}"#.to_string());
                    }
                    if questlib::items::add_item(&mut player.inventory, &req.item_id, catalog.as_ref()) {
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
                let catalog = questlib::items::ItemCatalog::from_json(
                    include_str!("../../../adventures/items.json")
                ).ok();
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    // Can't sell key items
                    let is_key = catalog.as_ref()
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
                        let name = catalog.as_ref().and_then(|c| c.get(&req.item_id)).map(|d| d.display_name.as_str()).unwrap_or(&req.item_id);
                        n.push(format!("Sold {} for {} gold", name, price));
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
                let catalog = questlib::items::ItemCatalog::from_json(
                    include_str!("../../../adventures/items.json")
                ).ok();
                let cat = catalog.as_ref();
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
                                    if let Some(cs) = combat_lock.values_mut().next() {
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
                                    if let Some(cs) = combat_lock.values_mut().next() {
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
                                    n.push(format!("Used {}! Area revealed.", def.map(|d| d.display_name.as_str()).unwrap_or("item")));
                                }
                                return ("200 OK", serde_json::to_string(&serde_json::json!({"ok": true, "reveal_fog": {"x": px, "y": py, "radius": radius}})).unwrap());
                            }
                        }
                    }
                    if let Ok(mut n) = notifs.lock() {
                        let item_name = def.map(|d| d.display_name.as_str()).unwrap_or(&req.item_id);
                        n.push(format!("Used {}! {}", item_name, messages.join(", ")));
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
                let catalog = questlib::items::ItemCatalog::from_json(
                    include_str!("../../../adventures/items.json")
                ).unwrap();
                let mut lock = state.lock().unwrap();
                if let Some(player) = lock.get_mut(&req.player_id) {
                    match questlib::items::equip_item(&mut player.equipment, &mut player.inventory, &req.item_id, &catalog) {
                        Ok(_old) => {
                            if let Ok(mut n) = notifs.lock() {
                                let name = catalog.get(&req.item_id).map(|d| d.display_name.as_str()).unwrap_or(&req.item_id);
                                n.push(format!("Equipped {}!", name));
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
                let catalog = questlib::items::ItemCatalog::from_json(
                    include_str!("../../../adventures/items.json")
                ).unwrap();
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

    // GET /events — all events
    if first_line.starts_with("GET /events/active") {
        let lock = events.lock().unwrap();
        let mut result: Vec<_> = lock.active_events().into_iter().cloned().collect();
        // Include pending repeatable events (shops, wells, etc.) — they're
        // permanent POI features not triggered as blocking events.
        for event in &lock.events {
            if event.repeatable && event.status == questlib::events::EventStatus::Pending {
                result.push(event.clone());
            }
        }
        let json = serde_json::to_string(&result).unwrap_or_default();
        return ("200 OK", json);
    }

    if first_line.starts_with("GET /events") {
        let lock = events.lock().unwrap();
        let json = lock.to_json();
        return ("200 OK", json);
    }

    // POST /events/{id}/complete — mark event as completed and apply outcomes
    if first_line.contains("/events/") && first_line.contains("/complete") {
        // Extract player_id from body (if provided)
        let body_player_id = request.find("\r\n\r\n")
            .and_then(|i| serde_json::from_str::<serde_json::Value>(&request[i + 4..]).ok())
            .and_then(|v| v.get("player_id")?.as_str().map(|s| s.to_string()));

        // Extract event id from URL: POST /events/some_id/complete
        let parts: Vec<&str> = first_line.split('/').collect();
        if parts.len() >= 3 {
            let event_id = parts.iter()
                .position(|&p| p == "events")
                .and_then(|i| parts.get(i + 1))
                .map(|s| s.split_whitespace().next().unwrap_or(s));

            if let Some(event_id) = event_id {
                let mut events_lock = events.lock().unwrap();
                if let Some(event) = events_lock.get_mut(event_id) {
                    if event.transition(questlib::events::EventStatus::Completed).is_ok() {
                        let outcomes = event.outcomes.clone();
                        let repeatable = event.repeatable;
                        if repeatable {
                            event.force_status(questlib::events::EventStatus::Pending);
                        }
                        drop(events_lock);
                        let mut state_lock = state.lock().unwrap();
                        // Use player_id from body, or fall back to first player
                        let player = if let Some(ref pid) = body_player_id {
                            state_lock.get_mut(pid)
                        } else {
                            state_lock.values_mut().next()
                        };
                        if let Some(player) = player {
                            for outcome in &outcomes {
                                match outcome {
                                    questlib::events::EventOutcome::Gold { amount } => {
                                        player.gold += amount;
                                    }
                                    questlib::events::EventOutcome::Item { name } => {
                                        let cat = questlib::items::ItemCatalog::from_json(
                                            include_str!("../../../adventures/items.json")
                                        ).ok();
                                        questlib::items::add_item(&mut player.inventory, name, cat.as_ref());
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
                                            n.push(text.clone());
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

    // GET /combat — current combat state (or null)
    if first_line.starts_with("GET /combat") {
        if let Some(state) = crate::combat::get_active_combat(combat) {
            let json = serde_json::to_string(&state).unwrap_or_default();
            return ("200 OK", json);
        }
        return ("200 OK", "null".to_string());
    }

    // POST /combat/flee — player runs away
    if first_line.starts_with("POST /combat/flee") {
        let event_id = crate::combat::get_active_combat(combat).map(|c| c.event_id);
        if let Some(eid) = event_id {
            if let Some(updated) = crate::combat::flee(combat, &eid) {
                let json = serde_json::to_string(&updated).unwrap_or_default();
                return ("200 OK", json);
            }
        }
        return ("400 Bad Request", r#"{"error":"no active combat"}"#.to_string());
    }

    // POST /heartbeat — mark player browser as open (no-op for dev)
    if first_line.starts_with("POST /heartbeat") {
        return ("200 OK", r#"{"ok":true}"#.to_string());
    }

    ("404 Not Found", r#"{"error":"not found"}"#.to_string())
}

fn sell_price(item_id: &str) -> i32 {
    let base = match item_id {
        "wooden_club" => 40, "iron_sword" => 120, "fire_blade" => 200, "frost_axe" => 180,
        "leather_vest" => 50, "chainmail" => 150, "dragonscale_armor" => 300,
        "warm_cloak" => 60, "bog_charm" => 60, "ring_of_vigor" => 100, "berserker_pendant" => 80,
        "health_potion" => 30, "greater_health_potion" => 60, "speed_potion" => 80,
        "mystery_potion" => 40, "battle_elixir" => 120,
        "torch" => 20, "compass" => 60, "explorers_map" => 180,
        _ => 20,
    };
    (base / 2).max(5)
}
