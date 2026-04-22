//! Ambient effects — currently just slow-drifting clouds over the
//! overworld. Runs once the player is InGame; does nothing in interiors
//! (you're underground, no sky).
//!
//! The cloud texture is generated at startup as a single 64×32 RGBA
//! image: a radial alpha gradient feathering to transparent at the
//! edges. All cloud instances share this one image — no asset files,
//! variety comes from random scale / alpha / drift-speed per instance.
//!
//! Z-layering:
//!   4 = player sprite (tilemap.rs)
//!   8 = POI labels
//!  20 = clouds
//! Anything below the UI layer (which is its own render pass) but above
//! everything else in world-space, so clouds feel like sky.

use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::states::AppState;
use crate::terrain::tilemap::MyPlayerState;
use crate::terrain::world::{WorldGrid, TILE_PX, WORLD_W, WORLD_H};

/// Cheap uniform random in [0, 1) via the browser's Math.random(). Works
/// on WASM without pulling in `rand` + its WASM-specific configuration.
fn rand01() -> f32 { js_sys::Math::random() as f32 }

fn rand_range(lo: f32, hi: f32) -> f32 { lo + rand01() * (hi - lo) }

const CLOUD_COUNT: usize = 10;
/// Soft-blob texture dimensions (power-of-two friendly).
const CLOUD_TEX_W: u32 = 64;
const CLOUD_TEX_H: u32 = 32;

/// World rectangle in pixels, from the world grid dimensions.
fn world_px_w() -> f32 { WORLD_W as f32 * TILE_PX }
fn world_px_h() -> f32 { WORLD_H as f32 * TILE_PX }

pub struct AmbientPlugin;

impl Plugin for AmbientPlugin {
    fn build(&self, app: &mut App) {
        app
            .add_systems(OnEnter(AppState::InGame), spawn_clouds)
            .add_systems(
                Update,
                (drift_clouds, hide_in_interiors)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Component)]
struct Cloud {
    velocity: Vec2, // px per second
}

#[derive(Component)]
struct CloudRoot; // tag on every cloud for easy show/hide

/// Generate a 64×32 cloud-blob texture: radial alpha gradient feathered
/// to zero at the edges; color is warm near-white. One image, shared
/// across all cloud instances.
fn generate_cloud_image() -> Image {
    let w = CLOUD_TEX_W;
    let h = CLOUD_TEX_H;
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    // An elliptical falloff: clouds are wider than tall, so shape the
    // gradient to match the 2:1 aspect. Soft edges look more cloud-like
    // than a crisp circle.
    let rx = cx * 0.92;
    let ry = cy * 0.85;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            let dx = (x as f32 - cx) / rx;
            let dy = (y as f32 - cy) / ry;
            let d2 = dx * dx + dy * dy;
            // Smoothstep: solid in the middle, feathering near the edge.
            let t = (1.0 - d2).clamp(0.0, 1.0);
            let alpha = (t * t * (3.0 - 2.0 * t) * 255.0) as u8;
            // Warm near-white so clouds don't look frozen-blue
            data.push(250);
            data.push(248);
            data.push(240);
            data.push(alpha);
        }
    }
    Image::new(
        Extent3d { width: w, height: h, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        bevy::render::render_asset::RenderAssetUsages::all(),
    )
}

fn spawn_clouds(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
) {
    let tex = images.add(generate_cloud_image());
    for _ in 0..CLOUD_COUNT {
        let x = rand_range(0.0, world_px_w());
        let y = rand_range(0.0, world_px_h());
        let scale = rand_range(0.7, 1.5);
        let alpha = rand_range(0.25, 0.55);
        let vx = rand_range(8.0, 15.0);
        let vy = rand_range(-1.5, 1.5); // tiny vertical drift for life
        commands.spawn((
            Sprite {
                image: tex.clone(),
                color: Color::srgba(1.0, 1.0, 1.0, alpha),
                ..default()
            },
            Transform::from_xyz(x, y, 20.0).with_scale(Vec3::splat(scale)),
            Cloud { velocity: Vec2::new(vx, vy) },
            CloudRoot,
        ));
    }
}

/// Drift clouds by velocity, wrapping horizontally at the world edge so
/// they form a continuous conveyor. Vertical drift is small enough that
/// a cloud won't leave the play area in a typical session, but clamp
/// to keep them from creeping off over long idles.
fn drift_clouds(
    time: Res<Time>,
    mut q: Query<(&Cloud, &mut Transform)>,
) {
    let dt = time.delta_secs();
    let ww = world_px_w();
    let wh = world_px_h();
    // A bit of horizontal padding — cloud texture is CLOUD_TEX_W wide,
    // scaled — so the wrap happens cleanly off-screen.
    let pad_x = (CLOUD_TEX_W as f32) * 1.5;
    for (cloud, mut tf) in &mut q {
        tf.translation.x += cloud.velocity.x * dt;
        tf.translation.y += cloud.velocity.y * dt;
        if tf.translation.x > ww + pad_x {
            // Wrap to the left edge, randomize vertical so it doesn't
            // look like the same cloud coming back.
            tf.translation.x = -pad_x;
            tf.translation.y = rand_range(0.0, wh);
        }
        // Clamp vertical so over very long sessions clouds don't escape.
        if tf.translation.y > wh { tf.translation.y = wh; }
        if tf.translation.y < 0.0 { tf.translation.y = 0.0; }
    }
}

/// Hide clouds when the player is in an interior — caves/castles have
/// no sky. Toggling visibility is cheaper than despawning + respawning
/// and keeps the drift state intact so clouds resume where they left off.
fn hide_in_interiors(
    state: Res<MyPlayerState>,
    world: Option<Res<WorldGrid>>,
    mut q: Query<&mut Visibility, With<CloudRoot>>,
) {
    // MyPlayerState.location is None on the overworld and Some(id) while
    // inside an interior. WorldGrid presence is a secondary safety check
    // (clouds shouldn't render before the world is loaded).
    let show = world.is_some() && state.location.is_none();
    for mut vis in &mut q {
        *vis = if show { Visibility::Visible } else { Visibility::Hidden };
    }
}
