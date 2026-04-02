use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use super::world::{WorldGrid, WORLD_W, WORLD_H, TILE_PX};
use super::path::{DisplayRoute, InterpolationState, find_path, position_and_index_from_route_meters, position_from_route_meters, tile_index_from_meters};
use crate::states::AppState;
use crate::supabase::{self, PolledPlayerState, SupabaseConfig};
use crate::{GameFont, GameSession};

pub struct TilemapPlugin;

impl Plugin for TilemapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::InGame), spawn_world)
            .add_systems(
                Update,
                (
                    apply_server_state,
                    interpolate_movement,
                    render_character,
                ).chain().run_if(in_state(AppState::InGame)),
            )
            .add_systems(
                Update,
                (
                    handle_map_click,
                    handle_clear_route,
                    handle_pan,
                ).run_if(in_state(AppState::InGame)
                    .and(not(crate::combat::combat_active))
                    .and(not(crate::dialogue::dialogue_active))),
            )
            .add_systems(
                Update,
                (
                    handle_zoom,
                    toggle_poi_labels,
                    update_fog_texture,
                    update_camera,
                    handle_debug_menu,
                ).run_if(in_state(AppState::InGame)),
            );
    }
}

// ── Components ────────────────────────────────────────

#[derive(Component)]
struct MapSprite;

#[derive(Component)]
struct FogSprite;

#[derive(Component)]
struct PathMarker;

#[derive(Component)]
pub struct PlayerSprite;

#[derive(Component)]
struct TileInfoText;

#[derive(Component)]
struct PoiLabel;

#[derive(Component)]
struct PlayerNameTag;

#[derive(Component)]
struct DebugMenuUi;

#[derive(Component)]
struct LoadingText;

use questlib::route::Facing;

/// Sprite sheet row for Katan walk animation based on facing direction.
fn facing_base_row(facing: Facing) -> usize {
    match facing {
        Facing::Down => 0,
        Facing::Up => 1,
        Facing::Right => 2,
        Facing::Left => 3,
    }
}

#[derive(Component)]
struct WalkAnimation {
    timer: Timer,
    frame: usize,
    facing: Facing,
    moving: bool,
}

// ── Resources ─────────────────────────────────────────

/// Authoritative player state from server. Updated every poll.
#[derive(Resource, Default)]
pub struct MyPlayerState {
    pub tile_x: i32,
    pub tile_y: i32,
    pub route: Vec<(usize, usize)>,
    pub route_meters: f64,
    pub speed_kmh: f32,
    pub is_walking: bool,
    pub gold: i32,
    pub revealed_tiles: String,
    pub facing: questlib::route::Facing,
    pub total_distance_m: i32,
    pub initialized: bool,
    pub last_poll_tile: (i32, i32),
}

/// Smoothly interpolated visual state, decoupled from server state.
#[derive(Resource)]
struct VisualState {
    pos: Vec2,
    initialized: bool,
}

impl Default for VisualState {
    fn default() -> Self { Self { pos: Vec2::ZERO, initialized: false } }
}

#[derive(Resource, Default)]
struct CameraPan { active: bool, last_pos: Option<Vec2> }

#[derive(Resource)]
struct DebugOptions { show_menu: bool, fog_disabled: bool, show_pois: bool }

impl Default for DebugOptions {
    fn default() -> Self { Self { show_menu: false, fog_disabled: false, show_pois: false } }
}

#[derive(Resource)]
struct FogOfWar {
    revealed: Vec<bool>,
    dirty: bool,
}

impl FogOfWar {
    fn new() -> Self { Self { revealed: vec![false; WORLD_W * WORLD_H], dirty: true } }
    fn reveal_around(&mut self, cx: usize, cy: usize, radius: usize) {
        let r = radius as i32;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy > r * r { continue; }
                let x = cx as i32 + dx;
                let y = cy as i32 + dy;
                if x >= 0 && x < WORLD_W as i32 && y >= 0 && y < WORLD_H as i32 {
                    let idx = y as usize * WORLD_W + x as usize;
                    if !self.revealed[idx] { self.revealed[idx] = true; self.dirty = true; }
                }
            }
        }
    }
    fn is_revealed(&self, x: usize, y: usize) -> bool {
        if x < WORLD_W && y < WORLD_H { self.revealed[y * WORLD_W + x] } else { false }
    }
}

