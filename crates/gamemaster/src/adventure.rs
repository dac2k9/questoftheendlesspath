//! Adventure preset registry + bundle loader.
//!
//! An "adventure" is a self-contained chapter: its own seed (and so
//! its own procedurally-generated map), its own events JSON, its own
//! mobile entities, its own interiors directory. Multiple adventures
//! load at server startup; each player is in exactly one adventure
//! at a time (`DevPlayerState.adventure_id`). When the title screen
//! offers "New Adventure", the server resets the player and switches
//! their `adventure_id` to the new preset id.
//!
//! Per-adventure routing (looking up the player's bundle on every
//! endpoint + tick) is wired in the next step — this module just
//! defines the data shapes and loader so the refactor can land in
//! small commits.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result};
use questlib::events::EventCatalog;
use questlib::mapgen::{PointOfInterest, PoiType, WorldMap};
use serde::Deserialize;

use crate::interior;
use crate::mobile_entity;

/// Authored data for one adventure. Owned strings so env-var overrides
/// for dev workflows (MAP_SEED / EVENTS_PATH / etc.) can still patch
/// the default preset without `unsafe` shenanigans on `&'static str`.
#[derive(Debug, Clone)]
pub struct AdventurePreset {
    /// Stable id used in `DevPlayerState.adventure_id` and the
    /// `/start_new_adventure` endpoint.
    pub id: String,
    /// Human-friendly label shown on the title screen.
    pub display_name: String,
    pub map_seed: u64,
    /// World dimensions in tiles. The frost_quest default of 100×80
    /// is mirrored by the client's MAP_W/MAP_H. Larger adventures
    /// (chaos = 200×160 for 4× area) propagate these through /join
    /// so the client rebuilds its WorldGrid to match.
    pub map_width: usize,
    pub map_height: usize,
    pub events_path: String,
    pub entities_path: String,
    pub interiors_dir: String,
    /// Authored POIs injected on top of the procedural placement.
    /// Empty (or missing file) is fine — adventure runs with just
    /// procedural POIs. Adventures with required-location quests
    /// (castles, travel gates) author their landmarks here so the
    /// quest's coords don't depend on whatever the seed happened
    /// to place.
    pub pois_path: String,
}

/// The default registry. First entry is the "current" adventure that
/// existing saves point at (their `adventure_id` defaults to this id
/// via serde). Env vars override the default preset's paths so the
/// existing `MAP_SEED=… EVENTS_PATH=… cargo run` workflow keeps
/// working unchanged.
pub fn presets() -> Vec<AdventurePreset> {
    let frost_quest = AdventurePreset {
        id: "frost_quest".into(),
        display_name: "The Frost Lord".into(),
        map_seed: std::env::var("MAP_SEED")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(12345),
        map_width: questlib::mapgen::MAP_W,
        map_height: questlib::mapgen::MAP_H,
        events_path: std::env::var("EVENTS_PATH")
            .unwrap_or_else(|_| "adventures/seed12345_events.json".into()),
        entities_path: std::env::var("ENTITIES_PATH")
            .unwrap_or_else(|_| "adventures/seed12345_entities.json".into()),
        interiors_dir: std::env::var("INTERIORS_DIR")
            .unwrap_or_else(|_| "adventures/interiors".into()),
        pois_path: "adventures/seed12345_pois.json".into(),
    };
    let chaos = AdventurePreset {
        id: "chaos".into(),
        display_name: "Chaos Unleashed".into(),
        // Distinct seed from frost_quest so the chaos world is
        // visibly different terrain. The client picks this up via
        // `GameSession.map_seed` (set from /join response) and
        // rebuilds WorldGrid on enter-game. Authored POI positions
        // in seed99999_pois.json are tuned for this seed's terrain;
        // change the seed = re-tune those coordinates.
        map_seed: 99999,
        // 4× area (200×160 tiles). Fits within the 4096-min WebGL
        // texture limit when baked at 16 px/tile (3200×2560). The
        // chaos authored POI layout in seed99999_pois.json is tuned
        // for this size — castles in each quadrant's outer ring,
        // gates between camp and castles, camp + spire central.
        map_width: 200,
        map_height: 160,
        events_path: "adventures/seed99999_events.json".into(),
        entities_path: "adventures/seed99999_entities.json".into(),
        // Shares the same interiors set for now. Authored chaos
        // castle interiors will land later under a separate dir.
        interiors_dir: "adventures/interiors".into(),
        pois_path: "adventures/seed99999_pois.json".into(),
    };
    vec![frost_quest, chaos]
}

/// The id every existing save points at by serde default. Don't
/// change unless you're prepared to migrate save files.
pub const DEFAULT_ADVENTURE_ID: &str = "frost_quest";

