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
use bevy::reflect::TypePath;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, ShaderRef, TextureDimension, TextureFormat,
};
use bevy::sprite::{AlphaMode2d, Material2d, Material2dPlugin};

use crate::states::AppState;
use crate::terrain::tilemap::{FogOfWar, MyPlayerState};
use crate::terrain::world::{WorldGrid, TILE_PX, world_w, world_h};

/// Cheap uniform random in [0, 1) via the browser's Math.random(). Works
/// on WASM without pulling in `rand` + its WASM-specific configuration.
fn rand01() -> f32 { js_sys::Math::random() as f32 }

fn rand_range(lo: f32, hi: f32) -> f32 { lo + rand01() * (hi - lo) }

const CLOUD_COUNT: usize = 18;
/// Cloud texture dimensions. Bigger canvas + larger on-screen scale gives
/// the fBm detail room to read instead of smearing into a blob.
const CLOUD_TEX_W: u32 = 192;
const CLOUD_TEX_H: u32 = 96;
/// Number of distinct textures generated at startup. Each cloud picks one
/// at random, so the sky isn't made of 18 copies of the same shape.
const CLOUD_VARIANTS: u32 = 3;

/// Fraction of clouds that are "rainy" — darker tint, emits rain drops.
/// With 18 clouds total, 5 % means ~1 rainy cloud on the map at a time.
const RAINY_FRACTION: f32 = 0.05;
/// Rain drops emitted per rainy cloud per second.
const DROPS_PER_CLOUD_PER_SEC: f32 = 6.0;
/// How far below the cloud a drop starts, and how far it falls before
/// despawning. Both in world-space pixels. Shorter fall = tighter,
/// drizzlier look — drops don't streak all the way down the screen.
const DROP_START_Y_BELOW_CLOUD: f32 = 12.0;
const DROP_FALL_DISTANCE: f32 = 80.0;
const DROP_SPEED: f32 = 240.0; // px/s (straight down)
/// Per-drop alpha range. Varying each drop breaks up the otherwise
/// uniform strip of color and makes the rain read as scattered
/// droplets instead of a wall.
const DROP_ALPHA_MIN: f32 = 0.35;
const DROP_ALPHA_MAX: f32 = 0.80;

/// World rectangle in world-space pixels. tile_to_world maps tile y to
/// `-y * TILE_PX`, so the world's Y range is [-WORLD_PX_H, 0] and X range
/// is [0, WORLD_PX_W]. Cloud positions use these bounds directly.
fn world_px_w() -> f32 { world_w() as f32 * TILE_PX }
fn world_px_h() -> f32 { world_h() as f32 * TILE_PX }

pub struct AmbientPlugin;