// ── Texture Baking (unchanged) ────────────────────────

fn bake_map_texture(world: &WorldGrid, tileset_img: &Image, tileset_cols: usize) -> Image {
    let map_w = WORLD_W * 16;
    let map_h = WORLD_H * 16;
    let mut pixels = vec![0u8; map_w * map_h * 4];
    let ts_w = tileset_img.width() as usize;
    let ts_data = &tileset_img.data;
    let tile_slot = 20;
    for y in 0..WORLD_H {
        for x in 0..WORLD_W {
            let ground = world.get_ground(x, y);
            blit_tile(&mut pixels, map_w, x * 16, y * 16, ts_data, ts_w, ground.tile_index_varied(x, y), tileset_cols, tile_slot);
            if let Some(overlay) = world.cells[y][x].overlay {
                blit_tile_alpha(&mut pixels, map_w, x * 16, y * 16, ts_data, ts_w, overlay.tile_index_varied(x, y), tileset_cols, tile_slot);
            }
        }
    }
    Image::new(Extent3d { width: map_w as u32, height: map_h as u32, depth_or_array_layers: 1 }, TextureDimension::D2, pixels, TextureFormat::Rgba8UnormSrgb, default())
}

fn blit_tile(dst: &mut [u8], dst_w: usize, dx: usize, dy: usize, src: &[u8], src_w: usize, tile_idx: usize, cols: usize, slot: usize) {
    let col = tile_idx % cols; let row = tile_idx / cols;
    let sx = col * slot + 2; let sy = row * slot + 2;
    for py in 0..16 { for px in 0..16 {
        let si = ((sy + py) * src_w + (sx + px)) * 4;
        let di = ((dy + py) * dst_w + (dx + px)) * 4;
        if si + 3 < src.len() && di + 3 < dst.len() { dst[di..di+4].copy_from_slice(&src[si..si+4]); }
    }}
}

fn blit_tile_alpha(dst: &mut [u8], dst_w: usize, dx: usize, dy: usize, src: &[u8], src_w: usize, tile_idx: usize, cols: usize, slot: usize) {
    let col = tile_idx % cols; let row = tile_idx / cols;
    let sx = col * slot + 2; let sy = row * slot + 2;
    for py in 0..16 { for px in 0..16 {
        let si = ((sy + py) * src_w + (sx + px)) * 4;
        let di = ((dy + py) * dst_w + (dx + px)) * 4;
        if si + 3 < src.len() && di + 3 < dst.len() && src[si + 3] > 128 { dst[di..di+4].copy_from_slice(&src[si..si+4]); }
    }}
}

fn create_fog_texture(fog: &FogOfWar, debug: &DebugOptions) -> Image {
    let w = WORLD_W * 16; let h = WORLD_H * 16;
    let mut pixels = vec![0u8; w * h * 4];
    for ty in 0..WORLD_H { for tx in 0..WORLD_W {
        let revealed = debug.fog_disabled || fog.is_revealed(tx, ty);
        let (r, g, b, a) = if revealed { (0, 0, 0, 0) } else { (15, 15, 25, 255) };
        for py in 0..16 { for px in 0..16 {
            let idx = ((ty * 16 + py) * w + (tx * 16 + px)) * 4;
            pixels[idx] = r; pixels[idx+1] = g; pixels[idx+2] = b; pixels[idx+3] = a;
        }}
    }}
    Image::new(Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 }, TextureDimension::D2, pixels, TextureFormat::Rgba8UnormSrgb, default())
}

// ── Spawn World ───────────────────────────────────────

