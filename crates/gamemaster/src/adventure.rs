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
use questlib::mapgen::WorldMap;

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
    pub events_path: String,
    pub entities_path: String,
    pub interiors_dir: String,
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
        events_path: std::env::var("EVENTS_PATH")
            .unwrap_or_else(|_| "adventures/seed12345_events.json".into()),
        entities_path: std::env::var("ENTITIES_PATH")
            .unwrap_or_else(|_| "adventures/seed12345_entities.json".into()),
        interiors_dir: std::env::var("INTERIORS_DIR")
            .unwrap_or_else(|_| "adventures/interiors".into()),
    };
    let chaos = AdventurePreset {
        id: "chaos".into(),
        display_name: "Chaos Unleashed".into(),
        map_seed: 99999,
        events_path: "adventures/seed99999_events.json".into(),
        entities_path: "adventures/seed99999_entities.json".into(),
        // Shares the same interiors set for now. Authored chaos
        // castle interiors will land later under a separate dir.
        interiors_dir: "adventures/interiors".into(),
    };
    vec![frost_quest, chaos]
}

/// The id every existing save points at by serde default. Don't
/// change unless you're prepared to migrate save files.
pub const DEFAULT_ADVENTURE_ID: &str = "frost_quest";

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
    tracing::info!("[adv] loading '{}' (seed {})", preset.id, preset.map_seed);

    let world = Arc::new(WorldMap::generate(preset.map_seed));
    tracing::info!(
        "[adv:{}] world: {}×{} tiles, {} POIs, {} roads",
        preset.id, world.width, world.height, world.pois.len(), world.roads.len()
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