impl Plugin for AmbientPlugin {
    fn build(&self, app: &mut App) {
        app
            .add_plugins(Material2dPlugin::<CloudMaterial>::default())
            .add_systems(OnEnter(AppState::InGame), spawn_clouds)
            .add_systems(
                Update,
                (drift_clouds, update_cloud_shadows, update_cloud_tint, emit_rain, fall_rain, hide_in_interiors)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Material for the lit cloud shader (`assets/shaders/cloud.wgsl`).
/// Each cloud gets its own instance of this asset so per-cloud tint /
/// alpha can vary live without leaking between clouds.
#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct CloudMaterial {
    #[uniform(0)]
    pub params: CloudParams,
    #[texture(1)]
    #[sampler(2)]
    pub texture: Handle<Image>,
}

#[derive(bevy::render::render_resource::ShaderType, Clone, Copy, Debug)]
pub struct CloudParams {
    /// rgb = sky tint, a = base alpha multiplier (per-cloud density).
    pub tint: Vec4,
    /// xyz = sun world position (matches DayNightCycle::light_pos),
    /// w = unused.
    pub sun_pos: Vec4,
}

impl Material2d for CloudMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/cloud.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

#[derive(Component)]
struct Cloud {
    velocity: Vec2, // px per second
    /// Alpha chosen at spawn, preserved so `update_cloud_tint` can
    /// rewrite the Sprite color every frame (shifting hue with
    /// time of day) without losing per-cloud density variation.
    base_alpha: f32,
}

#[derive(Component)]
struct CloudRoot; // tag on every cloud for easy show/hide

#[derive(Component)]
struct CloudShadow {
    /// Alpha chosen at spawn (based on the cloud's density). The
    /// per-frame updater multiplies this by the day-ness factor so
    /// shadows fade to nothing at night — moonlight is too weak to
    /// cast a crisp shadow. Without storing this we'd lose the per-
    /// cloud variation on the first frame.
    base_alpha: f32,
}

/// Marker + emit-rate carry for rainy clouds. Accumulates fractional
/// "drops owed" so non-integer per-frame emission rates work out.
#[derive(Component, Default)]
struct RainyCloud {
    drops_owed: f32,
}

#[derive(Component)]
struct RainDrop {
    /// Y coordinate (world-space) where the drop was spawned. Used to
    /// despawn the drop once it has fallen DROP_FALL_DISTANCE.
    spawn_y: f32,
}

/// Generate a cloud texture shaped by fractal Brownian motion noise.
/// The raw noise is multiplied by a soft elliptical falloff so edges
/// fade to transparent rather than getting chopped at the texture
/// boundary. Each call with a different `seed` produces a distinct
/// shape — we bake a few at startup and let clouds pick from them.
fn generate_cloud_image(seed: u32) -> Image {
    let w = CLOUD_TEX_W;
    let h = CLOUD_TEX_H;
    let cx = w as f32 / 2.0;
    let cy = h as f32 / 2.0;
    let rx = cx * 0.95;
    let ry = cy * 0.90;
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            // Noise scaled so ~2-3 "cells" span the width — produces
            // recognizable cloud clumps rather than TV static.
            let nx = x as f32 * 0.035;
            let ny = y as f32 * 0.060;
            let n = fbm(nx, ny, seed);
            // Threshold + stretch so the lowest-noise regions go fully
            // transparent and the peaks are near solid — gives the
            // irregular-edge look rather than a uniform haze.
            let density = ((n - 0.40) * 2.2).clamp(0.0, 1.0);

            // Elliptical falloff — values already in [0, 1] outside center
            // and 1 in the middle; smoothstep for a soft halo.
            let dx = (x as f32 - cx) / rx;
            let dy = (y as f32 - cy) / ry;
            let d2 = dx * dx + dy * dy;
            let t = (1.0 - d2).clamp(0.0, 1.0);
            let radial = t * t * (3.0 - 2.0 * t);

            let alpha = (density * radial * 255.0) as u8;
            // Warm near-white — keeps clouds from looking steel-blue.
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

/// 2D value noise — hash-based random at integer lattice points,
/// smoothly interpolated inside each cell. Output is in [0, 1).
fn value_noise(x: f32, y: f32, seed: u32) -> f32 {
    let xi = x.floor() as i32;
    let yi = y.floor() as i32;
    let fx = x - xi as f32;
    let fy = y - yi as f32;

    let v00 = hash2d(xi,     yi,     seed);
    let v10 = hash2d(xi + 1, yi,     seed);
    let v01 = hash2d(xi,     yi + 1, seed);
    let v11 = hash2d(xi + 1, yi + 1, seed);

    // Smoothstep eases the grid so we don't see cell boundaries.
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);

    let a = v00 + (v10 - v00) * sx;
    let b = v01 + (v11 - v01) * sx;
    a + (b - a) * sy
}

/// Deterministic unit-interval hash — same (x, y, seed) → same output.
fn hash2d(x: i32, y: i32, seed: u32) -> f32 {
    let mut h = (x as u32)
        .wrapping_mul(374761393)
        .wrapping_add((y as u32).wrapping_mul(668265263))
        .wrapping_add(seed);
    h ^= h >> 13;
    h = h.wrapping_mul(1274126177);
    h ^= h >> 16;
    (h & 0x00FF_FFFF) as f32 / (1u32 << 24) as f32
}

/// Fractal Brownian motion: stack four octaves of value noise with
/// doubling frequency and halving amplitude. Output normalized to [0, 1).
fn fbm(x: f32, y: f32, seed: u32) -> f32 {
    let mut sum = 0.0;
    let mut amp = 0.5;
    let mut freq = 1.0;
    // Sum of amps = 0.5 + 0.25 + 0.125 + 0.0625 = 0.9375 — near 1,
    // so result already ~[0, 1) without explicit normalization.
    for octave in 0..4 {
        sum += value_noise(x * freq, y * freq, seed.wrapping_add(octave * 101)) * amp;
        amp *= 0.5;
        freq *= 2.0;
    }
    sum.min(1.0)
}

fn spawn_clouds(
    mut commands: Commands,
    mut images: ResMut<Assets<Image>>,
    mut meshes: ResMut<Assets<Mesh>>,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    // Bake a few distinct fBm textures so clouds aren't identical
    // silhouettes. Different noise seeds = different shapes.
    let textures: Vec<Handle<Image>> = (0..CLOUD_VARIANTS)
        .map(|i| images.add(generate_cloud_image(0x1234 + i * 7919)))
        .collect();
    // Single shared quad mesh sized to the cloud texture; each cloud's
    // Transform.scale stretches it to its on-world size.
    let mesh_handle = meshes.add(Rectangle::new(CLOUD_TEX_W as f32, CLOUD_TEX_H as f32));

    for i in 0..CLOUD_COUNT {
        let x = rand_range(0.0, world_px_w());
        // World Y is NEGATIVE (tile row 0 at y=0, last row at y=-world_px_h).
        let y = rand_range(-world_px_h(), 0.0);
        let scale = rand_range(2.5, 4.5);
        // Rainy clouds are darker + emit drops. ~30 % of the pool.
        let is_rainy = rand01() < RAINY_FRACTION;
        // Low alpha so overlapping clouds naturally build density; each
        // pass adds only a small amount of haze. Rainy clouds slightly
        // opaquer + desaturated so they read as "the storm clouds".
        let alpha = if is_rainy { rand_range(0.35, 0.55) } else { rand_range(0.15, 0.30) };
        let vx = rand_range(8.0, 14.0);
        let vy = rand_range(-1.0, 1.0); // tiny vertical drift for life
        let tex = textures[i % textures.len()].clone();
        // Per-cloud material — tint + sun_pos written every frame by
        // update_cloud_tint. Initial values are placeholders.
        let material = materials.add(CloudMaterial {
            params: CloudParams {
                tint: Vec4::new(1.0, 1.0, 1.0, alpha),
                sun_pos: Vec4::new(0.0, 0.0, 8000.0, 0.0),
            },
            texture: tex.clone(),
        });
        let mut cloud_entity = commands.spawn((
            Mesh2d(mesh_handle.clone()),
            MeshMaterial2d(material),
            // Scale only X/Y. Bevy's transform propagation multiplies a
            // child's local Z by the parent's scale.z; if we scaled Z
            // here too, the child shadow's local z=-19.5 would get
            // multiplied to -40+ and end up below the ground sprite
            // at z=0 — invisible. Scale.z is cosmetic for 2D sprites.
            Transform::from_xyz(x, y, 20.0).with_scale(Vec3::new(scale, scale, 1.0)),
            Cloud { velocity: Vec2::new(vx, vy), base_alpha: alpha },
            CloudRoot,
        ));
        if is_rainy { cloud_entity.insert(RainyCloud::default()); }
        cloud_entity.with_children(|p| {
            // Ground shadow — same noise texture, dark color, slightly
            // larger, offset down-right to imply sun from upper-left.
            // Child transform is in the parent's local space, so (dx, dy)
            // here is in "texture units" that scale with the cloud.
            // Local z is parent_z + local_z = 20 - 19.5 = 0.5 world —
            // above ground (0.0) but below monsters (1.5), fog (2.0),
            // path markers (3.0), and the player (5.0). So the shadow
            // darkens the ground without dimming anything that's ON the
            // ground.
            // Bumped multiplier + floor when cloud shadows started
            // tracking the real sun — the prior static offset hid the
            // shadow under the cloud, so lower alpha read OK; now the
            // shadow falls beside the cloud and needs to actually darken
            // the ground to register.
            let base_alpha = (alpha * 1.6).max(0.40);
            p.spawn((
                Sprite {
                    image: tex,
                    // Pure black reads as shadow (not haze / fog) without
                    // going so deep it looks like smoke. `base_alpha` set
                    // from cloud density + boosted a bit with a floor —
                    // visible but subtle. The per-frame updater
                    // (update_cloud_shadows) writes this into the color
                    // every tick multiplied by the day-ness factor, so
                    // shadows naturally fade out by night.
                    color: Color::srgba(0.0, 0.0, 0.0, base_alpha),
                    ..default()
                },
                // Same "don't scale Z" rule — keep child z math exact.
                // Translation XY is re-written each frame by
                // update_cloud_shadows to track the current sun
                // direction; initial value doesn't matter much.
                Transform::from_xyz(0.0, 0.0, -19.5)
                    .with_scale(Vec3::new(1.25, 1.25, 1.0)),
                CloudShadow { base_alpha },
            ));
        });
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
    let pad_x = (CLOUD_TEX_W as f32) * 3.0;
    for (cloud, mut tf) in &mut q {
        tf.translation.x += cloud.velocity.x * dt;
        tf.translation.y += cloud.velocity.y * dt;
        if tf.translation.x > ww + pad_x {
            // Wrap to the left edge, randomize vertical so it doesn't
            // look like the same cloud coming back.
            tf.translation.x = -pad_x;
            tf.translation.y = rand_range(-wh, 0.0);
        }
        // Clamp vertical to the world's (negative-Y) range so clouds don't
        // escape over long idle sessions.
        if tf.translation.y > 0.0 { tf.translation.y = 0.0; }
        if tf.translation.y < -wh { tf.translation.y = -wh; }
    }
}

/// Per-frame: tint each cloud sprite with the sky color from the
/// day/night cycle so clouds turn pink-orange at dusk/dawn and cool
/// grey at night. Rainy clouds stay darker — multiplied by a fixed
/// factor so they keep their "storm cloud" read without having to
/// hand-tune two separate tint curves.
fn update_cloud_tint(
    cycle: Res<crate::daynight::DayNightCycle>,
    debug: Res<crate::terrain::tilemap::DebugOptions>,
    rainy: Query<(), With<RainyCloud>>,
    clouds: Query<(Entity, &Cloud, &MeshMaterial2d<CloudMaterial>), With<CloudRoot>>,
    mut materials: ResMut<Assets<CloudMaterial>>,
) {
    let tint = cycle.sky_tint();
    // Sun position: F8 debug override or cycle. Mirrors the resolution
    // order used by every other shader so they all see the same sun.
    let sun_pos = if debug.debug_sun_enabled {
        Vec4::new(debug.debug_sun_x, debug.debug_sun_y, debug.debug_sun_z, 0.0)
    } else {
        let center = Vec2::new(world_px_w() / 2.0, -world_px_h() / 2.0);
        let p = cycle.light_pos(center);
        Vec4::new(p.x, p.y, p.z, 0.0)
    };
    for (entity, cloud, mat_handle) in &clouds {
        // Rainy clouds are saturated-darker versions of the same hue,
        // not a totally different color — that way they still shift
        // pinker at dusk, just stay moodier overall.
        let m = if rainy.contains(entity) { 0.60_f32 } else { 1.0 };
        if let Some(mat) = materials.get_mut(&mat_handle.0) {
            mat.params.tint = Vec4::new(tint.x * m, tint.y * m, tint.z * m, cloud.base_alpha);
            mat.params.sun_pos = sun_pos;
        }
    }
}

/// Per-frame: project each cloud's shadow onto the ground using the
/// current sun position. Longer shadows at dawn/dusk, tiny shadows at
/// noon, nothing at night. The projection math treats the cloud as
/// floating at `CLOUD_HEIGHT` world pixels above the ground; the
/// light ray from sun through cloud continues to z=0 and the shadow
/// sits there. Using a fake abstract height (not a real Z on the
/// cloud entity) because top-down 2D has no true third dimension —
/// this just controls how exaggerated the offset looks.
fn update_cloud_shadows(
    cycle: Res<crate::daynight::DayNightCycle>,
    debug: Res<crate::terrain::tilemap::DebugOptions>,
    clouds: Query<(&Transform, &Children), With<Cloud>>,
    mut shadows: Query<(&mut Transform, &mut Sprite, &CloudShadow), Without<Cloud>>,
) {
    /// Cloud "height" for the projection. Taller = shadows get longer
    /// faster as the sun moves toward horizon. Set high enough that
    /// the computed offset is usually past the cloud sprite's own
    /// footprint (cloud scale × 192 px / 2 ≈ 200 px across), so the
    /// shadow visibly falls to the SIDE of the cloud instead of
    /// disappearing under it.
    const CLOUD_HEIGHT: f32 = 220.0;
    /// Cap on shadow offset so near-horizon suns don't flick shadows
    /// halfway across the map. With CLOUD_HEIGHT=220 the unclamped
    /// offset would exceed this almost all day, so in practice this
    /// sets the shadow distance at a fixed ~140 px in the direction
    /// opposite the sun — trading realism for consistent visibility.
    const MAX_OFFSET: f32 = 140.0;

    let light = if debug.debug_sun_enabled {
        Vec3::new(debug.debug_sun_x, debug.debug_sun_y, debug.debug_sun_z)
    } else {
        let center = Vec2::new(world_px_w() / 2.0, -world_px_h() / 2.0);
        cycle.light_pos(center)
    };
    // Opacity fades with day-ness: bright day → full shadow, midnight
    // → zero. `night_alpha` is 0 at day / 1 at midnight so we invert.
    let day_factor = (1.0 - cycle.night_alpha()).clamp(0.0, 1.0);

    for (cloud_tf, children) in &clouds {
        let cloud_pos = cloud_tf.translation;
        let scale = cloud_tf.scale.x.max(0.1);
        let to_sun = Vec3::new(
            light.x - cloud_pos.x,
            light.y - cloud_pos.y,
            light.z.max(1.0),
        );
        let dir = to_sun.normalize();
        // t = CLOUD_HEIGHT / dir.z — how far along -dir we travel to
        // reach the ground. Clamped z floor keeps horizon-grazing
        // suns from producing absurdly long shadows (divided by 0).
        let t = CLOUD_HEIGHT / dir.z.max(0.3);
        let mut world_ox = -dir.x * t;
        let mut world_oy = -dir.y * t;
        let len = (world_ox * world_ox + world_oy * world_oy).sqrt();
        if len > MAX_OFFSET {
            world_ox = world_ox / len * MAX_OFFSET;
            world_oy = world_oy / len * MAX_OFFSET;
        }
        // Shadow is a child with the cloud scaled. Divide world offset
        // by parent scale so the local translation produces the right
        // world-space position after the parent's transform applies.
        let local_ox = world_ox / scale;
        let local_oy = world_oy / scale;

        for child in children.iter() {
            if let Ok((mut tf, mut sprite, shadow)) = shadows.get_mut(*child) {
                tf.translation.x = local_ox;
                tf.translation.y = local_oy;
                // Preserve z (already -19.5 from spawn; overwrite to be safe).
                tf.translation.z = -19.5;
                sprite.color = Color::srgba(0.0, 0.0, 0.0, shadow.base_alpha * day_factor);
            }
        }
    }
}

/// Hide clouds when the player is in an interior — caves/castles have
/// no sky. Toggling visibility is cheaper than despawning + respawning
/// and keeps the drift state intact so clouds resume where they left off.
/// Rain drops are despawned entirely when indoors — they're cheap to
/// re-spawn and would otherwise accumulate while hidden.
fn hide_in_interiors(
    mut commands: Commands,
    state: Res<MyPlayerState>,
    world: Option<Res<WorldGrid>>,
    mut q: Query<&mut Visibility, With<CloudRoot>>,
    drops: Query<Entity, With<RainDrop>>,
) {
    // MyPlayerState.location is None on the overworld and Some(id) while
    // inside an interior. WorldGrid presence is a secondary safety check
    // (clouds shouldn't render before the world is loaded).
    let show = world.is_some() && state.location.is_none();
    for mut vis in &mut q {
        *vis = if show { Visibility::Visible } else { Visibility::Hidden };
    }
    if !show {
        for e in &drops { commands.entity(e).despawn(); }
    }
}

/// Each rainy cloud spawns drops at DROPS_PER_CLOUD_PER_SEC. The entity
/// carries a fractional "owed" counter so non-integer per-frame rates
/// integrate correctly over time.
fn emit_rain(
    mut commands: Commands,
    time: Res<Time>,
    state: Res<MyPlayerState>,
    world: Option<Res<WorldGrid>>,
    fog: Option<Res<FogOfWar>>,
    mut q: Query<(&Transform, &mut RainyCloud, &Visibility), With<CloudRoot>>,
) {
    if world.is_none() || state.location.is_some() { return; }
    let dt = time.delta_secs();
    for (tf, mut rainy, vis) in &mut q {
        if *vis == Visibility::Hidden { continue; }
        rainy.drops_owed += DROPS_PER_CLOUD_PER_SEC * dt;
        while rainy.drops_owed >= 1.0 {
            rainy.drops_owed -= 1.0;
            // Spawn inside the cloud's footprint. World coords are already
            // baked into the cloud's Transform; use its scale to spread
            // drops across the visible cloud width.
            // cloud scale.x represents its world-scale factor (sprite is
            // CLOUD_TEX_W wide before scaling).
            let half_w = (CLOUD_TEX_W as f32 * tf.scale.x) * 0.4;
            let spawn_x = tf.translation.x + rand_range(-half_w, half_w);
            let spawn_y = tf.translation.y - DROP_START_Y_BELOW_CLOUD;
            let alpha = rand_range(DROP_ALPHA_MIN, DROP_ALPHA_MAX);
            // "Adaptive" color: check the fog bitfield at the drop's
            // spawn tile. Over fogged (unrevealed) terrain the drop is
            // rendered pale-blue so it reads against the dark fog.
            // Over revealed terrain the drop is dark blue-grey so it
            // reads against bright biomes. Chosen at spawn — drops fall
            // only ~5 tiles so the color stays correct through the fall.
            let (tx, ty) = WorldGrid::world_to_tile(Vec2::new(spawn_x, spawn_y));
            let revealed = fog.as_ref()
                .and_then(|f| f.revealed.get(ty * world_w() + tx).copied())
                .unwrap_or(true);
            let color = if revealed {
                Color::srgba(0.15, 0.25, 0.50, alpha) // dark over terrain
            } else {
                Color::srgba(0.80, 0.88, 1.00, alpha) // light over fog
            };
            commands.spawn((
                Sprite {
                    color,
                    custom_size: Some(Vec2::new(1.0, 5.0)),
                    ..default()
                },
                Transform::from_xyz(spawn_x, spawn_y, 19.0), // below clouds (z=20), above shadows
                RainDrop { spawn_y },
            ));
        }
    }
}

/// Move drops downward and despawn once they've fallen far enough.
fn fall_rain(
    mut commands: Commands,
    time: Res<Time>,
    mut q: Query<(Entity, &RainDrop, &mut Transform)>,
) {
    let dt = time.delta_secs();
    for (e, drop, mut tf) in &mut q {
        tf.translation.y -= DROP_SPEED * dt;
        if drop.spawn_y - tf.translation.y >= DROP_FALL_DISTANCE {
            commands.entity(e).despawn();
        }
    }
}