fn spawn_world(
    mut commands: Commands,
    font: Res<GameFont>,
    asset_server: Res<AssetServer>,
    mut images: ResMut<Assets<Image>>,
    mut atlases: ResMut<Assets<TextureAtlasLayout>>,
) {
    let world = WorldGrid::from_seed(42);

    let tileset_bytes = include_bytes!("../../assets/tilesets/miniworld.png");
    let tileset_dyn = image::load_from_memory(tileset_bytes).expect("tileset");
    let tileset_rgba = tileset_dyn.to_rgba8();
    let (ts_w, ts_h) = tileset_rgba.dimensions();
    let tileset_img = Image::new(Extent3d { width: ts_w, height: ts_h, depth_or_array_layers: 1 }, TextureDimension::D2, tileset_rgba.into_raw(), TextureFormat::Rgba8UnormSrgb, default());

    let map_img = bake_map_texture(&world, &tileset_img, 16);
    let map_handle = images.add(map_img);
    let map_cx = (WORLD_W as f32 * TILE_PX) / 2.0 - TILE_PX / 2.0;
    let map_cy = -(WORLD_H as f32 * TILE_PX) / 2.0 + TILE_PX / 2.0;

    commands.spawn((Sprite { image: map_handle, ..default() }, Transform::from_xyz(map_cx, map_cy, 0.0), Visibility::Hidden, MapSprite));

    let fog = FogOfWar::new();
    let debug = DebugOptions::default();
    let fog_img = create_fog_texture(&fog, &debug);
    let fog_handle = images.add(fog_img);
    commands.spawn((Sprite { image: fog_handle, ..default() }, Transform::from_xyz(map_cx, map_cy, 2.0), Visibility::Hidden, FogSprite));

    // Player character (hidden until server sends position)
    let champion_tex: Handle<Image> = asset_server.load("sprites/Katan.png");
    let layout = TextureAtlasLayout::from_grid(UVec2::new(16, 16), 6, 8, None, None);
    let layout_handle = atlases.add(layout);
    commands.spawn((
        Sprite { image: champion_tex, texture_atlas: Some(TextureAtlas { layout: layout_handle, index: 0 }), ..default() },
        Transform::from_xyz(0.0, 0.0, 5.0), Visibility::Hidden, PlayerSprite,
        WalkAnimation { timer: Timer::from_seconds(0.15, TimerMode::Repeating), frame: 0, facing: Facing::Down, moving: false },
    ));
    commands.spawn((Text2d::new(""), TextFont { font: font.0.clone(), font_size: 8.0, ..default() }, TextColor(Color::srgb(0.1, 0.1, 0.1)), Transform::from_xyz(0.0, 12.0, 6.0), Visibility::Hidden, PlayerNameTag));
    commands.spawn((Text2d::new(""), TextFont { font: font.0.clone(), font_size: 8.0, ..default() }, TextColor(Color::srgb(1.0, 1.0, 1.0)), Transform::from_xyz(0.0, 0.0, 10.0), Visibility::Hidden, TileInfoText));

    // Loading text
    commands.spawn((Node { position_type: PositionType::Absolute, top: Val::Percent(45.0), width: Val::Percent(100.0), justify_content: JustifyContent::Center, ..default() }, LoadingText))
        .with_children(|p| { p.spawn((Text::new("Loading world..."), TextFont { font: font.0.clone(), font_size: 16.0, ..default() }, TextColor(Color::srgb(0.77, 0.64, 0.35)))); });

    // POI labels
    for poi in &world.map.pois {
        let pos = WorldGrid::tile_to_world(poi.x, poi.y);
        commands.spawn((Text2d::new(format!("{:?}", poi.poi_type)), TextFont { font: font.0.clone(), font_size: 8.0, ..default() }, TextColor(Color::srgb(0.1, 0.1, 0.1)), Transform::from_xyz(pos.x, pos.y - 12.0, 8.0), Visibility::Hidden, PoiLabel));
    }

    commands.insert_resource(fog);
    commands.insert_resource(debug);
    commands.insert_resource(MyPlayerState::default());
    commands.insert_resource(DisplayRoute::default());
    commands.insert_resource(InterpolationState::default());
    commands.insert_resource(VisualState::default());
    commands.insert_resource(CameraPan::default());
    commands.insert_resource(world);
}

// ── Core Systems ──────────────────────────────────────

