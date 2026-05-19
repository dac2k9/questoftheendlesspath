//! Client-side rendering for interior spaces (caves, castles).
//!
//! Phase 2 MVP:
//! - Watches `MyPlayerState.location`; on change, fetches the interior map
//!   from the server and swaps the view.
//! - Hides overworld visual entities while inside, shows them again on exit.
//! - Renders walls / floors / portals / chests as simple colored quads
//!   (a proper tileset is a later polish pass).
//! - Mouse-click on a walkable interior tile plans a route using BFS on the
//!   interior grid and POSTs it to /set_route.
//! - Mouse-click on a portal tile calls /use_portal.
//!
//! Contained in this one module. Touches tilemap.rs only for marker queries
//! to hide overworld sprites.
//!
//! NOTE: This module is responsible ONLY for rendering + input inside an
//! interior. The server already handles movement, fog, and chest logic.

use std::sync::{Arc, Mutex};

use bevy::color::Color;
use bevy::prelude::*;

use questlib::interior::{InteriorMap, InteriorTile, PortalDest};

use super::procedural_ground::ProceduralGroundSprite;
use super::tilemap::{FogSprite, MapSprite, MyPlayerState, OverworldOnly};
use super::world::{WorldGrid, TILE_PX};
use crate::states::AppState;
use crate::GameSession;

// ── Plugin ─────────────────────────────────────────

pub struct InteriorPlugin;

