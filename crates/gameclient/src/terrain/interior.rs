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

use super::tilemap::{FogSprite, MapSprite, MyPlayerState};
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
                    handle_interior_click,
                    update_hud_label,
                    sync_monster_visibility,
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
}

#[derive(Component)]
struct InteriorMonsterMarker {
    /// Compound defeated-monsters key: "<interior_id>:monster:<idx>".
    pub key: String,
}

#[derive(Component)]
struct InteriorHudLabel;

// ── Location-change watcher ────────────────────────

/// Detects transitions in MyPlayerState.location:
///   - None → Some(id): kick off the interior fetch
///   - Some(a) → Some(b): kick off fetch for b
///   - Some → None: clear the interior scene and re-show overworld
fn watch_location_changes(
    mut commands: Commands,
    state: Res<MyPlayerState>,
    mut current: ResMut<CurrentInterior>,
    interior_entities: Query<Entity, With<InteriorEntity>>,
    mut map_vis: Query<&mut Visibility, (With<MapSprite>, Without<FogSprite>)>,
    mut fog_vis: Query<&mut Visibility, (With<FogSprite>, Without<MapSprite>)>,
) {
    let desired = state.location.clone();
    let rendered = current.map.as_ref().map(|m| m.id.clone());

    if desired == rendered { return; }

    // Case 1: leaving interior entirely → clear + show overworld
    if desired.is_none() {
        for e in &interior_entities { commands.entity(e).despawn_recursive(); }
        current.map = None;
        current.fetching_id = None;
        for mut v in &mut map_vis { *v = Visibility::Visible; }
        for mut v in &mut fog_vis { *v = Visibility::Visible; }
        return;
    }

    let want_id = desired.unwrap();
    // Case 2: already fetching this one, just wait for the fetch to land.
    if current.fetching_id.as_deref() == Some(&want_id) { return; }

    // Case 3: kick off a fetch for this interior.
    current.fetching_id = Some(want_id.clone());
    let slot = current.fetched.clone();
    let url = crate::api_url(&format!("/interior?id={}", want_id));
    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        match client.get(&url).send().await {
            Ok(resp) => match resp.json::<InteriorMap>().await {
                Ok(map) => {
                    if let Ok(mut lock) = slot.lock() { *lock = Some(map); }
                }
                Err(e) => log::error!("[interior] parse failed: {}", e),
            },
            Err(e) => log::error!("[interior] fetch failed: {}", e),
        }
    });

    // Hide overworld visual entities immediately so the user doesn't see a
    // one-frame flash of the overworld while the fetch is in flight.
    for mut v in &mut map_vis { *v = Visibility::Hidden; }
    for mut v in &mut fog_vis { *v = Visibility::Hidden; }
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

    // Portals — bright teal quad + hover label text above.
    for (i, portal) in map.portals.iter().enumerate() {
        let pos = WorldGrid::tile_to_world(portal.x, portal.y);
        commands.spawn((
            Sprite {
                color: Color::srgb(0.20, 0.70, 0.85),
                custom_size: Some(tile_size * 0.85),
                ..default()
            },
            Transform::from_xyz(pos.x, pos.y, 1.0),
            InteriorPortal { idx: i, label: portal.label.clone() },
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

/// POST /set_route with an interior-tile route. Same shape as overworld.
fn post_route(player_id: &str, route: &[(usize, usize)]) {
    let route_json = serde_json::to_string(route).unwrap_or_default();
    let body = serde_json::json!({
        "player_id": player_id,
        "route": route_json,
        "meters": 0.0,
    });
    let url = crate::api_url("/set_route");
    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let _ = client.post(&url).json(&body).send().await;
    });
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
            portals: vec![Portal { x: 2, y: 2, destination: PortalDest::Overworld { x: 0, y: 0 }, label: "".into() }],
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
