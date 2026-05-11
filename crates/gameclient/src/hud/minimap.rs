//! Minimap — a small world overview in the bottom-right corner.
//!
//! Generates a 100×80 RGBA texture (one pixel per tile) colored by
//! ground type, with fogged tiles darkened. The texture is displayed
//! scaled up 2× via Bevy UI Node sizing (nearest-neighbor = crisp).
//! Player dots (you + other co-located players) are spawned as tiny
//! positioned UI Nodes above the image layer and updated each frame.
//!
//! Regeneration:
//!   - Once on first InGame enter when the world is ready.
//!   - Again whenever FogOfWar.dirty is set during play.

use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::states::AppState;
use crate::supabase::PolledPlayerState;
use crate::terrain::tilemap::{FogOfWar, MyPlayerState};
use crate::terrain::world::{WorldGrid, WORLD_W, WORLD_H};
use crate::terrain::{Ground};
use crate::GameSession;

/// Rendered size on screen. 2× tile scale keeps the whole 100×80 world
/// compact (~200×160 px) without UI hogging the corner.
const MINIMAP_WIDTH_PX: f32 = WORLD_W as f32 * 2.0;   // 200
const MINIMAP_HEIGHT_PX: f32 = WORLD_H as f32 * 2.0;  // 160

pub struct MinimapPlugin;