/// Read polled data from server → update MyPlayerState.
fn apply_server_state(
    polled: Res<PolledPlayerState>,
    session: Res<GameSession>,
    mut state: ResMut<MyPlayerState>,
    mut interp: ResMut<InterpolationState>,
    mut display_route: ResMut<DisplayRoute>,
    mut fog: ResMut<FogOfWar>,
    mut commands: Commands,
    mut player_tf: Query<(&mut Transform, &mut Visibility), (With<PlayerSprite>, Without<Camera2d>, Without<MapSprite>, Without<FogSprite>)>,
    mut camera_tf: Query<&mut Transform, (With<Camera2d>, Without<PlayerSprite>)>,
    mut map_vis: Query<&mut Visibility, (With<MapSprite>, Without<PlayerSprite>, Without<FogSprite>)>,
    mut fog_vis: Query<&mut Visibility, (With<FogSprite>, Without<PlayerSprite>, Without<MapSprite>)>,
    loading_q: Query<Entity, With<LoadingText>>,
    path_markers: Query<Entity, With<PathMarker>>,
    world: Option<Res<WorldGrid>>,
) {
    let Ok(players) = polled.players.lock() else { return };
    if players.is_empty() || session.player_name.is_empty() { return; }
    let Some(me) = players.iter().find(|p| p.name.eq_ignore_ascii_case(&session.player_name)) else { return };

    let tile_changed = me.map_tile_x.unwrap_or(0) != state.tile_x || me.map_tile_y.unwrap_or(0) != state.tile_y;

    // Update state from server
    state.tile_x = me.map_tile_x.unwrap_or(0);
    state.tile_y = me.map_tile_y.unwrap_or(0);
    state.speed_kmh = me.current_speed_kmh;
    state.is_walking = me.is_walking;
    state.gold = me.gold;
    state.facing = me.facing;
    state.total_distance_m = me.total_distance_m;

    // Parse route from server — check if server has caught up to local changes.
    let server_in_sync = if let Some(ref route_json) = me.planned_route {
        if !route_json.is_empty() {
            if let Some(route) = questlib::route::parse_route_json(route_json) {
                if !display_route.locally_modified {
                    state.route = route.clone();
                    display_route.waypoints = route;
                    true
                } else if state.route == route {
                    // Server caught up to our local route — clear flag
                    display_route.locally_modified = false;
                    true
                } else {
                    false // server still has stale route
                }
            } else {
                !display_route.locally_modified
            }
        } else {
            if !display_route.locally_modified {
                state.route.clear();
                display_route.waypoints.clear();
                true
            } else if display_route.waypoints.is_empty() {
                // Server confirmed empty route matches our local clear
                display_route.locally_modified = false;
                true
            } else {
                false
            }
        }
    } else {
        !display_route.locally_modified
    };

    // Only accept server meters/interp when the server has our current route.
    // Otherwise its meters refer to a stale route and would cause jumps.
    if server_in_sync {
        let server_meters = me.route_meters_walked.unwrap_or(0.0);
        let target = me.interp_meters_target.unwrap_or(server_meters);
        let duration = me.interp_duration_secs.unwrap_or(0.0);
        state.route_meters = server_meters;
        interp.start_meters = server_meters;
        interp.target_meters = target;
        interp.duration = duration;
        interp.elapsed = 0.0;
    }


    // Update fog from server
    if let Some(ref encoded) = me.revealed_tiles {
        if !encoded.is_empty() {
            if let Some(server_fog) = questlib::fog::FogBitfield::from_base64(encoded) {
                for y in 0..WORLD_H {
                    for x in 0..WORLD_W {
                        if server_fog.is_revealed(x, y) && !fog.is_revealed(x, y) {
                            fog.revealed[y * WORLD_W + x] = true;
                            fog.dirty = true;
                        }
                    }
                }
            }
        }
    }

    // First init — show everything, snap camera
    if !state.initialized {
        state.initialized = true;

        let pos = WorldGrid::tile_to_world(state.tile_x as usize, state.tile_y as usize);
        for (mut tf, mut vis) in &mut player_tf { tf.translation.x = pos.x; tf.translation.y = pos.y; *vis = Visibility::Visible; }
        for mut vis in &mut map_vis { *vis = Visibility::Visible; }
        for mut vis in &mut fog_vis { *vis = Visibility::Visible; }
        for mut cam in &mut camera_tf { cam.translation.x = pos.x; cam.translation.y = pos.y; }
        for entity in &loading_q { commands.entity(entity).despawn_recursive(); }
    }

    // Redraw path markers when tile changes (but not if user just modified route)
    if (tile_changed || !state.initialized) && !display_route.locally_modified {
        for entity in &path_markers { commands.entity(entity).despawn(); }
        if let Some(world) = &world {
            let tile_idx = tile_index_from_meters(&state.route, state.route_meters, world);
            draw_path_markers(&mut commands, &display_route.waypoints, tile_idx, &fog);
        }
    }

    state.last_poll_tile = (state.tile_x, state.tile_y);
}

