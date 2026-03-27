use bevy::prelude::*;

use super::world::{WorldGrid, WORLD_W, WORLD_H, TILE_PX};
use super::path::{PlannedRoute, find_path};
use crate::states::AppState;
use crate::GameFont;

pub struct TilemapPlugin;

impl Plugin for TilemapPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::InGame), spawn_world)
            .add_systems(
                Update,
                (handle_map_click, handle_zoom, handle_right_click, toggle_poi_labels, update_path_visuals, update_camera)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Component)]
struct WorldTile;

#[derive(Component)]
struct PathMarker;

#[derive(Component)]
struct PlayerSprite;

#[derive(Component)]
struct TileInfoText;

#[derive(Component)]
struct PoiLabel;

fn spawn_world(
    mut commands: Commands,
    font: Res<GameFont>,
    asset_server: Res<AssetServer>,
    mut atlases: ResMut<Assets<TextureAtlasLayout>>,
) {
    let world = WorldGrid::from_seed(42); // TODO: get seed from game session

    // MiniWorld atlas: 16 cols, 7 rows, 16x16 tiles with 2px padding (20px slots)
    let tileset: Handle<Image> = asset_server.load("tilesets/miniworld.png");
    let layout = TextureAtlasLayout::from_grid(
        UVec2::new(16, 16),  // tile size
        16, 7,                // columns, rows
        Some(UVec2::new(4, 4)),  // padding between tiles (2px on each side = 4px gap)
        Some(UVec2::new(2, 2)),  // offset to first tile (2px padding)
    );
    let layout_handle = atlases.add(layout);

    // Layer 0: Ground
    for y in 0..WORLD_H {
        for x in 0..WORLD_W {
            let ground = world.get_ground(x, y);
            let pos = WorldGrid::tile_to_world(x, y);

            commands.spawn((
                Sprite {
                    image: tileset.clone(),
                    texture_atlas: Some(TextureAtlas {
                        layout: layout_handle.clone(),
                        index: ground.tile_index_varied(x, y),
                    }),
                    custom_size: Some(Vec2::splat(TILE_PX)),
                    ..default()
                },
                Transform::from_xyz(pos.x, pos.y, 0.0),
                WorldTile,
            ));
        }
    }

    // Layer 1: Overlays (trees, rocks, buildings)
    for y in 0..WORLD_H {
        for x in 0..WORLD_W {
            if let Some(overlay) = world.cells[y][x].overlay {
                let pos = WorldGrid::tile_to_world(x, y);

                commands.spawn((
                    Sprite {
                        image: tileset.clone(),
                        texture_atlas: Some(TextureAtlas {
                            layout: layout_handle.clone(),
                            index: overlay.tile_index_varied(x, y),
                        }),
                        ..default()
                    },
                    Transform::from_xyz(pos.x, pos.y, 1.0),
                    WorldTile,
                ));
            }
        }
    }

    // Player sprite
    // Start at the first town/village
    let start_tile = world.map.pois.iter()
        .find(|p| matches!(p.poi_type, questlib::mapgen::PoiType::Town | questlib::mapgen::PoiType::Village))
        .map(|p| (p.x, p.y))
        .unwrap_or((50, 40));
    let start_pos = WorldGrid::tile_to_world(start_tile.0, start_tile.1);
    commands.spawn((
        Sprite {
            color: Color::srgb(0.77, 0.64, 0.35),
            custom_size: Some(Vec2::new(12.0, 12.0)),
            ..default()
        },
        Transform::from_xyz(start_pos.x, start_pos.y, 5.0),
        PlayerSprite,
    ));

    commands.spawn((
        Text2d::new("Dac"),
        TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
        TextColor(Color::srgb(0.77, 0.64, 0.35)),
        Transform::from_xyz(start_pos.x, start_pos.y + 12.0, 6.0),
        PlayerSprite,
    ));

    // Tile info text
    commands.spawn((
        Text2d::new(""),
        TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
        TextColor(Color::srgb(1.0, 1.0, 1.0)),
        Transform::from_xyz(0.0, 0.0, 10.0),
        TileInfoText,
    ));

    // POI labels on the map — hidden by default, shown on TAB
    for poi in &world.map.pois {
        let pos = WorldGrid::tile_to_world(poi.x, poi.y);
        let label = format!("{:?}", poi.poi_type);
        commands.spawn((
            Text2d::new(label),
            TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
            TextColor(Color::srgb(0.1, 0.1, 0.1)),
            Transform::from_xyz(pos.x, pos.y - 12.0, 8.0),
            Visibility::Hidden,
            PoiLabel,
            WorldTile,
        ));
    }

    commands.insert_resource(PlannedRoute {
        waypoints: vec![start_tile],
        meters_walked: 0.0,
        total_meters: 0.0,
        current_index: 0,
    });

    commands.insert_resource(world);
}

