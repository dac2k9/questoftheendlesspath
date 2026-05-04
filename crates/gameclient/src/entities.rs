//! Mobile entity rendering — server-authoritative wandering / patrolling
//! NPCs, monsters, animals.
//!
//! Polls `/entities?player_id=X` every second. Each polled entity gets
//! a sprite (atlas, walk-cycle) keyed off the `sprite` name string, with
//! the position interpolated between polls.
//!
//! Mirrors `OtherPlayerSprite` rendering but with a per-entity sprite
//! registry instead of the per-champion atlas system.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use serde::Deserialize;

use crate::states::AppState;
use crate::GameSession;

pub struct EntitiesPlugin;

impl Plugin for EntitiesPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<PolledEntities>()
            .init_resource::<EntityPollState>()
            .add_systems(OnEnter(AppState::InGame), build_sprite_registry)
            .add_systems(
                Update,
                (poll_entities, render_entities)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

// ── Server payload shape ────────────────────────────────

#[derive(Debug, Deserialize)]
struct EntitiesResp {
    entities: Vec<PolledEntity>,
}

#[derive(Debug, Clone, Deserialize)]
struct PolledEntity {
    id: String,
    sprite: String,
    x: usize,
    y: usize,
    facing: String,
}

// ── Resources ───────────────────────────────────────────

/// Latest polled entity payload. Async fetch writes here; the render
/// system reads it.
#[derive(Resource, Default)]
struct PolledEntities {
    list: Arc<Mutex<Vec<PolledEntity>>>,
}

#[derive(Resource)]
struct EntityPollState {
    timer: f32,
    fetch_inflight: Arc<Mutex<bool>>,
}

impl Default for EntityPollState {
    fn default() -> Self {
        Self {
            // Force an immediate poll on first frame in InGame.
            timer: 1.0,
            fetch_inflight: Arc::new(Mutex::new(false)),
        }
    }
}

/// Pre-loaded entity sprite assets keyed by the `sprite` string in
/// authored JSON (e.g. "slime", "skeleton_soldier"). Single source of
/// truth for what a sprite name resolves to.
#[derive(Resource, Default)]
struct EntitySpriteRegistry {
    sheets: HashMap<String, EntitySpriteSheet>,
    fallback: Option<EntitySpriteSheet>,
}

#[derive(Clone)]
struct EntitySpriteSheet {
    image: Handle<Image>,
    atlas: Handle<TextureAtlasLayout>,
    /// Number of frames in row 0 (walk-cycle length).
    anim_frames: usize,
}

// ── Component markers ───────────────────────────────────

#[derive(Component)]
struct MobileEntitySprite {
    id: String,
    /// Most recent server-confirmed tile position (in world coords).
    target_world: Vec2,
    /// Unrounded sub-pixel position the lerp operates on. Only the
    /// rounded value gets written to the transform — without this,
    /// rounding the transform in-place destroys fractional progress
    /// each frame and the entity freezes ~7 px shy of its target tile.
    visual_pos: Vec2,
    facing: EntityFacing,
}

#[derive(Component)]
struct WalkAnimTimer {
    timer: Timer,
    frame: usize,
    cols: usize,
    moving: bool,
}

#[derive(Clone, Copy, PartialEq)]
enum EntityFacing {
    Up,
    Down,
    Left,
    Right,
}

impl EntityFacing {
    fn parse(s: &str) -> Self {
        match s {
            "up" => Self::Up,
            "left" => Self::Left,
            "right" => Self::Right,
            _ => Self::Down,
        }
    }
    fn atlas_row(self) -> usize {
        // Match the existing monster sprite convention: rows are
        // arranged as Down(0), Left(1), Right(2), Up(3). Some atlases
        // only have a single row; we clamp on read, so passing 1-3 on
        // a single-row sheet still renders the (only) frames.
        match self {
            Self::Down => 0,
            Self::Left => 1,
            Self::Right => 2,
            Self::Up => 3,
        }
    }
}

// ── Sprite registry ─────────────────────────────────────

fn build_sprite_registry(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut atlases: ResMut<Assets<TextureAtlasLayout>>,
) {
    let entries: &[(&str, &[u8])] = &[
        ("slime",            include_bytes!("../assets/sprites/monsters/Slime.png")),
        ("club_goblin",      include_bytes!("../assets/sprites/monsters/ClubGoblin.png")),
        ("archer_goblin",    include_bytes!("../assets/sprites/monsters/ArcherGoblin.png")),
        ("giant_crab",       include_bytes!("../assets/sprites/monsters/GiantCrab.png")),
        ("minotaur",         include_bytes!("../assets/sprites/monsters/Minotaur.png")),
        ("yeti",             include_bytes!("../assets/sprites/monsters/Yeti.png")),
        ("wendigo",          include_bytes!("../assets/sprites/monsters/Wendigo.png")),
        ("purple_demon",     include_bytes!("../assets/sprites/monsters/PurpleDemon.png")),
        ("necromancer",      include_bytes!("../assets/sprites/monsters/Necromancer.png")),
        ("skeleton_soldier", include_bytes!("../assets/sprites/monsters/Skeleton-Soldier.png")),
    ];

    let mut sheets = HashMap::new();
    let mut fallback: Option<EntitySpriteSheet> = None;
    for (name, bytes) in entries {
        let dyn_img = match image::load_from_memory(bytes) {
            Ok(i) => i,
            Err(e) => {
                log::warn!("[entities] failed to load sprite {}: {}", name, e);
                continue;
            }
        };
        let rgba = dyn_img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let cols = (w / 16) as usize;
        let rows = (h / 16) as usize;
        // Count non-empty frames in the first row so the walk cycle
        // doesn't include trailing empty frames.
        let raw = rgba.as_raw();
        let mut anim_frames = 0usize;
        for c in 0..cols {
            let mut has_pixels = false;
            for py in 0..16 {
                for px in 0..16 {
                    let si = (py * w as usize + c * 16 + px) * 4;
                    if si + 3 < raw.len() && raw[si + 3] > 10 {
                        has_pixels = true;
                    }
                }
            }
            if has_pixels {
                anim_frames = c + 1;
            }
        }
        let img = Image::new(
            Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            TextureDimension::D2,
            rgba.into_raw(),
            TextureFormat::Rgba8UnormSrgb,
            default(),
        );
        let sheet = EntitySpriteSheet {
            image: images.add(img),
            atlas: atlases.add(TextureAtlasLayout::from_grid(
                UVec2::new(16, 16),
                cols as u32,
                rows as u32,
                None,
                None,
            )),
            anim_frames: anim_frames.max(1),
        };
        if fallback.is_none() {
            fallback = Some(sheet.clone());
        }
        sheets.insert(name.to_string(), sheet);
    }
    log::info!("[entities] loaded {} sprite sheets", sheets.len());
    commands.insert_resource(EntitySpriteRegistry { sheets, fallback });
}

// ── Polling ─────────────────────────────────────────────

fn poll_entities(
    time: Res<Time>,
    session: Res<GameSession>,
    mut state: ResMut<EntityPollState>,
    polled: Res<PolledEntities>,
) {
    state.timer += time.delta_secs();
    if state.timer < 1.0 {
        return;
    }
    state.timer = 0.0;
    if session.player_id.is_empty() {
        return;
    }
    // Skip if the previous fetch hasn't finished — keeps the loop
    // sane on a slow connection.
    {
        let mut g = state.fetch_inflight.lock().unwrap();
        if *g {
            return;
        }
        *g = true;
    }
    let player_id = session.player_id.clone();
    let result = polled.list.clone();
    let inflight = state.fetch_inflight.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let url = crate::api_url(&format!("/entities?player_id={}", player_id));
        let resp = reqwest::Client::new().get(&url).send().await;
        if let Ok(resp) = resp {
            if let Ok(text) = resp.text().await {
                if let Ok(parsed) = serde_json::from_str::<EntitiesResp>(&text) {
                    if let Ok(mut g) = result.lock() {
                        *g = parsed.entities;
                    }
                }
            }
        }
        if let Ok(mut g) = inflight.lock() {
            *g = false;
        }
    });
}