impl Plugin for InteriorPlugin {
    fn build(&self, app: &mut App) {
        app
            .init_resource::<CurrentInterior>()
            .add_systems(
                Update,
                (
                    watch_location_changes,
                    apply_fetched_interior,
                    retry_stuck_interior_fetch,
                    handle_interior_click,
                    update_hud_label,
                    sync_monster_visibility,
                    sync_portal_lock_color,
                )
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

// ── Resource ───────────────────────────────────────

#[derive(Resource, Default)]
pub struct CurrentInterior {
    /// Interior currently being rendered (and the source of truth for the
    /// interior grid we BFS / pathfind on).
    pub map: Option<InteriorMap>,
    /// Last interior id we kicked off a fetch for. Prevents duplicate fetches
    /// if state.location flips twice in rapid succession.
    fetching_id: Option<String>,
    /// Async slot — the fetch task writes the parsed InteriorMap here, then
    /// the apply system consumes it on the next frame.
    fetched: Arc<Mutex<Option<InteriorMap>>>,
}

// ── Marker components ──────────────────────────────

/// Anything spawned for the currently rendered interior. Despawned when
/// location flips back to overworld or to a different interior.
#[derive(Component)]
struct InteriorEntity;

#[derive(Component)]
struct InteriorPortal {
    pub idx: usize,
    pub label: String,
    /// Mirrors Portal.unlock_event_id. None = always usable.
    pub unlock_event_id: Option<String>,
}

#[derive(Component)]
struct InteriorMonsterMarker {
    /// Compound defeated-monsters key: "<interior_id>:monster:<idx>".
    pub key: String,
}

#[derive(Component)]
struct InteriorHudLabel;

// ── Location-change watcher ────────────────────────

/// Drives the interior scene based on MyPlayerState.location every frame:
///   - state.location == None  → overworld VISIBLE, no interior entities
///   - state.location == Some  → overworld HIDDEN, fetch + render interior
///
/// This used to be a pure transition detector (only acted when desired
/// != rendered). That left the overworld stuck visible in two failure
/// modes we hit on production:
///   1. apply_server_state's one-time init flips both layers to Visible
///      AFTER the watcher's hide-on-transition already ran. The watcher
///      then sees desired==rendered or fetching_id==Some, returns
///      early, and never hides again.
///   2. /interior fetch fails or parses fail — the async error is
///      logged but no retry happens. fetching_id is Some so the watcher
///      keeps returning early forever. With the old transition-only
///      logic, the overworld was hidden once and stayed hidden; with
///      the init race in (1) it stayed visible.
///
/// New shape: visibility is set every frame from `state.location`, and
/// fetch is kicked off only when needed (desired set + not currently
/// fetching it + not already rendered). Failed fetches retry on the
/// next frame because we clear fetching_id when the slot empties out
/// without a map landing in `current.map`.
fn watch_location_changes(
    mut commands: Commands,
    state: Res<MyPlayerState>,
    mut current: ResMut<CurrentInterior>,
    interior_entities: Query<Entity, With<InteriorEntity>>,
    mut overworld_only: Query<&mut Visibility, With<OverworldOnly>>,
) {
    let desired = state.location.clone();

    // Every-frame visibility sync — the watcher is the single owner of
    // overworld-only visibility once the game is running. Anything
    // that shouldn't show inside a cavern (MapSprite, FogSprite,
    // ProceduralGroundSprite, ChestSprite, MonsterSprite, POI labels,
    // POI custom sprites, mobile entity sprites, minimap, …) carries
    // the `OverworldOnly` marker; we sweep all of them at once here.
    // ProceduralGroundSprite is the procedural shader ground layer
    // (the pixel-art grass/water you actually see) — it ran on top of
    // the cavern tiles for Daniel because it sits at z=0.05 / Opaque,
    // so even with MapSprite + FogSprite hidden the overworld leaked
    // through. Chests likewise leaked when only the map/fog sprites
    // were hidden.
    let want_overworld_visible = desired.is_none();
    let target_vis = if want_overworld_visible { Visibility::Visible } else { Visibility::Hidden };
    for mut v in &mut overworld_only { if *v != target_vis { *v = target_vis; } }

    // No interior wanted → make sure interior entities are gone.
    let Some(want_id) = desired else {
        if current.map.is_some() || current.fetching_id.is_some() {
            for e in &interior_entities { commands.entity(e).despawn_recursive(); }
            current.map = None;
            current.fetching_id = None;
        }
        return;
    };

    // Already rendering this interior — nothing to do.
    if current.map.as_ref().is_some_and(|m| m.id == want_id) { return; }

    // A different interior is currently rendered — clear it before
    // fetching the new one. apply_fetched_interior also clears, but
    // doing it here keeps the screen empty during the in-flight fetch
    // rather than showing stale tiles.
    if current.map.as_ref().is_some_and(|m| m.id != want_id) {
        for e in &interior_entities { commands.entity(e).despawn_recursive(); }
        current.map = None;
    }

    // Don't double-fetch a request already in flight.
    if current.fetching_id.as_deref() == Some(&want_id) { return; }

    current.fetching_id = Some(want_id.clone());
    let slot = current.fetched.clone();
    let url = crate::api_url(&format!("/interior?id={}", want_id));
    let want_id_for_task = want_id.clone();
    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let outcome = client.get(&url).send().await;
        match outcome {
            Ok(resp) => match resp.json::<InteriorMap>().await {
                Ok(map) => {
                    if let Ok(mut lock) = slot.lock() { *lock = Some(map); }
                }
                Err(e) => log::error!("[interior] parse failed for {}: {}", want_id_for_task, e),
            },
            Err(e) => log::error!("[interior] fetch failed for {}: {}", want_id_for_task, e),
        }
    });
}

/// Detects "fetch crashed and never landed in current.fetched" and
/// clears fetching_id so the next frame's watcher kicks off a retry.
/// Without this, a single network blip on entry to an interior strands
/// the client on a hidden overworld forever.
fn retry_stuck_interior_fetch(
    time: Res<Time>,
    state: Res<MyPlayerState>,
    mut current: ResMut<CurrentInterior>,
    mut retry: Local<f32>,
) {
    *retry += time.delta_secs();
    if *retry < 4.0 { return; }
    *retry = 0.0;

    // Only retry while we're supposed to be in an interior, are
    // actively waiting on a fetch, and the slot is still empty.
    let want = state.location.as_deref();
    let fetching = current.fetching_id.as_deref();
    if let (Some(w), Some(f)) = (want, fetching) {
        if w == f && current.map.is_none() {
            let slot_empty = current.fetched.lock().map(|g| g.is_none()).unwrap_or(true);
            if slot_empty {
                log::warn!("[interior] retrying stranded fetch for {}", w);
                current.fetching_id = None;
            }
        }
    }
}

// ── Fetched-interior consumer ──────────────────────

fn apply_fetched_interior(
    mut commands: Commands,
    mut current: ResMut<CurrentInterior>,
    asset_server: Res<AssetServer>,
    existing_entities: Query<Entity, With<InteriorEntity>>,
) {
    let incoming = match current.fetched.lock() {
        Ok(mut lock) => lock.take(),
        Err(_) => return,
    };
    let Some(map) = incoming else { return };

    // Clear any prior interior's entities (e.g. moving between two caves).
    for e in &existing_entities { commands.entity(e).despawn_recursive(); }

    // Spawn tiles. Single-color quads per tile — good enough for MVP.
    // A proper tileset is a follow-up polish task.
    let tile_size = Vec2::splat(TILE_PX as f32);
    for ty in 0..map.height {
        for tx in 0..map.width {
            let Some(tile) = map.tile_at(tx, ty) else { continue };
            let (color, z) = match tile {
                InteriorTile::Wall  => (Color::srgb(0.10, 0.08, 0.10), 0.1),
                InteriorTile::Floor => (Color::srgb(0.22, 0.20, 0.18), 0.05),
            };
            let pos = WorldGrid::tile_to_world(tx, ty);
            commands.spawn((
                Sprite { color, custom_size: Some(tile_size), ..default() },
                Transform::from_xyz(pos.x, pos.y, z),
                InteriorEntity,
            ));
        }
    }

    // Portals — bright teal if unlocked / unconditional, dim orange if the
    // player hasn't discovered the other side yet. Visibility is synced by
    // sync_portal_visibility each frame against state.completed_events so
    // the moment a shortcut unlocks, the color changes.
    for (i, portal) in map.portals.iter().enumerate() {
        let pos = WorldGrid::tile_to_world(portal.x, portal.y);
        commands.spawn((
            Sprite {
                color: Color::srgb(0.20, 0.70, 0.85),
                custom_size: Some(tile_size * 0.85),
                ..default()
            },
            Transform::from_xyz(pos.x, pos.y, 1.0),
            InteriorPortal {
                idx: i,
                label: portal.label.clone(),
                unlock_event_id: portal.unlock_event_id.clone(),
            },
            InteriorEntity,
        ));
    }

    // Chests — gold quad (placeholder) on top of floor.
    for chest in &map.chests {
        let pos = WorldGrid::tile_to_world(chest.x, chest.y);
        commands.spawn((
            Sprite {
                color: Color::srgb(0.90, 0.70, 0.20),
                custom_size: Some(tile_size * 0.55),
                ..default()
            },
            Transform::from_xyz(pos.x, pos.y, 0.9),
            InteriorEntity,
        ));
    }

    // Monsters — red quad (placeholder to match the interior's colored-quad
    // aesthetic). Real monster sprites inside caves are a later polish pass
    // (needs the monster-atlas loader lifted out of spawn_world into a
    // shared resource so we can reuse it here).
    for (idx, monster) in map.monsters.iter().enumerate() {
        let pos = WorldGrid::tile_to_world(monster.x, monster.y);
        commands.spawn((
            Sprite {
                color: Color::srgb(0.80, 0.25, 0.25),
                custom_size: Some(tile_size * 0.65),
                ..default()
            },
            Transform::from_xyz(pos.x, pos.y, 0.95),
            InteriorMonsterMarker { key: questlib::interior::monster_key(&map.id, idx) },
            InteriorEntity,
        ));
    }

    // HUD "You are in: <Name>" label in the top-center.
    let font: Handle<Font> = asset_server.load("fonts/PressStart2P.ttf");
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(8.0),
            width: Val::Percent(100.0),
            justify_content: JustifyContent::Center,
            ..default()
        },
        InteriorHudLabel,
        InteriorEntity,
    )).with_children(|parent| {
        parent.spawn((
            Text::new(format!("⟐ {}", map.name)),
            TextFont { font, font_size: 12.0, ..default() },
            TextColor(Color::srgb(0.7, 0.9, 1.0)),
        ));
    });

    log::info!("[interior] rendered '{}' ({}x{}, {} portals, {} chests)",
        map.id, map.width, map.height, map.portals.len(), map.chests.len());
    current.map = Some(map);
}

