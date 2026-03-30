use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use super::world::{WorldGrid, WORLD_W, WORLD_H, TILE_PX};
use super::path::{PlannedRoute, find_path};
use crate::states::AppState;
use crate::supabase::{self, PolledPlayerState, SupabaseConfig};
use crate::{GameFont, GameSession};

pub struct TilemapPlugin;

impl Plugin for TilemapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::InGame), spawn_world)
            .add_systems(
                Update,
                (handle_map_click, handle_zoom, handle_pan, handle_clear_route, toggle_poi_labels, update_fog_texture, sync_from_supabase, check_path_redraw, update_path_visuals, update_camera, handle_debug_menu)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Component)]
struct MapSprite;

#[derive(Component)]
struct FogSprite;

#[derive(Component)]
struct PathMarker;

#[derive(Component)]
struct PlayerSprite;

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

#[derive(Clone, Copy, PartialEq)]
enum Direction { Down, Up, Right, Left }

impl Direction {
    /// Walk rows for Katan (6 cols x 8 rows):
    /// Rows 0-1: down (idle/walk), Rows 2-3: up (idle/walk)
    /// Rows 4-7: attack animations (not used for walking)
    /// For left/right walking, use down walk row.
    fn base_row(self) -> usize {
        match self {
            Direction::Down => 0,
            Direction::Up => 2,
            Direction::Right => 0,  // no dedicated right walk — use down
            Direction::Left => 0,   // no dedicated left walk — use down
        }
    }

    fn num_frames(self) -> usize {
        6 // Katan has 6 columns
    }
}

#[derive(Component)]
struct WalkAnimation {
    timer: Timer,
    frame: usize,
    direction: Direction,
    moving: bool,
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
    revealed: Vec<bool>, // flat array: y * WORLD_W + x
    sight_radius: usize,
    dirty: bool,
}

impl FogOfWar {
    fn new() -> Self {
        Self { revealed: vec![false; WORLD_W * WORLD_H], sight_radius: 5, dirty: true }
    }
    fn reveal_around(&mut self, cx: usize, cy: usize) {
        let r = self.sight_radius as i32;
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy > r * r { continue; }
                let x = cx as i32 + dx;
                let y = cy as i32 + dy;
                if x >= 0 && x < WORLD_W as i32 && y >= 0 && y < WORLD_H as i32 {
                    let idx = y as usize * WORLD_W + x as usize;
                    if !self.revealed[idx] {
                        self.revealed[idx] = true;
                        self.dirty = true;
                    }
                }
            }
        }
    }
    fn is_revealed(&self, x: usize, y: usize) -> bool {
        if x < WORLD_W && y < WORLD_H { self.revealed[y * WORLD_W + x] } else { false }
    }
}

