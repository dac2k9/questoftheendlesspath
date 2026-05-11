mod adventure;
mod combat;
mod devserver;
mod interior;
mod mobile_entity;
mod tick;
mod walker_bridge;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use questlib::events::EventCatalog;
use tracing::{error, info};

use devserver::{DevPlayerState, SharedState};

pub type SharedEvents = Arc<Mutex<EventCatalog>>;
pub type SharedNotifs = Arc<Mutex<HashMap<String, Vec<String>>>>;

/// Max pending notifications per player — oldest dropped beyond this.
/// Prevents the queue growing unbounded for players who never poll.
pub const NOTIF_QUEUE_MAX: usize = 100;

/// Push a notification to a player's queue, capped at NOTIF_QUEUE_MAX.
pub fn push_notif(notifs: &mut HashMap<String, Vec<String>>, player_id: &str, msg: String) {
    let q = notifs.entry(player_id.to_string()).or_default();
    q.push(msg);
    if q.len() > NOTIF_QUEUE_MAX {
        let drop = q.len() - NOTIF_QUEUE_MAX;
        q.drain(..drop);
    }
}

/// Lazy-loaded, process-wide item catalog. Parsed once from the embedded JSON.
pub fn item_catalog() -> &'static questlib::items::ItemCatalog {
    static CATALOG: std::sync::OnceLock<questlib::items::ItemCatalog> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        questlib::items::ItemCatalog::from_json(include_str!("../../../adventures/items.json"))
            .unwrap_or_default()
    })
}

/// Current save-file schema version. Bump whenever a breaking change to the
/// on-disk representation happens, and extend `migrate_save` to handle it.
const SAVE_VERSION: u32 = 1;

#[derive(serde::Serialize, serde::Deserialize)]
struct SaveData {
    /// Schema version. Defaults to 1 if absent (old saves written before
    /// versioning existed).
    #[serde(default = "default_save_version")]
    version: u32,
    players: Vec<DevPlayerState>,
    events: EventCatalog,
    /// Per-entity runtime state (current tile, alive/dead, respawn
    /// timer). Authored fields (sprite, behavior, etc.) load fresh
    /// from JSON every startup — only mutable bits live here. Empty
    /// for old saves; populated from JSON via ensure_states.
    #[serde(default)]
    mobile_entities: HashMap<String, questlib::mobile_entity::MobileEntityState>,
}

fn default_save_version() -> u32 { 1 }

/// Migrate a loaded SaveData to the current SAVE_VERSION in place. No-op
/// today — first real break in the schema will add its migration arm here.
fn migrate_save(save: &mut SaveData) {
    let from = save.version;
    if from == SAVE_VERSION { return; }
    info!("Migrating save from version {} to {}", from, SAVE_VERSION);
    // Example shape for future migrations:
    //   while save.version < SAVE_VERSION {
    //       match save.version {
    //           1 => { /* rewrite v1 → v2 */ save.version = 2; }
    //           _ => break,
    //       }
    //   }
    save.version = SAVE_VERSION;
}

/// Drop inventory slots / equipment references that point at item ids no
/// longer present in the catalog. Prevents "ghost" items after a rename or
/// removal from items.json.
fn prune_missing_items(players: &mut HashMap<String, DevPlayerState>, catalog: &questlib::items::ItemCatalog) {
    for (_, p) in players.iter_mut() {
        let mut dropped = Vec::new();
        p.inventory.retain(|slot| {
            if catalog.get(&slot.item_id).is_some() { true }
            else { dropped.push(slot.item_id.clone()); false }
        });
        for slot in questlib::items::EquipmentLoadout::all_slots() {
            if let Some(id) = p.equipment.get_slot(slot).map(|s| s.to_string()) {
                if catalog.get(&id).is_none() {
                    // Clearing the slot via set_slot(None) would require a pub setter;
                    // we reach in directly via match since we control the struct.
                    match slot {
                        questlib::items::EquipmentSlot::Weapon    => p.equipment.weapon = None,
                        questlib::items::EquipmentSlot::Armor     => p.equipment.armor = None,
                        questlib::items::EquipmentSlot::Accessory => p.equipment.accessory = None,
                        questlib::items::EquipmentSlot::Feet      => p.equipment.feet = None,
                        questlib::items::EquipmentSlot::ToeRings  => p.equipment.toe_rings = None,
                    }
                    dropped.push(id);
                }
            }
        }
        if !dropped.is_empty() {
            info!("[{}] dropped {} missing-item reference(s): {:?}", p.name, dropped.len(), dropped);
        }
    }
}

