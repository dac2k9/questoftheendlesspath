mod combat;
mod devserver;
mod interior;
mod tick;
mod walker_bridge;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use questlib::events::EventCatalog;
use questlib::mapgen::WorldMap;
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

    let seed: u64 = std::env::var("MAP_SEED")
        .unwrap_or_else(|_| "12345".to_string())
        .parse()
        .unwrap_or(12345);

    info!("Generating world map from seed {seed}");
    let world = Arc::new(WorldMap::generate(seed));
    info!(
        "World: {}x{} tiles, {} POIs, {} roads",
        world.width, world.height, world.pois.len(), world.roads.len()
    );

    // Interior spaces (caves, dungeons, castles) — loaded from JSON at
    // startup and then treated as read-only reference data. Per-player
    // interior state (position, fog, opened chests) lives on DevPlayerState.
    let interiors_dir = std::env::var("INTERIORS_DIR").unwrap_or_else(|_| "adventures/interiors".to_string());
    let interiors: interior::SharedInteriors = Arc::new(interior::load_interiors(&interiors_dir)?);
    info!("Loaded {} interior(s) from {}", interiors.len(), interiors_dir);

    // Save path is configurable via SAVE_PATH. On Render, set this to a path
    // inside a mounted persistent disk (e.g. "/data/dev_state.json") so state
    // survives redeploys. Locally, defaults to the working directory.
    let save_path_string = std::env::var("SAVE_PATH")
        .unwrap_or_else(|_| "dev_state.json".to_string());
    let save_path = save_path_string.as_str();
    // Ensure the parent directory exists (create if needed). Silent on failure;
    // the first save will log the actual I/O error.
    if let Some(parent) = std::path::Path::new(save_path).parent() {
        if !parent.as_os_str().is_empty() {
            let _ = std::fs::create_dir_all(parent);
        }
    }
    info!("Save path: {}", save_path);

    // Load events. Always start from the JSON definitions so shop inventories,
    // new events, trigger tweaks etc. take effect across restarts. Then overlay
    // the per-event `status` from the saved state so completed quests stay
    // completed. Per-player completion lives on DevPlayerState.completed_events,
    // which is unaffected by this refresh.
    let events_path = std::env::var("EVENTS_PATH")
        .unwrap_or_else(|_| "adventures/seed12345_events.json".to_string());

    let saved_events = std::fs::read_to_string(save_path)
        .ok()
        .and_then(|json| serde_json::from_str::<SaveData>(&json).ok())
        .map(|s| s.events);

    let catalog = if std::path::Path::new(&events_path).exists() {
        let json = std::fs::read_to_string(&events_path)?;
        let mut fresh = EventCatalog::from_json(&json)?;
        info!("Loaded {} events from {}", fresh.events.len(), events_path);

        if let Some(saved) = saved_events.as_ref() {
            let mut carried = 0;
            for event in fresh.events.iter_mut() {
                if let Some(prev) = saved.events.iter().find(|e| e.id == event.id) {
                    if prev.status != event.status {
                        event.force_status(prev.status);
                        carried += 1;
                    }
                }
            }
            info!("Merged {} status flags from saved state", carried);
        }
        fresh
    } else if let Some(events) = saved_events {
        info!("No events JSON at {}; using saved catalog ({} events)", events_path, events.events.len());
        events
    } else {
        info!("No events found, starting empty");
        EventCatalog::default()
    };

    let shared_events: SharedEvents = Arc::new(Mutex::new(catalog));

    // Initialize shared player state — load from disk or start empty
    let state: SharedState = Arc::new(Mutex::new({
        let mut loaded = load_state(save_path).unwrap_or_else(|| {
            info!("No saved state found, players will join via /join");
            HashMap::new()
        });
        // Drop references to items that no longer exist in the catalog (renames etc.)
        prune_missing_items(&mut loaded, item_catalog());
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
    tokio::spawn(async move {
        if let Err(e) = devserver::start_dev_server(server_state, server_events, server_notifs, server_world, server_combat, server_tick_signal, server_bridged, server_interiors).await {
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

    // Simple RNG for random encounter rolls
    let mut rng_state: u64 = seed;
    let mut save_counter: u32 = 0;

    // Shutdown signal — on SIGTERM (Render redeploy) or SIGINT (Ctrl-C),
    // do one last save before exiting so we don't lose the last ~30s.
    let mut shutdown = shutdown_signal();

    loop {
        tokio::select! {
            _ = interval.tick() => {
                rng_state = rng_state.wrapping_mul(6364136223846793005).wrapping_add(1);
                let rng_roll = (rng_state >> 33) as f32 / (u32::MAX as f32);

                // Tick interior players first. Their overworld-tick counterpart
                // is a no-op thanks to the `Location::Interior { .. }` guard
                // added in tick.rs — so they're handled exactly once.
                let interior_players: Vec<String> = {
                    let lock = state.lock().unwrap();
                    lock.iter()
                        .filter_map(|(pid, p)| p.location.interior_id().map(|_| pid.clone()))
                        .collect()
                };
                for pid in &interior_players {
                    if let Err(e) = interior::run_interior_tick(
                        &interiors,
                        &state,
                        &shared_notifs,
                        &shared_combat,
                        &mut player_last_distance,
                        &mut interior_fogs,
                        pid,
                    ) {
                        error!("Interior tick error for {}: {e:#}", pid);
                    }
                }

                if let Err(e) = tick::run_tick_dev(
                    &state,
                    &world,
                    &shared_events,
                    &shared_notifs,
                    &shared_combat,
                    &interiors,
                    &mut player_fogs,
                    &mut player_last_distance,
                    &mut player_boss_wait_notified,
                    rng_roll,
                ) {
                    error!("Tick error: {e:#}");
                }

                // Wake all long-polling clients — they get fresh post-tick state
                tick_signal.tick();

                // Save state to disk every ~30 ticks (~30 seconds)
                save_counter += 1;
                if save_counter % 30 == 0 {
                    save_state(save_path, &state, &shared_events);
                }
            }
            _ = &mut shutdown => {
                info!("Shutdown signal received — saving state and exiting");
                save_state(save_path, &state, &shared_events);
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

fn save_state(path: &str, state: &SharedState, events: &SharedEvents) {
    let save = {
        let lock = match state.lock() {
            Ok(l) => l,
            Err(e) => { error!("save_state: state mutex poisoned: {e}"); return; }
        };
        let events_lock = match events.lock() {
            Ok(l) => l,
            Err(e) => { error!("save_state: events mutex poisoned: {e}"); return; }
        };
        SaveData {
            version: SAVE_VERSION,
            players: lock.values().cloned().collect(),
            events: events_lock.clone(),
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