/// Bake the world map into a single texture.
fn bake_map_texture(
    world: &WorldGrid,
    tileset_img: &Image,
    tileset_cols: usize,
) -> Image {
    let map_w = WORLD_W * 16;
    let map_h = WORLD_H * 16;
    let mut pixels = vec![0u8; map_w * map_h * 4];

    let ts_w = tileset_img.width() as usize;
    let ts_data = &tileset_img.data;
    let tile_slot = 20; // 16px tile + 2px padding on each side

    for y in 0..WORLD_H {
        for x in 0..WORLD_W {
            // Ground tile
            let ground = world.get_ground(x, y);
            let tile_idx = ground.tile_index_varied(x, y);
            blit_tile(&mut pixels, map_w, x * 16, y * 16, ts_data, ts_w, tile_idx, tileset_cols, tile_slot);

            // Overlay tile
            if let Some(overlay) = world.cells[y][x].overlay {
                let ov_idx = overlay.tile_index_varied(x, y);
                blit_tile_alpha(&mut pixels, map_w, x * 16, y * 16, ts_data, ts_w, ov_idx, tileset_cols, tile_slot);
            }
        }
    }

    Image::new(
        Extent3d { width: map_w as u32, height: map_h as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        default(),
    )
}

fn blit_tile(dst: &mut [u8], dst_w: usize, dx: usize, dy: usize, src: &[u8], src_w: usize, tile_idx: usize, cols: usize, slot: usize) {
    let col = tile_idx % cols;
    let row = tile_idx / cols;
    let sx = col * slot + 2; // skip 2px padding
    let sy = row * slot + 2;

    for py in 0..16 {
        for px in 0..16 {
            let si = ((sy + py) * src_w + (sx + px)) * 4;
            let di = ((dy + py) * dst_w + (dx + px)) * 4;
            if si + 3 < src.len() && di + 3 < dst.len() {
                dst[di] = src[si];
                dst[di + 1] = src[si + 1];
                dst[di + 2] = src[si + 2];
                dst[di + 3] = src[si + 3];
            }
        }
    }
}

fn blit_tile_alpha(dst: &mut [u8], dst_w: usize, dx: usize, dy: usize, src: &[u8], src_w: usize, tile_idx: usize, cols: usize, slot: usize) {
    let col = tile_idx % cols;
    let row = tile_idx / cols;
    let sx = col * slot + 2;
    let sy = row * slot + 2;

    for py in 0..16 {
        for px in 0..16 {
            let si = ((sy + py) * src_w + (sx + px)) * 4;
            let di = ((dy + py) * dst_w + (dx + px)) * 4;
            if si + 3 < src.len() && di + 3 < dst.len() && src[si + 3] > 128 {
                dst[di] = src[si];
                dst[di + 1] = src[si + 1];
                dst[di + 2] = src[si + 2];
                dst[di + 3] = 255;
            }
        }
    }
}

/// Create fog overlay texture (dark where unrevealed).
fn create_fog_texture(fog: &FogOfWar, debug: &DebugOptions) -> Image {
    let w = WORLD_W * 16;
    let h = WORLD_H * 16;
    let mut pixels = vec![0u8; w * h * 4];

    for ty in 0..WORLD_H {
        for tx in 0..WORLD_W {
            let revealed = debug.fog_disabled || fog.is_revealed(tx, ty);
            let (r, g, b, a) = if revealed { (0, 0, 0, 0) } else { (15, 15, 25, 255) };

            for py in 0..16 {
                for px in 0..16 {
                    let idx = ((ty * 16 + py) * w + (tx * 16 + px)) * 4;
                    pixels[idx] = r;
                    pixels[idx + 1] = g;
                    pixels[idx + 2] = b;
                    pixels[idx + 3] = a;
                }
            }
        }
    }

    Image::new(
        Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        pixels,
        TextureFormat::Rgba8UnormSrgb,
        default(),
    )
}

// ── Spawn ─────────────────────────────────────────────

fn spawn_world(
    mut commands: Commands,
    font: Res<GameFont>,
    asset_server: Res<AssetServer>,
    mut images: ResMut<Assets<Image>>,
    mut atlases: ResMut<Assets<TextureAtlasLayout>>,
) {
    let world = WorldGrid::from_seed(42);

    // Load tileset synchronously to bake map
    let tileset_bytes = include_bytes!("../../assets/tilesets/miniworld.png");
    let tileset_dyn = image::load_from_memory(tileset_bytes).expect("failed to load tileset");
    let tileset_rgba = tileset_dyn.to_rgba8();
    let (ts_w, ts_h) = tileset_rgba.dimensions();
    let tileset_img = Image::new(
        Extent3d { width: ts_w, height: ts_h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        tileset_rgba.into_raw(),
        TextureFormat::Rgba8UnormSrgb,
        default(),
    );

    // Bake entire map into one texture — single draw call!
    let map_img = bake_map_texture(&world, &tileset_img, 16);
    let map_handle = images.add(map_img);

    // Map sprite — one entity for the entire ground + overlay
    let map_center_x = (WORLD_W as f32 * TILE_PX) / 2.0 - TILE_PX / 2.0;
    let map_center_y = -(WORLD_H as f32 * TILE_PX) / 2.0 + TILE_PX / 2.0;
    commands.spawn((
        Sprite {
            image: map_handle,
            ..default()
        },
        Transform::from_xyz(map_center_x, map_center_y, 0.0),
        Visibility::Hidden,
        MapSprite,
    ));

    // Fog overlay texture
    let mut fog = FogOfWar::new();
    let start_tile = world.map.pois.iter()
        .find(|p| matches!(p.poi_type, questlib::mapgen::PoiType::Town | questlib::mapgen::PoiType::Village))
        .map(|p| (p.x, p.y))
        .unwrap_or((50, 40));
    fog.reveal_around(start_tile.0, start_tile.1);

    let debug = DebugOptions::default();
    let fog_img = create_fog_texture(&fog, &debug);
    let fog_handle = images.add(fog_img);

    commands.spawn((
        Sprite {
            image: fog_handle,
            ..default()
        },
        Transform::from_xyz(map_center_x, map_center_y, 2.0),
        Visibility::Hidden,
        FogSprite,
    ));

    // Player character — hidden until first server poll sets position
    let champion_tex: Handle<Image> = asset_server.load("sprites/Katan.png");
    let champion_layout = TextureAtlasLayout::from_grid(UVec2::new(16, 16), 6, 8, None, None);
    let champion_layout_handle = atlases.add(champion_layout);

    commands.spawn((
        Sprite {
            image: champion_tex,
            texture_atlas: Some(TextureAtlas { layout: champion_layout_handle, index: 0 }),
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, 5.0),
        Visibility::Hidden,
        PlayerSprite,
        WalkAnimation { timer: Timer::from_seconds(0.15, TimerMode::Repeating), frame: 0, direction: Direction::Down, moving: false },
    ));

    commands.spawn((
        Text2d::new("Dac"),
        TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
        TextColor(Color::srgb(0.1, 0.1, 0.1)),
        Transform::from_xyz(0.0, 12.0, 6.0),
        Visibility::Hidden,
        PlayerNameTag,
    ));

    // Loading text — shown until server position arrives
    commands.spawn((
        Text::new("Loading world..."),
        TextFont { font: font.0.clone(), font_size: 16.0, ..default() },
        TextColor(Color::srgb(0.77, 0.64, 0.35)),
        Node { position_type: PositionType::Absolute, top: Val::Percent(45.0), left: Val::Percent(38.0), ..default() },
        LoadingText,
    ));

    // Tile info text
    commands.spawn((
        Text2d::new(""),
        TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
        TextColor(Color::srgb(1.0, 1.0, 1.0)),
        Transform::from_xyz(0.0, 0.0, 10.0),
        TileInfoText,
    ));

    // POI labels
    for poi in &world.map.pois {
        let pos = WorldGrid::tile_to_world(poi.x, poi.y);
        commands.spawn((
            Text2d::new(format!("{:?}", poi.poi_type)),
            TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
            TextColor(Color::srgb(0.1, 0.1, 0.1)),
            Transform::from_xyz(pos.x, pos.y - 12.0, 8.0),
            Visibility::Hidden,
            PoiLabel,
        ));
    }

    commands.insert_resource(fog);
    commands.insert_resource(debug);
    // Route starts empty — sync_from_supabase will set position from server on first poll
    commands.insert_resource(PlannedRoute { waypoints: vec![], meters_walked: 0.0, total_meters: 0.0, current_index: 0, needs_redraw: false });
    commands.insert_resource(CameraPan::default());
    commands.insert_resource(ServerTilePos::default());
    commands.insert_resource(world);
}

// ── Systems ───────────────────────────────────────────

fn handle_map_click(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform)>,
    world: Res<WorldGrid>,
    fog_res: Res<FogOfWar>,
    debug: Res<DebugOptions>,
    supa_config: Res<SupabaseConfig>,
    session: Res<GameSession>,
    mut route: ResMut<PlannedRoute>,
    mut commands: Commands,
    path_markers: Query<Entity, With<PathMarker>>,
    mut info_q: Query<(&mut Text2d, &mut Transform), (With<TileInfoText>, Without<PlayerSprite>)>,
) {
    let Ok(window) = windows.get_single() else { return };
    let Ok((camera, cam_transform)) = camera_q.get_single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    let Ok(world_pos) = camera.viewport_to_world_2d(cam_transform, cursor) else { return };

    let (tx, ty) = WorldGrid::world_to_tile(world_pos);
    let terrain = world.get(tx, ty);

    if let Ok((mut text, mut transform)) = info_q.get_single_mut() {
        let fog = fog_res.as_ref();
        if fog.is_revealed(tx, ty) || debug.fog_disabled {
            let cost_str = if terrain.is_passable() { format!("{}m", terrain.movement_cost()) } else { "impassable".to_string() };
            *text = Text2d::new(format!("{} {}", terrain.name(), cost_str));
        } else {
            *text = Text2d::new("???");
        }
        let tile_pos = WorldGrid::tile_to_world(tx, ty);
        transform.translation = Vec3::new(tile_pos.x, tile_pos.y + 16.0, 10.0);
    }

    if mouse.just_pressed(MouseButton::Left) && terrain.is_passable() {
        let start = if route.waypoints.is_empty() { (50, 40) } else { *route.waypoints.last().unwrap() };
        if start == (tx, ty) { return; }
        if let Some(mut new_segment) = find_path(&world, start, (tx, ty)) {
            if !new_segment.is_empty() && !route.waypoints.is_empty() { new_segment.remove(0); }
            route.waypoints.extend(new_segment);
            route.recalculate_total(&world);
            redraw_path_markers(&mut commands, &path_markers, &route, &fog_res);

            // Write route to Supabase so Game Master can advance the player
            let route_json = questlib::route::encode_route_json(&route.waypoints);
            supabase::write_planned_route(&supa_config, &session.player_id, &route_json);
        }
    }
}

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
            if let Some(last) = pan.last_pos {
                let delta = cursor - last;
                cam.translation.x -= delta.x * proj.scale;
                cam.translation.y += delta.y * proj.scale;
            }
            pan.last_pos = Some(cursor);
            pan.active = true;
        }
    } else {
        pan.last_pos = None;
        pan.active = false;
    }
}