/// Between polls: advance interpolation timer. The actual meters are computed
/// by InterpolationState::current_meters() which lerps between server-confirmed
/// position and the server's projected target. Can never overshoot.
fn interpolate_movement(
    time: Res<Time>,
    mut interp: ResMut<InterpolationState>,
) {
    if interp.duration > 0.0 {
        interp.elapsed += time.delta_secs();
    }
}

/// Set character position with smooth interpolation.
fn render_character(
    state: Res<MyPlayerState>,
    interp: Res<InterpolationState>,
    mut visual: ResMut<VisualState>,
    session: Res<GameSession>,
    time: Res<Time>,
    world: Option<Res<WorldGrid>>,
    mut player_q: Query<(&mut Transform, &mut WalkAnimation, &mut Sprite), With<PlayerSprite>>,
    mut nametag_q: Query<(&mut Transform, &mut Visibility), (With<PlayerNameTag>, Without<PlayerSprite>)>,
    keys: Res<ButtonInput<KeyCode>>,
) {
    if !state.initialized { return; }

    // Compute target position and facing from route
    let total_meters = interp.current_meters();
    let (target_pos, visual_facing) = if !state.route.is_empty() {
        if let Some(world) = &world {
            if let Some((pos, idx)) = position_and_index_from_route_meters(&state.route, total_meters, world) {
                let facing = questlib::route::facing_along_route(&state.route, idx);
                (pos, facing)
            } else {
                (WorldGrid::tile_to_world(state.tile_x as usize, state.tile_y as usize), state.facing)
            }
        } else {
            (WorldGrid::tile_to_world(state.tile_x as usize, state.tile_y as usize), state.facing)
        }
    } else {
        (WorldGrid::tile_to_world(state.tile_x as usize, state.tile_y as usize), state.facing)
    };

    // Initialize visual position on first frame (snap, don't lerp)
    if !visual.initialized {
        visual.pos = target_pos;
        visual.initialized = true;
    }

    // Smoothly interpolate visual position toward target.
    // Uses exponential decay: lerp factor = 1 - e^(-rate * dt)
    // Rate of 6 gives smooth movement (~115ms to close half the gap).
    let dt = time.delta_secs();
    let lerp_factor = 1.0 - (-6.0_f32 * dt).exp();
    visual.pos = visual.pos.lerp(target_pos, lerp_factor);

    // Snap if very close to avoid perpetual micro-drift
    if visual.pos.distance_squared(target_pos) < 0.01 {
        visual.pos = target_pos;
    }

    for (mut tf, mut anim, mut sprite) in &mut player_q {
        tf.translation.x = visual.pos.x;
        tf.translation.y = visual.pos.y;

        // Derive facing from the visual position on the route
        anim.facing = visual_facing;

        sprite.flip_x = false;

        let should_animate = state.is_walking && state.speed_kmh > 0.1;
        if should_animate {
            let speed_factor = state.speed_kmh.clamp(0.5, 6.0);
            anim.timer.set_duration(std::time::Duration::from_secs_f32(0.3 / speed_factor));
            anim.timer.tick(time.delta());
            if anim.timer.just_finished() { anim.frame = (anim.frame % 4) + 1; }
            let row = facing_base_row(anim.facing);
            if let Some(ref mut atlas) = sprite.texture_atlas { atlas.index = row * 6 + anim.frame; }
            anim.moving = true;
        } else if anim.moving {
            anim.moving = false;
            anim.frame = 0;
            let row = facing_base_row(anim.facing);
            if let Some(ref mut atlas) = sprite.texture_atlas { atlas.index = row * 6; }
        }
    }

    // Name tag
    if let Ok((player_tf, _, _)) = player_q.get_single() {
        let show = keys.pressed(KeyCode::Tab);
        for (mut tf, mut vis) in &mut nametag_q {
            tf.translation.x = player_tf.translation.x;
            tf.translation.y = player_tf.translation.y + 12.0;
            *vis = if show { Visibility::Visible } else { Visibility::Hidden };
        }
    }
}

// ── Route Planning ────────────────────────────────────

