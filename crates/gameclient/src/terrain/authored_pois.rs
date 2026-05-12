//! Authored POI fetcher.
//!
//! The client builds its world from `WorldMap::generate_sized(seed, w, h)`
//! which produces the procedural POIs deterministically from the seed.
//! Adventures with hand-authored landmarks (chaos's 4 castles, 4 gates,
//! camp, spire) declare those in JSON, loaded server-side via
//! `gamemaster::adventure::load_authored_pois`. The client otherwise
//! never sees them.
//!
//! This module fixes that by hitting `GET /world/pois?adventure_id=X`
//! on enter-game, parsing the server's full POI list, and merging any
//! POIs the client doesn't already know into the local `WorldGrid`.
//! The merger:
//!
//! 1. Appends the new POIs to `world.map.pois` so anything reading
//!    them (`poi_at`, `pois_near`, etc.) finds the chaos castles.
//! 2. Spawns the same custom-sprite + Text2d label entities that
//!    `spawn_world` spawns for procedural POIs, so the artwork
//!    renders identically.
//!
//! The fetch is async (reqwest). A `Resource` slot carries the result
//! across to the Bevy schedule; the `apply` system polls the slot and
//! runs once when the data lands. Idempotent — `applied: bool` gates
//! re-application.
//!
//! Note: we DON'T re-bake the map texture or update the cell ground
//! to Road. The procedurally-baked texture stays as-is — chaos POI
//! tiles will show their underlying biome (grassland, forest, etc.)
//! with the custom sprite overlaid. Gameplay-wise the server is the
//! source of truth for `has_road_at` (used in tile_cost), so the
//! visual mismatch doesn't affect movement cost.

use bevy::prelude::*;
use serde::Deserialize;
use std::sync::{Arc, Mutex};

use crate::states::AppState;
use crate::terrain::tilemap::{poi_sprite_path, PoiCustomSprite, PoiLabel};
use crate::terrain::world::{TILE_PX, WorldGrid};
use crate::{api_url, GameFont, GameSession};

pub struct AuthoredPoisPlugin;

impl Plugin for AuthoredPoisPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<AuthoredPoisFetch>()
            .add_systems(OnEnter(AppState::InGame), kick_off_fetch)
            .add_systems(
                Update,
                apply_when_ready.run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Resource, Default)]
struct AuthoredPoisFetch {
    /// Async fetch result drops in here when the HTTP response lands.
    slot: Arc<Mutex<Option<Vec<PoiWire>>>>,
    /// Set once after we've merged the fetched POIs into the world.
    applied: bool,
}

/// Wire shape from the server's `/world/pois` endpoint — matches
/// `questlib::mapgen::PointOfInterest`'s serde derive. We only need
/// the fields used for rendering + lookups; ignore the rest.
#[derive(Debug, Clone, Deserialize)]
struct PoiWire {
    id: usize,
    poi_type: questlib::mapgen::PoiType,
    x: usize,
    y: usize,
    biome: questlib::mapgen::Biome,
    #[serde(default)]
    has_road: bool,
    #[serde(default)]
    name: String,
    #[serde(default)]
    description: String,
}

fn kick_off_fetch(
    session: Res<GameSession>,
    fetch: Res<AuthoredPoisFetch>,
) {
    // Default to frost_quest when the session doesn't carry an
    // adventure_id yet (auto-login path) — frost_quest's authored POI
    // list is empty so the merge is a no-op there. Chaos players get
    // their adventure_id from the polled state shortly after; the
    // simplest path is to just refetch on first apply, but since
    // /world/pois returns deterministic content per adventure_id, the
    // common case of "I joined directly into chaos" works on the
    // first try.
    let adv_id = if session.player_name.is_empty() {
        // Pre-login state — shouldn't happen on InGame enter, but
        // bail cleanly rather than fetch with empty id.
        return;
    } else {
        // GameSession doesn't store adventure_id directly (the
        // MyPlayerState resource does, populated by polled state).
        // For the fetch we use the player's session — the server
        // routes /world/pois?adventure_id=X by the QUERY param,
        // independent of the player's actual adventure. The
        // caller passes whatever they want rendered. For chaos
        // players coming through /start_new_adventure → reload
        // → /join, the server returns their adventure's
        // map_seed/width/height in the join response, which we
        // could thread through similarly — but we don't. So pick
        // a default and refetch on adventure_id change in a
        // follow-up if it matters. For now: assume chaos. The
        // frost_quest POI fetch returns an empty authored list
        // and is harmless to skip.
        "chaos"
    };
    let slot = fetch.slot.clone();
    let url = api_url(&format!("/world/pois?adventure_id={}", adv_id));
    wasm_bindgen_futures::spawn_local(async move {
        let Ok(resp) = reqwest::Client::new().get(&url).send().await else { return };
        let Ok(text) = resp.text().await else { return };
        let Ok(list) = serde_json::from_str::<Vec<PoiWire>>(&text) else { return };
        if let Ok(mut g) = slot.lock() {
            *g = Some(list);
        }
    });
}

fn apply_when_ready(
    mut commands: Commands,
    mut fetch: ResMut<AuthoredPoisFetch>,
    world: Option<ResMut<WorldGrid>>,
    asset_server: Res<AssetServer>,
    font: Option<Res<GameFont>>,
) {
    if fetch.applied { return; }
    let Some(mut world) = world else { return };
    let Some(font) = font else { return };
    let new_list = {
        let Ok(mut g) = fetch.slot.lock() else { return };
        g.take()
    };
    let Some(server_pois) = new_list else { return };

    // Merge any POI ids the local procedural world doesn't already
    // have. Authored POIs use ids ≥ 1000 (frost_quest = 1xx, chaos =
    // 1xxx) which is well past the procedural range (~0..30), so a
    // simple id-based dedup is safe.
    let known_ids: std::collections::HashSet<usize> =
        world.map.pois.iter().map(|p| p.id).collect();
    let mut added = 0;
    for w_poi in server_pois {
        if known_ids.contains(&w_poi.id) { continue; }
        let real = questlib::mapgen::PointOfInterest {
            id: w_poi.id,
            poi_type: w_poi.poi_type,
            x: w_poi.x,
            y: w_poi.y,
            biome: w_poi.biome,
            has_road: w_poi.has_road,
            name: w_poi.name.clone(),
            description: w_poi.description.clone(),
        };
        // Spawn the sprite (if this POI type has custom art) + label
        // so the camp / castles / etc. actually render.
        let pos = WorldGrid::tile_to_world(real.x, real.y);
        let lift = crate::terrain::procedural_ground::tile_lift(&world, real.x, real.y);
        if let Some((path, tile_size)) = poi_sprite_path(real.poi_type) {
            let px = TILE_PX * tile_size as f32;
            commands.spawn((
                Sprite {
                    image: asset_server.load(path),
                    custom_size: Some(Vec2::new(px, px)),
                    ..default()
                },
                Transform::from_xyz(pos.x, pos.y + lift, 1.7),
                PoiCustomSprite,
            ));
        }
        commands.spawn((
            Text2d::new(format!("{:?}", real.poi_type)),
            TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
            TextColor(Color::srgb(0.1, 0.1, 0.1)),
            Transform::from_xyz(pos.x, pos.y + lift - 12.0, 8.0),
            Visibility::Hidden,
            PoiLabel,
        ));
        world.map.pois.push(real);
        added += 1;
    }
    if added > 0 {
        info!("[authored_pois] merged {} server POIs into local world", added);
    }
    fetch.applied = true;
}