fn handle_clear_route(
    keys: Res<ButtonInput<KeyCode>>,
    mut route: ResMut<PlannedRoute>,
    mut commands: Commands,
    path_markers: Query<Entity, With<PathMarker>>,
    world: Res<WorldGrid>,
) {
    if keys.just_pressed(KeyCode::Escape) {
        let current = route.current_tile().unwrap_or((50, 40));
        route.waypoints = vec![current];
        route.meters_walked = 0.0;
        route.current_index = 0;
        route.recalculate_total(&world);
        for entity in &path_markers { commands.entity(entity).despawn(); }
    }
}

fn redraw_path_markers(commands: &mut Commands, path_markers: &Query<Entity, With<PathMarker>>, route: &PlannedRoute, fog: &FogOfWar) {
    for entity in path_markers { commands.entity(entity).despawn(); }

    let start = route.current_index;
    let len = route.waypoints.len();
    if len <= start { return; }

    // Black dashed line between consecutive waypoints
    let dash_len = 4.0_f32;
    let gap_len = 3.0_f32;
    let line_width = 1.5_f32;

    for i in (start + 1)..len {
        let (x1, y1) = route.waypoints[i - 1];
        let (x2, y2) = route.waypoints[i];
        let p1 = WorldGrid::tile_to_world(x1, y1);
        let p2 = WorldGrid::tile_to_world(x2, y2);

        let dx = p2.x - p1.x;
        let dy = p2.y - p1.y;
        let seg_len = (dx * dx + dy * dy).sqrt();
        if seg_len < 0.1 { continue; }

        let nx = dx / seg_len;
        let ny = dy / seg_len;

        // Draw dashes along this segment
        let mut d = 0.0_f32;
        let mut drawing = true;
        while d < seg_len {
            if drawing {
                let end = (d + dash_len).min(seg_len);
                let cx = p1.x + nx * (d + end) * 0.5;
                let cy = p1.y + ny * (d + end) * 0.5;
                let length = end - d;

                // Determine if horizontal or vertical segment
                let (w, h) = if nx.abs() > ny.abs() {
                    (length, line_width)
                } else {
                    (line_width, length)
                };

                // Pick color based on fog — white on fogged, black on revealed
                let (tile_x, tile_y) = WorldGrid::world_to_tile(Vec2::new(cx, cy));
                let dash_color = if fog.is_revealed(tile_x, tile_y) {
                    Color::srgba(0.0, 0.0, 0.0, 0.7)
                } else {
                    Color::srgba(1.0, 1.0, 1.0, 0.7)
                };

                commands.spawn((
                    Sprite {
                        color: dash_color,
                        custom_size: Some(Vec2::new(w, h)),
                        ..default()
                    },
                    Transform::from_xyz(cx, cy, 3.0),
                    PathMarker,
                ));
                d = end + gap_len;
            } else {
                d += gap_len;
            }
            drawing = !drawing;
        }
    }

    // Flag at destination
    if len > start + 1 {
        let (fx, fy) = route.waypoints[len - 1];
        let pos = WorldGrid::tile_to_world(fx, fy);
        // Pole
        commands.spawn((
            Sprite {
                color: Color::srgb(0.3, 0.2, 0.1),
                custom_size: Some(Vec2::new(1.5, 14.0)),
                ..default()
            },
            Transform::from_xyz(pos.x - 3.0, pos.y + 4.0, 3.5),
            PathMarker,
        ));
        // Pennant
        commands.spawn((
            Sprite {
                color: Color::srgb(0.9, 0.2, 0.1),
                custom_size: Some(Vec2::new(8.0, 6.0)),
                ..default()
            },
            Transform::from_xyz(pos.x + 1.0, pos.y + 9.0, 3.6),
            PathMarker,
        ));
    }
}