/// Tint locked portals differently so the player can tell a shortcut end
/// hasn't been discovered yet. Unlocked = teal (default); locked = dim
/// orange. Flips back to teal the moment the corresponding CaveEntrance
/// event lands in state.completed_events.
fn sync_portal_lock_color(
    state: Res<MyPlayerState>,
    mut portals: Query<(&InteriorPortal, &mut Sprite)>,
) {
    for (portal, mut sprite) in &mut portals {
        let locked = match &portal.unlock_event_id {
            Some(id) => !state.completed_events.contains(id),
            None => false,
        };
        sprite.color = if locked {
            Color::srgb(0.55, 0.40, 0.20) // dim orange
        } else {
            Color::srgb(0.20, 0.70, 0.85) // teal
        };
    }
}

/// Hide interior monster sprites whose compound key is in the player's
/// defeated_monsters list. Cheap to run every frame — one string lookup
/// per monster, and there are only a handful per cave.
fn sync_monster_visibility(
    state: Res<MyPlayerState>,
    mut monsters: Query<(&InteriorMonsterMarker, &mut Visibility)>,
) {
    for (marker, mut vis) in &mut monsters {
        let should_hide = state.defeated_monsters.contains(&marker.key);
        let target = if should_hide { Visibility::Hidden } else { Visibility::Visible };
        if *vis != target { *vis = target; }
    }
}