/// Derive revealed_shops from completed_events for players who played
/// before the field existed. Idempotent — safe to run every startup.
///
/// Two sources feed revealed_shops:
///   1. A directly-completed shop event id — the player has been to the
///      shop. (In practice shops are repeatable and usually don't land
///      in completed_events, but we check just in case one does.)
///   2. Any completed event with a RevealShop outcome — the player
///      finished an NPC dialogue that told them about a shop. Applies
///      retroactively so outcomes added to pre-completed events still
///      take effect.
fn backfill_revealed_shops(
    players: &mut HashMap<String, DevPlayerState>,
    catalog: &questlib::events::EventCatalog,
) {
    use questlib::events::{kind::EventKind, EventOutcome};
    // Index shop event ids and completed-event → revealed-shop mappings.
    let shop_ids: std::collections::HashSet<&str> = catalog.events.iter()
        .filter(|e| matches!(e.kind, EventKind::Shop { .. }))
        .map(|e| e.id.as_str())
        .collect();
    let reveals_by_event: std::collections::HashMap<&str, Vec<&str>> = catalog.events.iter()
        .map(|e| (e.id.as_str(), e.outcomes.iter().filter_map(|o| match o {
            EventOutcome::RevealShop { shop_event_id } => Some(shop_event_id.as_str()),
            _ => None,
        }).collect::<Vec<_>>()))
        .filter(|(_, v)| !v.is_empty())
        .collect();

    for (_, p) in players.iter_mut() {
        for eid in &p.completed_events.clone() {
            // (1) Direct shop completion.
            if shop_ids.contains(eid.as_str()) && !p.revealed_shops.contains(eid) {
                p.revealed_shops.push(eid.clone());
            }
            // (2) RevealShop outcomes on any completed event.
            if let Some(shop_ids) = reveals_by_event.get(eid.as_str()) {
                for shop_id in shop_ids {
                    let s = shop_id.to_string();
                    if !p.revealed_shops.contains(&s) {
                        p.revealed_shops.push(s);
                    }
                }
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let _ = rustls::crypto::ring::default_provider().install_default();

    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "gamemaster=info".parse().expect("valid filter")),
        )
        .init();

    dotenvy::dotenv().ok();

    // Save path is configurable via SAVE_PATH. On Render, set this to a path
    // inside a mounted persistent disk (e.g. "/data/dev_state.json") so state
    // survives redeploys. Locally, defaults to the working directory.
    let save_path_string = std::env::var("SAVE_PATH")
        .unwrap_or_else(|_| "dev_state.json".to_string());
    let save_path = save_path_string.as_str();
    if let Some(parent) = std::path::Path::new(save_path).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    info!("Save path: {}", save_path);

    // Load every registered adventure into a single map keyed by
    // adventure_id. Each player's `adventure_id` picks which bundle
    // their tick + endpoints run against. For now all interiors are
    // shared, but world/events/entities are per-adventure.
    let bundles: Arc<HashMap<String, adventure::AdventureBundle>> = {
        let mut map = HashMap::new();
        for preset in adventure::presets() {
            let id = preset.id.clone();
            let b = adventure::load_bundle(preset)?;
            map.insert(id, b);
        }
        Arc::new(map)
    };
    info!("Loaded {} adventure bundle(s): {:?}",
        bundles.len(),
        bundles.keys().collect::<Vec<_>>(),
    );

    // Seed mobile-entity runtime state for each bundle from the save
    // file. Save format is single-adventure today (all entity states
    // under one key) — we apply it to the default bundle only.
    // Future: split saved states per adventure_id when we have
    // entities to save across multiple worlds.
    {
        let saved = load_mobile_entities(save_path);
        if let Some(default) = bundles.get(adventure::DEFAULT_ADVENTURE_ID) {
            if let Ok(mut s) = default.entity_states.lock() {
                *s = saved;
                mobile_entity::ensure_states(&default.entity_defs, &mut s);
                info!("[{}] Mobile entities runtime state: {} active",
                    adventure::DEFAULT_ADVENTURE_ID, s.len());
            }
        }
        // Make sure other bundles have a fresh-init runtime state too.
        for (id, bundle) in bundles.iter() {
            if id == adventure::DEFAULT_ADVENTURE_ID { continue }
            if let Ok(mut s) = bundle.entity_states.lock() {
                mobile_entity::ensure_states(&bundle.entity_defs, &mut s);
                info!("[{}] Mobile entities runtime state: {} active (fresh)", id, s.len());
            }
        }
    }

    // Overlay saved per-event status flags onto the freshly-loaded
    // events catalogs so completed quests stay completed across
    // restarts. Saved events are keyed by event_id only (no adventure
    // scope), so we apply them to whichever bundle's catalog contains
    // a matching id — adventure-specific event_ids prevent collisions.
    {
        let saved_events = std::fs::read_to_string(save_path)
            .ok()
            .and_then(|json| serde_json::from_str::<SaveData>(&json).ok())
            .map(|s| s.events);
        if let Some(saved) = saved_events {
            for bundle in bundles.values() {
                if let Ok(mut catalog) = bundle.events.lock() {
                    let mut carried = 0;
                    for event in catalog.events.iter_mut() {
                        if let Some(prev) = saved.events.iter().find(|e| e.id == event.id) {
                            if prev.status != event.status {
                                event.force_status(prev.status);
                                carried += 1;
                            }
                        }
                    }
                    if carried > 0 {
                        info!("[{}] Merged {} event status flag(s) from saved state",
                            bundle.preset.id, carried);
                    }
                }
            }
        }
    }

    // Legacy refs pointing at the default bundle's pieces, so the
    // save-loading + devserver + Walker-bridge code below keeps
    // compiling. The tick loop iterates ALL bundles (per-adventure).
    // Endpoints will be refactored to look up by player.adventure_id
    // in a follow-up chunk.
    let default_bundle = bundles.get(adventure::DEFAULT_ADVENTURE_ID)
        .expect("default adventure registered");
    let world = default_bundle.world.clone();
    let interiors = default_bundle.interiors.clone();
    let entity_defs = default_bundle.entity_defs.clone();
    let entity_states = default_bundle.entity_states.clone();
    let shared_events = default_bundle.events.clone();

    // Initialize shared player state — load from disk or start empty
    let state: SharedState = Arc::new(Mutex::new({
        let mut loaded = load_state(save_path).unwrap_or_else(|| {
            info!("No saved state found, players will join via /join");
            HashMap::new()
        });
        // Drop references to items that no longer exist in the catalog (renames etc.)
        prune_missing_items(&mut loaded, item_catalog());
        // Populate revealed_shops for players whose completed_events already
        // includes a shop — otherwise existing saves would show no shop
        // markers until the player revisits each one.
        if let Ok(cat) = shared_events.lock() {
            backfill_revealed_shops(&mut loaded, &cat);
        }
        loaded
    }));

    let shared_notifs: SharedNotifs = Arc::new(Mutex::new(HashMap::new()));
    let shared_combat: combat::SharedCombat = Arc::new(Mutex::new(HashMap::new()));
    let tick_signal = devserver::new_tick_signal();

    // Start dev HTTP server
    // Track which players have active Walker bridges
    let bridged_players: walker_bridge::BridgedPlayers = Arc::new(Mutex::new(std::collections::HashSet::new()));

    let server_state = state.clone();
    let server_events = shared_events.clone();
    let server_notifs = shared_notifs.clone();
    let server_world = world.clone();
    let server_combat = shared_combat.clone();
    let server_tick_signal = tick_signal.clone();
    let server_bridged = bridged_players.clone();
    let server_interiors = interiors.clone();
    let server_entity_defs = entity_defs.clone();
    let server_entity_states = entity_states.clone();
    tokio::spawn(async move {
        if let Err(e) = devserver::start_dev_server(server_state, server_events, server_notifs, server_world, server_combat, server_tick_signal, server_bridged, server_interiors, server_entity_defs, server_entity_states).await {
            error!("Dev server error: {e}");
        }
    });

    // Start Walker bridges for saved players that have walker_uuid
    {
        let pairs: Vec<(String, String, String)> = {
            let lock = state.lock().unwrap();
            lock.iter()
                .filter_map(|(pid, p)| p.walker_uuid.as_ref().map(|wid| (pid.clone(), wid.clone(), p.name.clone())))
                .collect()
        };
        for (pid, wid, name) in &pairs {
            walker_bridge::ensure_bridge(state.clone(), bridged_players.clone(), pid, wid);
            info!("Restored Walker bridge: {} -> {}", name, wid);
        }
    }

    // Track per-player state
    let mut player_fogs: HashMap<String, questlib::fog::FogBitfield> = HashMap::new();
    let mut player_last_distance: HashMap<String, f64> = HashMap::new();
    let mut player_boss_wait_notified: HashMap<String, String> = HashMap::new();
    // Fog of war for each (player, interior) pair. Persisted via
    // DevPlayerState.interior_fog; this is the hot-path mirror.
    let mut interior_fogs: HashMap<(String, String), questlib::fog::FogBitfield> = HashMap::new();

    info!("Game Master running (dev mode). Tick interval: 3s. Dev server on :3001");
    let mut interval = tokio::time::interval(Duration::from_secs(1));

    // Simple RNG for random encounter rolls. Seeded from the default
    // adventure's map_seed so the stream is deterministic across runs
    // on the same world.
    let mut rng_state: u64 = default_bundle.preset.map_seed;
    let mut save_counter: u32 = 0;

    // Shutdown signal — on SIGTERM (Render redeploy) or SIGINT (Ctrl-C),
    // do one last save before exiting so we don't lose the last ~30s.
    let mut shutdown = shutdown_signal();

    loop {
        tokio::select! {
            _ = interval.tick() => {
                rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let rng_roll = (rng_state >> 33) as f32 / (u32::MAX as f32);

                let now_unix_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as u64)
                    .unwrap_or(0);

                // Per-adventure tick: each bundle owns its own world,
                // events, mobile entities, and interiors. Players are
                // routed by their `adventure_id`. Interior players in
                // an adventure are ticked first against that bundle's
                // interiors; the overworld pass below short-circuits
                // for them via the Location::Interior guard.
                for (adv_id, bundle) in bundles.iter() {
                    // Per-bundle interior tick.
                    let interior_players: Vec<String> = {
                        let lock = state.lock().unwrap();
                        lock.iter()
                            .filter_map(|(pid, p)| {
                                (p.adventure_id == *adv_id
                                    && p.location.interior_id().is_some())
                                    .then(|| pid.clone())
                            })
                            .collect()
                    };
                    for pid in &interior_players {
                        if let Err(e) = interior::run_interior_tick(
                            &bundle.interiors,
                            &state,
                            &shared_notifs,
                            &shared_combat,
                            &mut player_last_distance,
                            &mut interior_fogs,
                            pid,
                        ) {
                            error!("[{adv_id}] Interior tick error for {pid}: {e:#}");
                        }
                    }

                    if let Err(e) = tick::run_tick_dev(
                        &state,
                        &bundle.world,
                        &bundle.events,
                        &shared_notifs,
                        &shared_combat,
                        &bundle.interiors,
                        &bundle.entity_defs,
                        &bundle.entity_states,
                        &mut player_fogs,
                        &mut player_last_distance,
                        &mut player_boss_wait_notified,
                        rng_roll,
                        adv_id,
                    ) {
                        error!("[{adv_id}] Tick error: {e:#}");
                    }

                    // Mobile entities advance after player ticks so the
                    // contact phase below sees fresh positions both sides.
                    mobile_entity::tick_entities(
                        &bundle.entity_defs,
                        &bundle.entity_states,
                        &bundle.world,
                        now_unix_ms,
                        &mut rng_state,
                        &shared_combat,
                    );
                    mobile_entity::check_contacts(
                        &bundle.entity_defs,
                        &bundle.entity_states,
                        &state,
                        &shared_combat,
                        &shared_notifs,
                        adv_id,
                    );
                }

                // Wake all long-polling clients — they get fresh post-tick state
                tick_signal.tick();

                // Save state to disk every ~30 ticks (~30 seconds)
                save_counter += 1;
                if save_counter % 30 == 0 {
                    save_state(save_path, &state, &shared_events, &entity_states);
                }
            }
            _ = &mut shutdown => {
                info!("Shutdown signal received — saving state and exiting");
                save_state(save_path, &state, &shared_events, &entity_states);
                return Ok(());
            }
        }
    }
}