/// Sync player character position from Supabase polled data.
/// Tracks the authoritative server tile position.
#[derive(Resource, Default)]
struct ServerTilePos {
    x: usize,
    y: usize,
    initialized: bool,
    camera_snapped: bool, // true after first camera snap to player
}

fn sync_from_supabase(
    polled: Res<PolledPlayerState>,
    session: Res<GameSession>,
    mut route: ResMut<PlannedRoute>,
    mut fog: ResMut<FogOfWar>,
    mut server_pos: ResMut<ServerTilePos>,
    mut player_vis: Query<&mut Visibility, With<PlayerSprite>>,
    mut nametag_vis: Query<&mut Visibility, (With<PlayerNameTag>, Without<PlayerSprite>, Without<LoadingText>, Without<MapSprite>, Without<FogSprite>)>,
    mut map_vis: Query<&mut Visibility, (With<MapSprite>, Without<PlayerSprite>, Without<PlayerNameTag>, Without<FogSprite>, Without<LoadingText>)>,
    mut fog_vis: Query<&mut Visibility, (With<FogSprite>, Without<PlayerSprite>, Without<PlayerNameTag>, Without<MapSprite>, Without<LoadingText>)>,
    mut player_tf: Query<&mut Transform, (With<PlayerSprite>, Without<Camera2d>)>,
    mut camera_tf: Query<&mut Transform, (With<Camera2d>, Without<PlayerSprite>)>,
    mut commands: Commands,
    loading_q: Query<Entity, With<LoadingText>>,
) {
    let Ok(players) = polled.players.lock() else { return };
    if players.is_empty() || session.player_name.is_empty() { return; }

    let Some(me) = players.iter().find(|p| p.name.eq_ignore_ascii_case(&session.player_name)) else { return };

    if let (Some(tx), Some(ty)) = (me.map_tile_x, me.map_tile_y) {
        let tile = (tx as usize, ty as usize);

        // Always update the authoritative server position
        server_pos.x = tile.0;
        server_pos.y = tile.1;

        // First server sync — show player, snap camera, remove loading
        if !server_pos.initialized {
            server_pos.initialized = true;

            // Show everything
            for mut vis in &mut player_vis { *vis = Visibility::Visible; }
            for mut vis in &mut nametag_vis { *vis = Visibility::Visible; }
            for mut vis in &mut map_vis { *vis = Visibility::Visible; }
            for mut vis in &mut fog_vis { *vis = Visibility::Visible; }

            // Snap player to position instantly (no lerp)
            let world_pos = WorldGrid::tile_to_world(tile.0, tile.1);
            for mut tf in &mut player_tf {
                tf.translation.x = world_pos.x;
                tf.translation.y = world_pos.y;
            }

            // Snap camera to player instantly
            for mut cam in &mut camera_tf {
                cam.translation.x = world_pos.x;
                cam.translation.y = world_pos.y;
            }
            server_pos.camera_snapped = true;

            // Remove loading text
            for entity in &loading_q {
                commands.entity(entity).despawn_recursive();
            }
        }

        // Restore route from server if browser has no route
        if route.waypoints.is_empty() {
            if let Some(ref route_json) = me.planned_route {
                if !route_json.is_empty() {
                    if let Some(server_route) = questlib::route::parse_route_json(route_json) {
                        if !server_route.is_empty() {
                            route.waypoints = server_route;
                            route.meters_walked = me.route_meters_walked.unwrap_or(0.0) as f32;
                            // Find current index from server position
                            if let Some(idx) = route.waypoints.iter().position(|&w| w == tile) {
                                route.current_index = idx;
                            }
                            route.needs_redraw = true;
                        }
                    }
                }
            }

            // If still no route, just place character at server position
            if route.waypoints.is_empty() && route.current_tile() != Some(tile) {
                route.waypoints = vec![tile];
                route.current_index = 0;
            }
        } else {
            // Already have a route — advance index to match server position
            if let Some(idx) = route.waypoints.iter().position(|&w| w == tile) {
                if idx != route.current_index {
                    route.current_index = idx;
                    route.needs_redraw = true;
                }
            }
        }

        // Update fog from server's revealed_tiles
        if let Some(ref encoded) = me.revealed_tiles {
            if !encoded.is_empty() {
                if let Some(server_fog) = questlib::fog::FogBitfield::from_base64(encoded) {
                    // Merge server fog into local fog
                    for y in 0..super::world::WORLD_H {
                        for x in 0..super::world::WORLD_W {
                            if server_fog.is_revealed(x, y) && !fog.is_revealed(x, y) {
                                fog.revealed[y * super::world::WORLD_W + x] = true;
                                fog.dirty = true;
                            }
                        }
                    }
                }
            }
        }
    }
}