fn handle_map_click(
    mouse: Res<ButtonInput<MouseButton>>,
    keys: Res<ButtonInput<KeyCode>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform)>,
    world: Res<WorldGrid>,
    fog: Res<FogOfWar>,
    debug: Res<DebugOptions>,
    config: Res<SupabaseConfig>,
    session: Res<GameSession>,
    mut state: ResMut<MyPlayerState>,
    mut display_route: ResMut<DisplayRoute>,
    mut interp: ResMut<InterpolationState>,
    mut commands: Commands,
    path_markers: Query<Entity, With<PathMarker>>,
    mut info_q: Query<(&mut Text2d, &mut Transform), (With<TileInfoText>, Without<PlayerSprite>)>,
) {
    let Ok(window) = windows.get_single() else { return };
    let Ok((camera, cam_tf)) = camera_q.get_single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    let Ok(world_pos) = camera.viewport_to_world_2d(cam_tf, cursor) else { return };

    let (tx, ty) = WorldGrid::world_to_tile(world_pos);
    let terrain = world.get(tx, ty);

    // Tile info on TAB
    if let Ok((mut text, mut transform)) = info_q.get_single_mut() {
        if keys.pressed(KeyCode::Tab) {
            if fog.is_revealed(tx, ty) || debug.fog_disabled {
                let cost = if terrain.is_passable() { format!("{}m", terrain.movement_cost()) } else { "impassable".into() };
                *text = Text2d::new(format!("{} {}", terrain.name(), cost));
            } else { *text = Text2d::new("???"); }
            let p = WorldGrid::tile_to_world(tx, ty);
            transform.translation = Vec3::new(p.x, p.y + 16.0, 10.0);
        } else { *text = Text2d::new(""); }
    }

    // Click to plan route
    let is_revealed = fog.is_revealed(tx, ty) || debug.fog_disabled;
    if mouse.just_pressed(MouseButton::Left) && terrain.is_passable() && is_revealed {
        let current_pos = (state.tile_x as usize, state.tile_y as usize);
        let has_active_route = !display_route.waypoints.is_empty();

        // Determine pathfinding start: extend from last waypoint, or start fresh
        let start = if has_active_route {
            *display_route.waypoints.last().unwrap()
        } else {
            current_pos
        };
        if start == (tx, ty) { return; }

        if let Some(mut segment) = find_path(&world, start, (tx, ty)) {
            let send_meters;
            let marker_skip;

            if has_active_route {
                // Extending: append new segment, preserve walked progress.
                if !segment.is_empty() { segment.remove(0); }
                display_route.waypoints.extend(segment);
                let current_meters = interp.current_meters();
                send_meters = Some(current_meters);
                marker_skip = tile_index_from_meters(&display_route.waypoints, current_meters, &world);
            } else {
                // Fresh route from current position
                display_route.waypoints = segment;
                state.route_meters = 0.0;
                interp.start_meters = 0.0;
                interp.target_meters = 0.0;
                interp.elapsed = 0.0;
                interp.duration = 0.0;
                send_meters = None;
                marker_skip = 0;
            }
            display_route.locally_modified = true;
            state.route = display_route.waypoints.clone();

            // Redraw markers (skip already-walked tiles)
            for entity in &path_markers { commands.entity(entity).despawn(); }
            draw_path_markers(&mut commands, &display_route.waypoints, marker_skip, &fog);

            // Send to server (with meters when extending, so server preserves progress)
            let route_json = questlib::route::encode_route_json(&display_route.waypoints);
            supabase::write_planned_route(&config, &session.player_id, &route_json, send_meters);
        }
    }
}

fn handle_clear_route(
    keys: Res<ButtonInput<KeyCode>>,
    config: Res<SupabaseConfig>,
    session: Res<GameSession>,
    mut state: ResMut<MyPlayerState>,
    mut display_route: ResMut<DisplayRoute>,
    mut interp: ResMut<InterpolationState>,
    world: Res<WorldGrid>,
    mut commands: Commands,
    path_markers: Query<Entity, With<PathMarker>>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        // Snap tile position to where the character visually is on the route,
        // so we don't jump back to the last server-confirmed tile.
        if !state.route.is_empty() {
            let current_meters = interp.current_meters();
            let idx = tile_index_from_meters(&state.route, current_meters, &world);
            if let Some(&(tx, ty)) = state.route.get(idx) {
                state.tile_x = tx as i32;
                state.tile_y = ty as i32;
            }
        }

        display_route.waypoints.clear();
        display_route.locally_modified = true;
        state.route.clear();
        state.route_meters = 0.0;
        interp.start_meters = 0.0;
        interp.target_meters = 0.0;
        interp.elapsed = 0.0;
        interp.duration = 0.0;
        for entity in &path_markers { commands.entity(entity).despawn(); }
        supabase::write_planned_route(&config, &session.player_id, "", None);
    }
}