fn handle_map_click(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform)>,
    world: Res<WorldGrid>,
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
        let cost_str = if terrain.is_passable() {
            format!("{}m", terrain.movement_cost())
        } else {
            "impassable".to_string()
        };
        *text = Text2d::new(format!("{} {}", terrain.name(), cost_str));
        let tile_pos = WorldGrid::tile_to_world(tx, ty);
        transform.translation = Vec3::new(tile_pos.x, tile_pos.y + 16.0, 10.0);
    }

    if mouse.just_pressed(MouseButton::Left) && terrain.is_passable() {
        let start = if route.waypoints.is_empty() {
            (50, 40)
        } else {
            *route.waypoints.last().unwrap()
        };

        if start == (tx, ty) { return; }

        if let Some(mut new_segment) = find_path(&world, start, (tx, ty)) {
            if !new_segment.is_empty() && !route.waypoints.is_empty() {
                new_segment.remove(0);
            }
            route.waypoints.extend(new_segment);
            route.recalculate_total(&world);
            redraw_path_markers(&mut commands, &path_markers, &route);
        }
    }
}

fn handle_right_click(
    mouse: Res<ButtonInput<MouseButton>>,
    mut route: ResMut<PlannedRoute>,
    mut commands: Commands,
    path_markers: Query<Entity, With<PathMarker>>,
    world: Res<WorldGrid>,
) {
    if mouse.just_pressed(MouseButton::Right) {
        let current = route.current_tile().unwrap_or((50, 40));
        route.waypoints = vec![current];
        route.meters_walked = 0.0;
        route.current_index = 0;
        route.recalculate_total(&world);
        for entity in &path_markers {
            commands.entity(entity).despawn();
        }
    }
}

fn redraw_path_markers(
    commands: &mut Commands,
    path_markers: &Query<Entity, With<PathMarker>>,
    route: &PlannedRoute,
) {
    for entity in path_markers { commands.entity(entity).despawn(); }

    for (i, &(px, py)) in route.waypoints.iter().enumerate() {
        if i <= route.current_index { continue; }
        let pos = WorldGrid::tile_to_world(px, py);
        commands.spawn((
            Sprite {
                color: Color::srgba(1.0, 0.8, 0.2, 0.4),
                custom_size: Some(Vec2::new(TILE_PX, TILE_PX)),
                ..default()
            },
            Transform::from_xyz(pos.x, pos.y, 3.0),
            PathMarker,
        ));
    }
}

fn toggle_poi_labels(
    keys: Res<ButtonInput<KeyCode>>,
    mut labels: Query<&mut Visibility, With<PoiLabel>>,
) {
    let show = keys.pressed(KeyCode::Tab);
    for mut vis in &mut labels {
        *vis = if show { Visibility::Visible } else { Visibility::Hidden };
    }
}

fn handle_zoom(
    mut scroll_evr: EventReader<bevy::input::mouse::MouseWheel>,
    mut camera_q: Query<&mut OrthographicProjection, With<Camera2d>>,
) {
    let Ok(mut projection) = camera_q.get_single_mut() else { return };
    for ev in scroll_evr.read() {
        let zoom_delta = -ev.y * 0.1;
        projection.scale = (projection.scale + zoom_delta).clamp(0.2, 3.0);
    }
}

fn update_path_visuals(
    route: Res<PlannedRoute>,
    mut player_q: Query<&mut Transform, With<PlayerSprite>>,
) {
    if let Some((x, y)) = route.current_tile() {
        let target = WorldGrid::tile_to_world(x, y);
        for mut transform in &mut player_q {
            let current = transform.translation;
            transform.translation.x += (target.x - current.x) * 0.1;
            transform.translation.y += (target.y - current.y) * 0.1;
        }
    }
}

fn update_camera(
    player_q: Query<&Transform, With<PlayerSprite>>,
    mut camera_q: Query<(&mut Transform, &mut OrthographicProjection), (With<Camera2d>, Without<PlayerSprite>)>,
    mut initialized: Local<bool>,
) {
    let Some(player_transform) = player_q.iter().next() else { return };
    let Ok((mut cam, mut proj)) = camera_q.get_single_mut() else { return };

    if !*initialized {
        proj.scale = 0.4; // 2.5x zoom
        *initialized = true;
    }

    let target = player_transform.translation;
    cam.translation.x += (target.x - cam.translation.x) * 0.05;
    cam.translation.y += (target.y - cam.translation.y) * 0.05;

    // Snap camera to pixel grid to avoid sub-pixel gaps between tiles
    let pixel_scale = 1.0 / proj.scale;
    cam.translation.x = (cam.translation.x * pixel_scale).round() / pixel_scale;
    cam.translation.y = (cam.translation.y * pixel_scale).round() / pixel_scale;
}