/// Completes when SIGTERM or SIGINT is received. On non-unix, only SIGINT.
fn shutdown_signal() -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
    Box::pin(async {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{signal, SignalKind};
            let mut term = match signal(SignalKind::terminate()) {
                Ok(s) => s,
                Err(e) => {
                    error!("Failed to install SIGTERM handler: {e}");
                    // Fall back to waiting for Ctrl-C only
                    let _ = tokio::signal::ctrl_c().await;
                    return;
                }
            };
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    })
}

fn load_state(path: &str) -> Option<HashMap<String, DevPlayerState>> {
    let json = std::fs::read_to_string(path).ok()?;
    let mut save: SaveData = serde_json::from_str(&json).ok()?;
    migrate_save(&mut save);
    for p in &save.players {
        info!("  Restored {}: tile=({},{}) gold={} route_m={:.0}", p.name, p.map_tile_x, p.map_tile_y, p.gold, p.route_meters_walked);
    }
    info!("Loaded {} players from {} (schema v{})", save.players.len(), path, save.version);
    Some(save.players.into_iter().map(|p| (p.id.clone(), p)).collect())
}

/// Re-read just the mobile_entities section from disk so the main loop
/// can restore positions / alive flags / respawn timers across
/// restarts. ensure_states fills in fresh state for any def whose
/// saved entry is missing (new entity added) and prunes saved entries
/// for defs that are gone (removed from JSON).
fn load_mobile_entities(path: &str) -> HashMap<String, questlib::mobile_entity::MobileEntityState> {
    let Ok(json) = std::fs::read_to_string(path) else { return HashMap::new() };
    let Ok(save) = serde_json::from_str::<SaveData>(&json) else { return HashMap::new() };
    save.mobile_entities
}