fn update_hud_label(
    current: Res<CurrentInterior>,
    mut label_q: Query<&mut Text, With<InteriorHudLabel>>,
) {
    if !current.is_changed() { return; }
    let Ok(mut text) = label_q.get_single_mut() else { return };
    if let Some(ref map) = current.map {
        **text = format!("⟐ {}", map.name);
    }
}

// ── Click handling inside an interior ──────────────

fn handle_interior_click(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform)>,
    current: Res<CurrentInterior>,
    session: Res<GameSession>,
    state: Res<MyPlayerState>,
    portals: Query<(&Transform, &InteriorPortal)>,
    ui_hover: Res<crate::UiHovered>,
) {
    // Only handle clicks while inside an interior. Also require that the
    // interior data has actually landed (we don't want to send routes before
    // we know where the walls are).
    let Some(map) = &current.map else { return };
    if state.location.is_none() { return; }
    if !mouse.just_pressed(MouseButton::Left) || ui_hover.0 { return; }

    let Ok(window) = windows.get_single() else { return };
    let Ok((camera, cam_tf)) = camera_q.get_single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    let Ok(world_pos) = camera.viewport_to_world_2d(cam_tf, cursor) else { return };
    let (tx, ty) = WorldGrid::world_to_tile(world_pos);

    // Clamp to grid bounds.
    if tx >= map.width || ty >= map.height { return; }

    // Portal check first — special-case, bypasses route planning.
    // (We detect via the Portal component's transform matching the clicked
    //  tile, because the grid lookup already told us there's a portal here.)
    if let Some(portal_idx) = map.portal_at(tx, ty) {
        // Must be adjacent to the portal to actually take it — otherwise
        // route them there first and let the step-on-portal logic fire
        // server-side next tick. For Phase 2: route them to the portal; a
        // later pass can auto-call /use_portal when they reach it.
        let _ = (portals, portal_idx); // avoid unused; portals query is intentional for future hover UI
        // Plan a route to the portal tile using the same BFS as walkable clicks.
        let Some(route) = bfs_path(map, (state.tile_x as usize, state.tile_y as usize), (tx, ty)) else { return };
        post_route(&session.player_id, &route);
        // Also send use_portal — the server side will no-op unless the
        // player is actually on the portal tile. Next tick after arrival,
        // the client can also call /use_portal on step-detection. For MVP,
        // we just leave the portal as the destination; the player clicks
        // again on the portal when they've arrived.
        return;
    }

    // Regular walkable floor tile: BFS to path through walls.
    if !map.is_walkable(tx, ty) { return; }
    let Some(route) = bfs_path(map, (state.tile_x as usize, state.tile_y as usize), (tx, ty)) else { return };
    post_route(&session.player_id, &route);
}