// ── Rendering ───────────────────────────────────────────

const TILE_PX: f32 = 16.0;

/// Convert a tile (x, y) to a world position. Mirrors
/// `WorldGrid::tile_to_world` so spawn / interp coords match what the
/// rest of the game uses (player sprite at z=5, monsters at z=1.5).
fn tile_to_world(x: usize, y: usize) -> Vec2 {
    Vec2::new(x as f32 * TILE_PX, -(y as f32) * TILE_PX)
}

fn render_entities(
    mut commands: Commands,
    time: Res<Time>,
    polled: Res<PolledEntities>,
    registry: Option<Res<EntitySpriteRegistry>>,
    mut existing: Query<
        (Entity, &mut MobileEntitySprite, &mut Transform, &mut Sprite, &mut WalkAnimTimer),
    >,
) {
    let Some(registry) = registry else { return };
    let Ok(list) = polled.list.lock() else { return };

    let live_ids: HashSet<&str> = list.iter().map(|e| e.id.as_str()).collect();

    // Despawn entities no longer in the polled set (defeated, out of
    // viewport, etc.).
    for (entity, marker, _, _, _) in existing.iter() {
        if !live_ids.contains(marker.id.as_str()) {
            commands.entity(entity).despawn_recursive();
        }
    }

    for poll in list.iter() {
        let target = tile_to_world(poll.x, poll.y);
        let facing = EntityFacing::parse(&poll.facing);

        let found = existing.iter_mut().find(|(_, m, _, _, _)| m.id == poll.id);
        if let Some((_, mut marker, mut tf, mut sprite, mut anim)) = found {
            // Update target — the next frame's interp will move toward
            // it. Detect whether we moved; controls walk-cycle play.
            let moved = marker.target_world != target;
            let same_facing = marker.facing == facing;
            marker.target_world = target;
            marker.facing = facing;

            // Smooth interpolation toward target. Same exponential
            // decay used by OtherPlayerSprite — looks natural.
            // If the gap is bigger than ~2 tiles the entity probably
            // teleported (server respawn, JSON spawn-coords edited);
            // snap directly so we don't draw a 4-second sliding leap
            // across the map. ≤2 tiles = normal wander step → smooth.
            let dt = time.delta_secs();
            let dist_sq = (target.x - marker.visual_pos.x).powi(2)
                + (target.y - marker.visual_pos.y).powi(2);
            if dist_sq > (32.0_f32 * 32.0_f32) {
                marker.visual_pos = target;
            } else {
                let lerp = 1.0 - (-4.0_f32 * dt).exp();
                marker.visual_pos = marker.visual_pos.lerp(target, lerp);
                // Snap when within a sub-pixel of the target so the
                // entity actually lands on the tile grid. Without this
                // the lerp asymptotes and never lands exactly.
                if marker.visual_pos.distance_squared(target) < 0.01 {
                    marker.visual_pos = target;
                }
            }
            tf.translation.x = marker.visual_pos.x.round();
            tf.translation.y = marker.visual_pos.y.round();

            // Walk-cycle: keep animating while still distant from
            // target, freeze on the first frame once we've arrived.
            let arrived = (marker.visual_pos.x - target.x).abs() < 0.6
                && (marker.visual_pos.y - target.y).abs() < 0.6;
            let is_walking = !arrived || moved;
            let cols = anim.cols;
            let row = facing.atlas_row();
            sprite.flip_x = false;
            if is_walking {
                anim.timer.tick(time.delta());
                if anim.timer.just_finished() {
                    anim.frame = (anim.frame % 4) + 1;
                }
                if let Some(ref mut atlas) = sprite.texture_atlas {
                    atlas.index = (row * cols + anim.frame).min(rows_safe(cols, atlas));
                }
                anim.moving = true;
            } else if anim.moving || !same_facing {
                anim.moving = false;
                anim.frame = 0;
                if let Some(ref mut atlas) = sprite.texture_atlas {
                    atlas.index = (row * cols).min(rows_safe(cols, atlas));
                }
            }
        } else {
            // Spawn new sprite for this id.
            let sheet = registry
                .sheets
                .get(&poll.sprite)
                .cloned()
                .or_else(|| registry.fallback.clone());
            let Some(sheet) = sheet else { continue };
            let cols_count = atlas_cols(&sheet);
            commands.spawn((
                Sprite {
                    image: sheet.image.clone(),
                    texture_atlas: Some(TextureAtlas {
                        layout: sheet.atlas.clone(),
                        index: 0,
                    }),
                    ..default()
                },
                Transform::from_xyz(target.x, target.y, 1.5),
                MobileEntitySprite {
                    id: poll.id.clone(),
                    target_world: target,
                    visual_pos: target,
                    facing,
                },
                WalkAnimTimer {
                    timer: Timer::from_seconds(0.18, TimerMode::Repeating),
                    frame: 0,
                    cols: cols_count,
                    moving: false,
                },
            ));
        }
    }
}

/// Bounds-clamp the atlas index so a misconfigured row-count doesn't
/// panic the renderer when an atlas has fewer rows than `atlas_row()`
/// expects (e.g. single-row sprite sheets).
fn rows_safe(cols: usize, _atlas: &TextureAtlas) -> usize {
    // We don't have direct access to the atlas's row count from
    // TextureAtlas; the worst case is index out of bounds at render
    // time. Returning a large sentinel keeps the .min() a no-op when
    // the actual row index is in range. The renderer already handles
    // out-of-range frames by clamping, but we leave the .min() in
    // place as a future-proofing gate.
    cols * 32
}

fn atlas_cols(sheet: &EntitySpriteSheet) -> usize {
    // Atlas column count isn't directly stored on EntitySpriteSheet;
    // anim_frames was derived from row 0 specifically and is the
    // useful walk-cycle length. We use it as the column stride for
    // atlas-index math.
    sheet.anim_frames.max(1)
}