/// Draw dashed path markers from a given tile index onward.
fn draw_path_markers(commands: &mut Commands, waypoints: &[(usize, usize)], skip_until: usize, fog: &FogOfWar) {
    let len = waypoints.len();
    if len == 0 { return; }

    let dash_len = 4.0_f32;
    let gap_len = 3.0_f32;
    let line_width = 1.5_f32;

    let start = (skip_until + 1).min(len);
    for i in start..len {
        let p1 = WorldGrid::tile_to_world(waypoints[i - 1].0, waypoints[i - 1].1);
        let p2 = WorldGrid::tile_to_world(waypoints[i].0, waypoints[i].1);
        let dx = p2.x - p1.x; let dy = p2.y - p1.y;
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 0.1 { continue; }
        let nx = dx / seg_len; let ny = dy / seg_len;

        let mut d = 0.0_f32;
        let mut drawing = true;
        while d < seg_len {
            if drawing {
                let end = (d + dash_len).min(seg_len);
                let cx = p1.x + nx * (d + end) * 0.5;
                let cy = p1.y + ny * (d + end) * 0.5;
                let length = end - d;
                let (w, h) = if nx.abs() > ny.abs() { (length, line_width) } else { (line_width, length) };
                let (tile_x, tile_y) = WorldGrid::world_to_tile(Vec2::new(cx, cy));
                let color = if fog.is_revealed(tile_x, tile_y) { Color::srgba(0.0, 0.0, 0.0, 0.7) } else { Color::srgba(1.0, 1.0, 1.0, 0.7) };
                commands.spawn((Sprite { color, custom_size: Some(Vec2::new(w, h)), ..default() }, Transform::from_xyz(cx, cy, 3.0), PathMarker));
                d = end + gap_len;
            } else { d += gap_len; }
            drawing = !drawing;
        }
    }

    // Flag at destination
    if len > start {
        let pos = WorldGrid::tile_to_world(waypoints[len - 1].0, waypoints[len - 1].1);
        commands.spawn((Sprite { color: Color::srgb(0.3, 0.2, 0.1), custom_size: Some(Vec2::new(1.5, 14.0)), ..default() }, Transform::from_xyz(pos.x - 3.0, pos.y + 4.0, 3.5), PathMarker));
        commands.spawn((Sprite { color: Color::srgb(0.9, 0.2, 0.1), custom_size: Some(Vec2::new(8.0, 6.0)), ..default() }, Transform::from_xyz(pos.x + 1.0, pos.y + 9.0, 3.6), PathMarker));
    }
}

// ── Camera / UI Systems (mostly unchanged) ────────────

fn handle_pan(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    mut pan: ResMut<CameraPan>,
    mut camera_q: Query<(&mut Transform, &OrthographicProjection), With<Camera2d>>,
) {
    let Ok(window) = windows.get_single() else { return };
    let Ok((mut cam, proj)) = camera_q.get_single_mut() else { return };
    if mouse.pressed(MouseButton::Right) {
        if let Some(cursor) = window.cursor_position() {
            if let Some(last) = pan.last_pos { let d = cursor - last; cam.translation.x -= d.x * proj.scale; cam.translation.y += d.y * proj.scale; }
            pan.last_pos = Some(cursor); pan.active = true;
        }
    } else { pan.last_pos = None; pan.active = false; }
}

#[derive(Resource)]
struct ZoomTarget { target: f32 }
impl Default for ZoomTarget { fn default() -> Self { Self { target: 0.4 } } }

fn handle_zoom(
    mut scroll_evr: EventReader<bevy::input::mouse::MouseWheel>,
    mut camera_q: Query<&mut OrthographicProjection, With<Camera2d>>,
    mut zoom: Local<ZoomTarget>,
    time: Res<Time>,
) {
    let Ok(mut proj) = camera_q.get_single_mut() else { return };
    for ev in scroll_evr.read() {
        if ev.y > 0.0 { zoom.target = (zoom.target * 0.75).max(0.15); }
        else if ev.y < 0.0 { zoom.target = (zoom.target * 1.5).min(3.0); }
    }
    let diff = zoom.target - proj.scale;
    proj.scale += diff * (1.0 - (-6.0 * time.delta_secs()).exp());
}