fn check_path_redraw(
    mut route: ResMut<PlannedRoute>,
    fog: Res<FogOfWar>,
    mut commands: Commands,
    path_markers: Query<Entity, With<PathMarker>>,
) {
    if !route.needs_redraw { return; }
    route.needs_redraw = false;
    redraw_path_markers(&mut commands, &path_markers, &route, &fog);
}

fn toggle_poi_labels(keys: Res<ButtonInput<KeyCode>>, mut labels: Query<&mut Visibility, With<PoiLabel>>, debug: Res<DebugOptions>) {
    let show = keys.pressed(KeyCode::Tab) || debug.show_pois;
    for mut vis in &mut labels { *vis = if show { Visibility::Visible } else { Visibility::Hidden }; }
}

/// Only update fog texture when it changes.
fn update_fog_texture(
    mut fog: ResMut<FogOfWar>,
    route: Res<PlannedRoute>,
    debug: Res<DebugOptions>,
    fog_sprite_q: Query<&Sprite, With<FogSprite>>,
    mut images: ResMut<Assets<Image>>,
) {
    if let Some((px, py)) = route.current_tile() {
        fog.reveal_around(px, py);
    }

    if !fog.dirty { return; }
    fog.dirty = false;

    // Get the fog sprite's image handle and update it
    let Ok(sprite) = fog_sprite_q.get_single() else { return };
    let handle = &sprite.image;
    let Some(image) = images.get_mut(handle.id()) else { return };

    // Update fog pixels
    let w = WORLD_W * 16;
    for ty in 0..WORLD_H {
        for tx in 0..WORLD_W {
            let revealed = debug.fog_disabled || fog.is_revealed(tx, ty);
            let (r, g, b, a) = if revealed { (0, 0, 0, 0) } else { (15, 15, 25, 255) };
            for py in 0..16 {
                for px in 0..16 {
                    let idx = ((ty * 16 + py) * w + (tx * 16 + px)) * 4;
                    image.data[idx] = r;
                    image.data[idx + 1] = g;
                    image.data[idx + 2] = b;
                    image.data[idx + 3] = a;
                }
            }
        }
    }
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
    let Ok(mut projection) = camera_q.get_single_mut() else { return };
    for ev in scroll_evr.read() {
        if ev.y > 0.0 { zoom.target = (zoom.target * 0.75).max(0.15); }
        else if ev.y < 0.0 { zoom.target = (zoom.target * 1.5).min(3.0); }
    }
    let diff = zoom.target - projection.scale;
    let dt = time.delta_secs();
    projection.scale += diff * (1.0 - (-6.0 * dt).exp());
}