/// JSON shape for an authored POI: only the gameplay-relevant bits.
/// Biome / has_road get auto-filled from the world's tile at the
/// coords so JSON edits can't drift from the actual terrain.
#[derive(Debug, Clone, Deserialize)]
struct AuthoredPoi {
    pub id: usize,
    pub poi_type: PoiType,
    pub x: usize,
    pub y: usize,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
struct AuthoredPoisFile {
    pub pois: Vec<AuthoredPoi>,
}

/// Load extra POIs from JSON and append them to `world.pois`. Biome
/// + has_road are sampled from the existing world (the file just
/// declares coords + type + label). Returns the number appended.
fn load_authored_pois(path: &str, world: &mut WorldMap) -> Result<usize> {
    let json = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path))?;
    let parsed: AuthoredPoisFile = serde_json::from_str(&json)
        .with_context(|| format!("parse {}", path))?;
    let count = parsed.pois.len();
    for a in parsed.pois {
        let biome = world.biome_at(a.x, a.y);
        let has_road = world.has_road_at(a.x, a.y);
        world.pois.push(PointOfInterest {
            id: a.id,
            poi_type: a.poi_type,
            x: a.x,
            y: a.y,
            biome,
            has_road,
            name: a.name,
            description: a.description,
        });
    }
    Ok(count)
}

/// All per-adventure runtime state bundled together. Multiple bundles
/// coexist at runtime; the tick loop iterates them, and endpoints
/// look up the bundle for the calling player's `adventure_id`.
pub struct AdventureBundle {
    pub preset: AdventurePreset,
    pub world: Arc<WorldMap>,
    pub events: crate::SharedEvents,
    pub interiors: interior::SharedInteriors,
    pub entity_defs: mobile_entity::SharedEntityDefs,
    pub entity_states: mobile_entity::SharedEntityStates,
}

/// Build an AdventureBundle from a preset. Doesn't merge in any
/// per-player save data — the caller does that afterwards (event
/// status flags, mobile entity runtime state).
pub fn load_bundle(preset: AdventurePreset) -> Result<AdventureBundle> {
    tracing::info!(
        "[adv] loading '{}' (seed {}, dims {}×{})",
        preset.id, preset.map_seed, preset.map_width, preset.map_height,
    );

    let mut world = WorldMap::generate_sized(preset.map_seed, preset.map_width, preset.map_height);
    let procedural_pois = world.pois.len();
    if std::path::Path::new(&preset.pois_path).exists() {
        let added = load_authored_pois(&preset.pois_path, &mut world)
            .with_context(|| format!("load authored POIs from {}", preset.pois_path))?;
        tracing::info!(
            "[adv:{}] +{} authored POI(s) from {}",
            preset.id, added, preset.pois_path,
        );
    }
    let world = Arc::new(world);
    tracing::info!(
        "[adv:{}] world: {}×{} tiles, {} POIs ({} procedural + {} authored), {} roads",
        preset.id, world.width, world.height, world.pois.len(),
        procedural_pois, world.pois.len() - procedural_pois, world.roads.len()
    );

    let interiors: interior::SharedInteriors =
        Arc::new(interior::load_interiors(&preset.interiors_dir)?);
    tracing::info!(
        "[adv:{}] {} interior(s) from {}",
        preset.id, interiors.len(), preset.interiors_dir
    );

    let entity_defs: mobile_entity::SharedEntityDefs =
        Arc::new(mobile_entity::load_entities(&preset.entities_path)?);
    tracing::info!(
        "[adv:{}] {} mobile entit{} from {}",
        preset.id,
        entity_defs.len(),
        if entity_defs.len() == 1 { "y" } else { "ies" },
        preset.entities_path,
    );

    // Empty runtime state; main.rs merges in saved positions /
    // respawn timers before tick start.
    let entity_states: mobile_entity::SharedEntityStates =
        Arc::new(Mutex::new(HashMap::new()));

    let events_catalog = if std::path::Path::new(&preset.events_path).exists() {
        let json = std::fs::read_to_string(&preset.events_path)
            .with_context(|| format!("read {}", preset.events_path))?;
        EventCatalog::from_json(&json)
            .with_context(|| format!("parse {}", preset.events_path))?
    } else {
        tracing::warn!(
            "[adv:{}] no events file at {} — starting empty",
            preset.id, preset.events_path
        );
        EventCatalog::default()
    };
    tracing::info!(
        "[adv:{}] {} events from {}",
        preset.id, events_catalog.events.len(), preset.events_path
    );
    let events: crate::SharedEvents = Arc::new(Mutex::new(events_catalog));

    Ok(AdventureBundle {
        preset,
        world,
        events,
        interiors,
        entity_defs,
        entity_states,
    })
}