fn toggle_poi_labels(keys: Res<ButtonInput<KeyCode>>, mut labels: Query<&mut Visibility, With<PoiLabel>>, debug: Res<DebugOptions>) {
    let show = keys.pressed(KeyCode::Tab) || debug.show_pois;
    for mut vis in &mut labels { *vis = if show { Visibility::Visible } else { Visibility::Hidden }; }
}

fn update_fog_texture(
    mut fog: ResMut<FogOfWar>,
    debug: Res<DebugOptions>,
    fog_q: Query<&Sprite, With<FogSprite>>,
    mut images: ResMut<Assets<Image>>,
) {
    if !fog.dirty { return; }
    fog.dirty = false;
    let Ok(sprite) = fog_q.get_single() else { return };
    let Some(image) = images.get_mut(sprite.image.id()) else { return };
    let w = WORLD_W * 16;
    for ty in 0..WORLD_H { for tx in 0..WORLD_W {
        let revealed = debug.fog_disabled || fog.is_revealed(tx, ty);
        let (r, g, b, a) = if revealed { (0, 0, 0, 0) } else { (15, 15, 25, 255) };
        for py in 0..16 { for px in 0..16 {
            let idx = ((ty * 16 + py) * w + (tx * 16 + px)) * 4;
            image.data[idx] = r; image.data[idx+1] = g; image.data[idx+2] = b; image.data[idx+3] = a;
        }}
    }}
}

fn update_camera(
    player_q: Query<&Transform, With<PlayerSprite>>,
    mut camera_q: Query<(&mut Transform, &mut OrthographicProjection), (With<Camera2d>, Without<PlayerSprite>)>,
    pan: Res<CameraPan>,
    mut initialized: Local<bool>,
) {
    let Some(ptf) = player_q.iter().next() else { return };
    let Ok((mut cam, mut proj)) = camera_q.get_single_mut() else { return };
    if !*initialized { proj.scale = 0.4; *initialized = true; }
    if !pan.active {
        cam.translation.x += (ptf.translation.x - cam.translation.x) * 0.05;
        cam.translation.y += (ptf.translation.y - cam.translation.y) * 0.05;
    }
    let ps = 1.0 / proj.scale;
    cam.translation.x = (cam.translation.x * ps).round() / ps;
    cam.translation.y = (cam.translation.y * ps).round() / ps;
}

fn handle_debug_menu(
    keys: Res<ButtonInput<KeyCode>>,
    mut debug: ResMut<DebugOptions>,
    mut fog: ResMut<FogOfWar>,
    mut commands: Commands,
    font: Res<GameFont>,
    time: Res<Time>,
    existing: Query<Entity, With<DebugMenuUi>>,
    mut poi_labels: Query<&mut Visibility, With<PoiLabel>>,
) {
    if keys.just_pressed(KeyCode::F3) { debug.show_menu = !debug.show_menu; }
    if !debug.show_menu { for e in &existing { commands.entity(e).despawn_recursive(); } return; }
    if keys.just_pressed(KeyCode::Digit1) { debug.fog_disabled = !debug.fog_disabled; fog.dirty = true; }
    if keys.just_pressed(KeyCode::Digit2) { debug.show_pois = !debug.show_pois; }
    for mut vis in &mut poi_labels { *vis = if debug.show_pois { Visibility::Visible } else { Visibility::Hidden }; }
    for e in &existing { commands.entity(e).despawn_recursive(); }
    let fps = (1.0 / time.delta_secs()).round() as u32;
    let text = format!("=== DEBUG (F3) ===\nFPS: {}\n1: Fog [{:}]\n2: POIs [{}]", fps, if debug.fog_disabled {"OFF"} else {"ON"}, if debug.show_pois {"ON"} else {"OFF"});
    commands.spawn((Text::new(text), TextFont { font: font.0.clone(), font_size: 10.0, ..default() }, TextColor(Color::srgb(1.0, 1.0, 0.0)), Node { position_type: PositionType::Absolute, top: Val::Px(10.0), left: Val::Px(10.0), ..default() }, DebugMenuUi));
}