fn update_path_visuals(
    route: Res<PlannedRoute>,
    polled: Res<PolledPlayerState>,
    session: Res<GameSession>,
    time: Res<Time>,
    mut player_q: Query<(&mut Transform, &mut WalkAnimation, &mut Sprite), With<PlayerSprite>>,
    mut nametag_q: Query<&mut Transform, (With<PlayerNameTag>, Without<PlayerSprite>)>,
) {
    // Check if treadmill belt is running
    let belt_moving = {
        let Ok(players) = polled.players.lock() else { false; return };
        players.iter()
            .find(|p| p.name.eq_ignore_ascii_case(&session.player_name))
            .map(|p| p.is_walking)
            .unwrap_or(false)
    };

    if let Some((x, y)) = route.current_tile() {
        let target = WorldGrid::tile_to_world(x, y);
        for (mut transform, mut anim, mut sprite) in &mut player_q {
            let current = transform.translation;
            let dx = target.x - current.x;
            let dy = target.y - current.y;
            let dist = dx.abs() + dy.abs();

            // Move toward target tile
            if dist > 1.0 {
                transform.translation.x += dx * 0.1;
                transform.translation.y += dy * 0.1;
                // Update direction from movement
                if dx.abs() > dy.abs() { anim.direction = if dx > 0.0 { Direction::Right } else { Direction::Left }; }
                else { anim.direction = if dy > 0.0 { Direction::Up } else { Direction::Down }; }
            } else {
                transform.translation.x = target.x;
                transform.translation.y = target.y;
            }

            // Animate if belt is running OR physically moving between tiles
            let should_animate = belt_moving || dist > 1.0;

            if should_animate {
                anim.timer.tick(time.delta());
                if anim.timer.just_finished() { anim.frame = (anim.frame + 1) % 5; }
                let row = anim.direction.base_row() + 1; // walk row
                if let Some(ref mut atlas) = sprite.texture_atlas { atlas.index = row * 6 + anim.frame; }
                anim.moving = true;
            } else if anim.moving {
                // Just stopped — reset to idle
                anim.moving = false;
                anim.frame = 0;
                let row = anim.direction.base_row(); // idle row
                if let Some(ref mut atlas) = sprite.texture_atlas { atlas.index = row * 6; }
            }
        }
        if let Ok((player_tf, _, _)) = player_q.get_single() {
            for mut tf in &mut nametag_q { tf.translation.x = player_tf.translation.x; tf.translation.y = player_tf.translation.y + 12.0; }
        }
    }
}