impl Plugin for MinimapPlugin {
    fn build(&self, app: &mut App) {
        app
            .add_systems(OnEnter(AppState::InGame), spawn_minimap)
            .add_systems(
                Update,
                (regenerate_if_dirty, update_player_dots)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

// ── Components ─────────────────────────────────────

#[derive(Component)]
struct MinimapRoot;

#[derive(Component)]
struct MinimapImage {
    handle: Handle<Image>,
    /// Set true by regenerate_if_dirty when the image was actually redrawn —
    /// kept for future-facing use; harmless to ignore.
    #[allow(dead_code)]
    regenerated_this_frame: bool,
}

#[derive(Component)]
struct LocalPlayerDot;

#[derive(Component)]
struct OtherPlayerDot(String); // player_id

// ── Spawn ──────────────────────────────────────────

fn spawn_minimap(mut commands: Commands, mut images: ResMut<Assets<Image>>) {
    // Start with an all-black texture. regenerate_if_dirty fills it once
    // WorldGrid + FogOfWar are available (they come from spawn_world).
    let img = blank_minimap_image();
    let handle = images.add(img);

    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            right: Val::Px(12.0),
            bottom: Val::Px(12.0),
            width: Val::Px(MINIMAP_WIDTH_PX + 4.0),   // +border
            height: Val::Px(MINIMAP_HEIGHT_PX + 4.0),
            padding: UiRect::all(Val::Px(2.0)),
            border: UiRect::all(Val::Px(2.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.02, 0.08, 0.9)),
        BorderColor(Color::srgb(0.4, 0.35, 0.2)),
        BorderRadius::all(Val::Px(4.0)),
        MinimapRoot,
    )).with_children(|parent| {
        parent.spawn((
            Node {
                width: Val::Px(MINIMAP_WIDTH_PX),
                height: Val::Px(MINIMAP_HEIGHT_PX),
                ..default()
            },
            ImageNode::new(handle.clone()),
            MinimapImage { handle: handle.clone(), regenerated_this_frame: false },
        ));
    });
}

// ── Image generation ───────────────────────────────

/// Empty/black 100×80 image used until the world is ready.
fn blank_minimap_image() -> Image {
    let data = vec![0u8; WORLD_W * WORLD_H * 4];
    Image::new(
        Extent3d { width: WORLD_W as u32, height: WORLD_H as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        bevy::render::render_asset::RenderAssetUsages::all(),
    )
}

/// Regenerate the minimap image whenever fog is dirty, or on first frame
/// once WorldGrid has been inserted. Fog dirtiness already tracked for the
/// main-world texture — we piggyback on the same signal.
fn regenerate_if_dirty(
    world: Option<Res<WorldGrid>>,
    fog: Option<Res<FogOfWar>>,
    mut images: ResMut<Assets<Image>>,
    mut q: Query<&mut MinimapImage>,
    mut did_initial: Local<bool>,
) {
    let Some(world) = world else { return; };
    let Some(fog) = fog else { return; };
    // Only redraw when fog changed or we've never drawn with a real world.
    if !fog.dirty && *did_initial { return; }
    *did_initial = true;

    let Ok(mut minimap) = q.get_single_mut() else { return; };
    let Some(img) = images.get_mut(&minimap.handle) else { return; };

    let bytes = &mut img.data;
    for y in 0..WORLD_H {
        for x in 0..WORLD_W {
            let terrain = world.get(x, y);
            let (mut r, mut g, mut b) = ground_color(terrain.ground);
            let revealed = fog.revealed.get(y * WORLD_W + x).copied().unwrap_or(false);
            if !revealed {
                // Pure-dark fog cell. Earlier we kept a 15 % silhouette of
                // the ground color so the map shape was hinted; that
                // leaked terrain (roads / water shapes) the player
                // hadn't actually discovered yet. Make unrevealed
                // tiles a uniform near-black so the minimap obeys the
                // same fog-of-war rules as the overworld view.
                r = 12;
                g = 12;
                b = 20;
            }
            let idx = (y * WORLD_W + x) * 4;
            bytes[idx]     = r;
            bytes[idx + 1] = g;
            bytes[idx + 2] = b;
            bytes[idx + 3] = 255;
        }
    }
    minimap.regenerated_this_frame = true;
}

/// Per-ground RGB triple for the minimap. Roughly matches the in-world
/// palette so the minimap "feels like" the world — swamp is dark brown,
/// mountain is grey, etc. Chosen by eye, not a true atlas average.
fn ground_color(g: Ground) -> (u8, u8, u8) {
    match g {
        Ground::Road   => (200, 175, 120), // beige
        Ground::Grass  => (96, 150, 80),   // green
        Ground::Sand   => (220, 200, 130), // yellow-sand
        Ground::Snow   => (230, 235, 240), // near-white
        Ground::Stone  => (135, 130, 120), // warm grey
        Ground::Swamp  => (70, 60, 40),    // dark brown
        Ground::Water  => (50, 90, 160),   // blue
    }
}

// ── Player dots ────────────────────────────────────

fn update_player_dots(
    mut commands: Commands,
    session: Res<GameSession>,
    my: Res<MyPlayerState>,
    polled: Res<PolledPlayerState>,
    root_q: Query<Entity, With<MinimapRoot>>,
    mut local_q: Query<(Entity, &mut Node), (With<LocalPlayerDot>, Without<OtherPlayerDot>)>,
    mut others_q: Query<(Entity, &OtherPlayerDot, &mut Node), Without<LocalPlayerDot>>,
) {
    let Ok(root) = root_q.get_single() else { return; };

    // ── Self dot (bright gold, slightly larger)
    let my_x = minimap_px_x(my.tile_x as f32);
    let my_y = minimap_px_y(my.tile_y as f32);
    if let Ok((_, mut node)) = local_q.get_single_mut() {
        node.left = Val::Px(my_x - 2.0);
        node.top = Val::Px(my_y - 2.0);
    } else if my.initialized {
        commands.entity(root).with_children(|p| {
            p.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    left: Val::Px(my_x - 2.0),
                    top: Val::Px(my_y - 2.0),
                    width: Val::Px(4.0),
                    height: Val::Px(4.0),
                    ..default()
                },
                BackgroundColor(Color::srgb(1.0, 0.9, 0.3)),
                BorderRadius::all(Val::Px(2.0)),
                LocalPlayerDot,
            ));
        });
    }

    // ── Other players' dots (dim blue). Filter to co-located only — a
    // player inside a cave shouldn't flicker on the overworld minimap.
    let Ok(lock) = polled.players.lock() else { return; };
    let my_loc = lock.iter().find(|p| p.id == session.player_id).and_then(|p| p.location.clone());
    let others: Vec<_> = lock.iter()
        .filter(|p| p.id != session.player_id && p.location == my_loc)
        .collect();
    let visible_ids: std::collections::HashSet<&str> =
        others.iter().map(|p| p.id.as_str()).collect();

    // Despawn dots whose player is no longer co-located.
    for (e, tag, _) in &others_q {
        if !visible_ids.contains(tag.0.as_str()) {
            commands.entity(e).despawn_recursive();
        }
    }
    // Update or spawn.
    for other in &others {
        let tx = other.map_tile_x.unwrap_or(0) as f32;
        let ty = other.map_tile_y.unwrap_or(0) as f32;
        let px = minimap_px_x(tx);
        let py = minimap_px_y(ty);
        let existing = others_q.iter_mut().find(|(_, tag, _)| tag.0 == other.id);
        if let Some((_, _, mut node)) = existing {
            node.left = Val::Px(px - 1.5);
            node.top = Val::Px(py - 1.5);
        } else {
            let id = other.id.clone();
            commands.entity(root).with_children(|p| {
                p.spawn((
                    Node {
                        position_type: PositionType::Absolute,
                        left: Val::Px(px - 1.5),
                        top: Val::Px(py - 1.5),
                        width: Val::Px(3.0),
                        height: Val::Px(3.0),
                        ..default()
                    },
                    BackgroundColor(Color::srgb(0.5, 0.8, 1.0)),
                    BorderRadius::all(Val::Px(1.5)),
                    OtherPlayerDot(id),
                ));
            });
        }
    }
}

/// Map tile x to pixel x inside the minimap's image area (accounting for
/// the 2-px padding on MinimapRoot).
fn minimap_px_x(tile_x: f32) -> f32 {
    2.0 + tile_x * (MINIMAP_WIDTH_PX / WORLD_W as f32)
}

fn minimap_px_y(tile_y: f32) -> f32 {
    2.0 + tile_y * (MINIMAP_HEIGHT_PX / WORLD_H as f32)
}