fn save_state(
    path: &str,
    state: &SharedState,
    events: &SharedEvents,
    entity_states: &mobile_entity::SharedEntityStates,
) {
    let save = {
        let lock = match state.lock() {
            Ok(l) => l,
            Err(e) => { error!("save_state: state mutex poisoned: {e}"); return; }
        };
        let events_lock = match events.lock() {
            Ok(l) => l,
            Err(e) => { error!("save_state: events mutex poisoned: {e}"); return; }
        };
        let entity_lock = match entity_states.lock() {
            Ok(l) => l,
            Err(e) => { error!("save_state: entity states mutex poisoned: {e}"); return; }
        };
        SaveData {
            version: SAVE_VERSION,
            players: lock.values().cloned().collect(),
            events: events_lock.clone(),
            mobile_entities: entity_lock.clone(),
        }
    };
    let json = match serde_json::to_string_pretty(&save) {
        Ok(j) => j,
        Err(e) => { error!("save_state: serialize failed: {e}"); return; }
    };
    // Atomic write: write to a temp file in the same directory, then rename
    // over the target. Prevents a mid-write crash from corrupting the save.
    let tmp_path = format!("{}.tmp", path);
    if let Err(e) = std::fs::write(&tmp_path, &json) {
        error!("save_state: write {} failed: {e}", tmp_path);
        return;
    }
    if let Err(e) = std::fs::rename(&tmp_path, path) {
        error!("save_state: rename {} -> {} failed: {e}", tmp_path, path);
    }
}