fn update_camera(
    player_q: Query<&Transform, With<PlayerSprite>>,
    mut camera_q: Query<(&mut Transform, &mut OrthographicProjection), (With<Camera2d>, Without<PlayerSprite>)>,
    pan: Res<CameraPan>,
    mut initialized: Local<bool>,
) {
    let Some(player_transform) = player_q.iter().next() else { return };
    let Ok((mut cam, mut proj)) = camera_q.get_single_mut() else { return };
    if !*initialized { proj.scale = 0.4; *initialized = true; }
    if !pan.active {
        let target = player_transform.translation;
        cam.translation.x += (target.x - cam.translation.x) * 0.05;
        cam.translation.y += (target.y - cam.translation.y) * 0.05;
    }
    let pixel_scale = 1.0 / proj.scale;
    cam.translation.x = (cam.translation.x * pixel_scale).round() / pixel_scale;
    cam.translation.y = (cam.translation.y * pixel_scale).round() / pixel_scale;
}

fn handle_debug_menu(
    keys: Res<ButtonInput<KeyCode>>,
    mut debug: ResMut<DebugOptions>,
    mut fog: ResMut<FogOfWar>,
    mut commands: Commands,
    font: Res<GameFont>,
    time: Res<Time>,
    existing_menu: Query<Entity, With<DebugMenuUi>>,
    mut poi_labels: Query<&mut Visibility, With<PoiLabel>>,
) {
    if keys.just_pressed(KeyCode::F3) { debug.show_menu = !debug.show_menu; }
    if !debug.show_menu {
        for entity in &existing_menu { commands.entity(entity).despawn_recursive(); }
        return;
    }
    if keys.just_pressed(KeyCode::Digit1) { debug.fog_disabled = !debug.fog_disabled; fog.dirty = true; }
    if keys.just_pressed(KeyCode::Digit2) { debug.show_pois = !debug.show_pois; }
    for mut vis in &mut poi_labels { *vis = if debug.show_pois { Visibility::Visible } else { Visibility::Hidden }; }
    for entity in &existing_menu { commands.entity(entity).despawn_recursive(); }

    let fps = (1.0 / time.delta_secs()).round() as u32;
    let menu_text = format!(
        "=== DEBUG (F3) ===\nFPS: {}\n1: Fog of War  [{}]\n2: Show POIs    [{}]",
        fps, if debug.fog_disabled { "OFF" } else { "ON" }, if debug.show_pois { "ON" } else { "OFF" },
    );
    commands.spawn((
        Text::new(menu_text),
        TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
        TextColor(Color::srgb(1.0, 1.0, 0.0)),
        Node { position_type: PositionType::Absolute, top: Val::Px(10.0), left: Val::Px(10.0), ..default() },
        DebugMenuUi,
    ));
}