/// 4-neighbor BFS on the interior grid. Returns the full path including
/// start and end. None if unreachable.
fn bfs_path(
    map: &InteriorMap,
    start: (usize, usize),
    goal: (usize, usize),
) -> Option<Vec<(usize, usize)>> {
    if start == goal { return Some(vec![start]); }
    if !map.is_walkable(goal.0, goal.1) { return None; }
    let mut came_from: Vec<Option<(usize, usize)>> = vec![None; map.width * map.height];
    let mut visited: Vec<bool> = vec![false; map.width * map.height];
    let mut queue = std::collections::VecDeque::new();
    queue.push_back(start);
    visited[start.1 * map.width + start.0] = true;

    while let Some((x, y)) = queue.pop_front() {
        if (x, y) == goal { break; }
        for (nx, ny) in neighbors(x, y, map.width, map.height) {
            if !map.is_walkable(nx, ny) { continue; }
            let idx = ny * map.width + nx;
            if visited[idx] { continue; }
            visited[idx] = true;
            came_from[idx] = Some((x, y));
            queue.push_back((nx, ny));
        }
    }

    // Reconstruct path back from goal.
    let gidx = goal.1 * map.width + goal.0;
    if !visited[gidx] { return None; }
    let mut path = vec![goal];
    let mut cur = goal;
    while let Some(prev) = came_from[cur.1 * map.width + cur.0] {
        path.push(prev);
        cur = prev;
        if cur == start { break; }
    }
    path.reverse();
    Some(path)
}

fn neighbors(x: usize, y: usize, w: usize, h: usize) -> Vec<(usize, usize)> {
    let mut out = Vec::with_capacity(4);
    if x > 0         { out.push((x - 1, y)); }
    if x + 1 < w     { out.push((x + 1, y)); }
    if y > 0         { out.push((x, y - 1)); }
    if y + 1 < h     { out.push((x, y + 1)); }
    out
}

/// POST /set_route with an interior-tile route. Same shape as overworld —
/// client submits only geometry; server owns route_meters_walked.
fn post_route(player_id: &str, route: &[(usize, usize)]) {
    let route_json = serde_json::to_string(route).unwrap_or_default();
    crate::supabase::write_planned_route(player_id, &route_json);
}

/// Portal destination matches for use-portal eligibility (future wiring).
#[allow(dead_code)]
fn portal_allows_exit(dest: &PortalDest) -> bool {
    matches!(
        dest,
        PortalDest::Overworld { .. }
            | PortalDest::Interior { .. }
            | PortalDest::OverworldReturn
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use questlib::interior::{InteriorTile, Portal};

    fn m() -> InteriorMap {
        // 3x3 with a wall in the middle
        InteriorMap {
            id: "t".into(), name: "T".into(),
            width: 3, height: 3,
            tiles: vec![
                InteriorTile::Floor, InteriorTile::Floor, InteriorTile::Floor,
                InteriorTile::Floor, InteriorTile::Wall,  InteriorTile::Floor,
                InteriorTile::Floor, InteriorTile::Floor, InteriorTile::Floor,
            ],
            portals: vec![Portal { x: 2, y: 2, destination: PortalDest::Overworld { x: 0, y: 0 }, label: "".into(), unlock_event_id: None }],
            chests: vec![],
            monsters: vec![],
            floor_cost_m: 40,
        }
    }

    #[test]
    fn bfs_direct() {
        let path = bfs_path(&m(), (0, 0), (2, 0));
        assert_eq!(path, Some(vec![(0,0), (1,0), (2,0)]));
    }

    #[test]
    fn bfs_around_wall() {
        let path = bfs_path(&m(), (0, 0), (2, 2)).unwrap();
        assert!(path.first() == Some(&(0,0)));
        assert!(path.last()  == Some(&(2,2)));
        assert!(!path.contains(&(1,1)));
    }

    #[test]
    fn bfs_unwalkable_goal() {
        assert_eq!(bfs_path(&m(), (0, 0), (1, 1)), None);
    }
}
