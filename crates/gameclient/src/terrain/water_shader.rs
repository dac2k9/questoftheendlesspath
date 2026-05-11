//! Tier-2 water animation — a GPU shader ripple effect over water tiles.
//!
//! Press **F7** to toggle. Spawns one Mesh2d with a `WaterMaterial`
//! covering the whole world; the shader (`assets/shaders/water.wgsl`)
//! discards non-water pixels using a mask texture and renders an
//! additive Blinn-Phong shimmer on the rest. Time is pushed into the
//! `WaterMaterial` uniform every frame by `update_time`.

use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, ShaderRef, Extent3d, TextureDimension, TextureFormat};
use bevy::sprite::{Material2d, Material2dPlugin, AlphaMode2d};

use crate::states::AppState;
use crate::terrain::world::{WorldGrid, TILE_PX, world_w, world_h};

pub struct WaterShaderPlugin;

impl Plugin for WaterShaderPlugin {
    fn build(&self, app: &mut App) {
        app
            .add_plugins(Material2dPlugin::<WaterMaterial>::default())
            .add_systems(
                Update,
                (
                    toggle_and_manage,
                    debug_sun_input,
                    toggle_show_normals,
                    update_material,
                    update_sun_marker,
                )
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct WaterMaterial {
    #[uniform(0)]
    pub params: WaterParams,
    #[texture(1)]
    #[sampler(2)]
    pub mask: Handle<Image>,
    /// Pre-baked tileable normal map (RGBA8). Shader samples it with
    /// two scrolling UV offsets to produce animated waves, then
    /// decodes (`rgb * 2 - 1`) and combines the two sampled normals.
    #[texture(3)]
    #[sampler(4)]
    pub normal_map: Handle<Image>,
}

#[derive(bevy::render::render_resource::ShaderType, Clone, Copy, Debug)]
pub struct WaterParams {
    pub time: f32,
    /// Non-zero → shader outputs the normal as RGB for debugging (F9).
    pub show_normals: f32,
    /// Count of active entries in `lights`. Shader casts to int and
    /// loops `0..num_lights`.
    pub num_lights: f32,
    /// Fade factor for the warm lantern shimmer — ramps 0→1 from day
    /// to midnight so player/POI lights only reflect off water at
    /// night, while the sun owns daytime spec.
    pub night_alpha: f32,
    /// Sun position (world x, world y, height-above-water, unused).
    pub sun_pos: Vec4,
    /// Same scene point lights the night_lights shader carves with —
    /// xyz = world pos, w = radius in world px. Populated from the
    /// shared `SceneLights` resource so both shaders stay in sync.
    pub lights: [Vec4; super::night_lights::MAX_LIGHTS],
}

impl Material2d for WaterMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/water.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

/// Marker on the world-sized water sprite.
#[derive(Component)]
struct WaterShaderSprite;

fn toggle_and_manage(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut debug: ResMut<super::tilemap::DebugOptions>,
    world: Option<Res<WorldGrid>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<WaterMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    existing: Query<Entity, With<WaterShaderSprite>>,
) {
    if keys.just_pressed(KeyCode::F7) {
        debug.water_shader_enabled = !debug.water_shader_enabled;
    }

    let want = debug.water_shader_enabled;
    let has = !existing.is_empty();
    if want == has { return; }

    if !want {
        for e in &existing { commands.entity(e).despawn_recursive(); }
        return;
    }

    let Some(world) = world else { return; };

    // Build the water/ice/swamp mask: 100×80 R8, encoding four states
    // so a single shader can render different shimmer styles:
    //   0   — land (shader discards)
    //   64  — swamp (slow, murky, green-tinted ripples)
    //   128 — snow / ice (subtle frosty glints, no flow)
    //   255 — water (full ripple shimmer)
    let mut mask_bytes = vec![0u8; world_w() * world_h()];
    for y in 0..world_h() {
        for x in 0..world_w() {
            mask_bytes[y * world_w() + x] = match world.map.biome_at(x, y) {
                questlib::mapgen::Biome::Water | questlib::mapgen::Biome::DeepWater => 255,
                questlib::mapgen::Biome::Snow => 128,
                questlib::mapgen::Biome::Swamp => 64,
                _ => 0,
            };
        }
    }
    let mut mask_img = Image::new(
        Extent3d { width: world_w() as u32, height: world_h() as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        mask_bytes,
        TextureFormat::R8Unorm,
        bevy::render::render_asset::RenderAssetUsages::all(),
    );
    // Nearest filtering so water/land boundary stays crisp rather than
    // bleeding the shimmer onto shoreline land pixels.
    mask_img.sampler = bevy::image::ImageSampler::nearest();
    let mask_handle = images.add(mask_img);

    // Bake a tileable water-surface normal map (256×256, RGBA8). Runs
    // once per F7 toggle-on. The map is the TRUE "normal map" —
    // xyz normal packed into rgb as (rgb = n*0.5+0.5) — not a bump/
    // heightmap. Shader samples it with scrolling UV for animation.
    let normal_handle = images.add(generate_water_normal_map());

    let material = materials.add(WaterMaterial {
        params: WaterParams {
            time: 0.0,
            show_normals: 0.0,
            num_lights: 0.0,
            night_alpha: 0.0,
            // Default sun: positioned far to the upper-left, high above
            // water. Large distance → per-pixel directions are nearly
            // parallel, which approximates directional sunlight.
            sun_pos: Vec4::new(-10_000.0, -10_000.0, 8_000.0, 0.0),
            lights: [Vec4::ZERO; super::night_lights::MAX_LIGHTS],
        },
        mask: mask_handle,
        normal_map: normal_handle,
    });

    // World rectangle in pixel space, matching the ground sprite's
    // transform convention (tile (0,0) centered at world origin).
    let w = world_w() as f32 * TILE_PX;
    let h = world_h() as f32 * TILE_PX;
    let cx = w / 2.0 - TILE_PX / 2.0;
    let cy = -h / 2.0 + TILE_PX / 2.0;
    let mesh_handle = meshes.add(Rectangle::new(w, h));

    commands.spawn((
        Mesh2d(mesh_handle),
        MeshMaterial2d(material),
        // z=0.99 sits above the procedural ground mesh (which can
        // push vertex z up toward 0.95 with the live tile_z_factor
        // knob) and just below the lighting overlay at 1.0. Was 0.35
        // originally; bumped so snow / water shimmer don't get
        // occluded by raised mountain-quad geometry.
        Transform::from_xyz(cx, cy, 0.99),
        WaterShaderSprite,
    ));
}

/// F8 toggles debug-sun mode. While enabled, the cursor's WORLD
/// position drives the sun's XY position (not a direction — it's a
/// point light), and the mouse wheel drives its height above the
/// water. "Moving a lamp around the scene" — positioning the mouse
/// at a water pixel places the sun directly over that pixel.
fn debug_sun_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut mouse_wheel: EventReader<bevy::input::mouse::MouseWheel>,
    mut debug: ResMut<super::tilemap::DebugOptions>,
    windows: Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
    cameras: Query<(&Camera, &GlobalTransform), With<Camera2d>>,
) {
    if keys.just_pressed(KeyCode::F8) {
        debug.debug_sun_enabled = !debug.debug_sun_enabled;
    }
    if !debug.debug_sun_enabled { return; }

    // Convert cursor (screen px) → world XY via the 2D camera.
    let Ok(window) = windows.get_single() else { return };
    let Some(cursor) = window.cursor_position() else { return };
    let Ok((camera, camera_transform)) = cameras.get_single() else { return };
    let Ok(world_xy) = camera.viewport_to_world_2d(camera_transform, cursor) else { return };
    debug.debug_sun_x = world_xy.x;
    debug.debug_sun_y = world_xy.y;

    // Wheel → sun height above water (the Z component). Small values
    // (close to water) → grazing-angle light with long highlight
    // streaks; large → overhead, bright uniform spec.
    let mut dz = 0.0_f32;
    for ev in mouse_wheel.read() {
        use bevy::input::mouse::MouseScrollUnit;
        dz += match ev.unit {
            MouseScrollUnit::Line  => ev.y * 20.0,
            MouseScrollUnit::Pixel => ev.y * 0.8,
        };
    }
    if dz != 0.0 {
        // Sun height now a world-space scalar. Range [10, 2000] covers
        // "sun just above the water" → "overhead". Defaults to 200.
        debug.debug_sun_z = (debug.debug_sun_z + dz).clamp(10.0, 2_000.0);
    }
}

/// F9 toggles normal-map debug viz — shader renders rgb = n*0.5+0.5
/// instead of Phong so we can sanity-check what normals the texture
/// is producing.
fn toggle_show_normals(
    keys: Res<ButtonInput<KeyCode>>,
    mut debug: ResMut<super::tilemap::DebugOptions>,
) {
    if keys.just_pressed(KeyCode::F9) {
        debug.debug_show_normals = !debug.debug_show_normals;
    }
    if keys.just_pressed(KeyCode::F10) {
        debug.debug_show_heightmap = !debug.debug_show_heightmap;
    }
}

/// Marker for the on-screen sun indicator (visible while F8 is on).
#[derive(Component)]
struct SunMarker;

/// Show a small yellow dot at the cursor when debug-sun is active,
/// plus a "z = 0.80" HUD label so the user sees the current sun_z
/// value without opening the F3 panel.
fn update_sun_marker(
    mut commands: Commands,
    debug: Res<super::tilemap::DebugOptions>,
    font: Res<crate::GameFont>,
    existing: Query<Entity, With<SunMarker>>,
    windows: Query<&bevy::window::Window, With<bevy::window::PrimaryWindow>>,
) {
    let want = debug.debug_sun_enabled;
    let has = !existing.is_empty();

    if !want {
        if has { for e in &existing { commands.entity(e).despawn_recursive(); } }
        return;
    }
    if has { return; }

    let _ = windows; // reserved for future cursor-tracking visual
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(10.0),
            right: Val::Px(10.0),
            padding: UiRect::all(Val::Px(8.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.02, 0.08, 0.8)),
        BorderRadius::all(Val::Px(4.0)),
        ZIndex(50),
        SunMarker,
    )).with_children(|p| {
        p.spawn((
            Text::new("F8 debug sun — cursor = XY · wheel = Z"),
            TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(1.0, 0.95, 0.4)),
        ));
    });
}

/// Push the per-frame uniform values (time + sun direction + scene
/// lights) into every WaterMaterial asset.
fn update_material(
    time: Res<Time>,
    debug: Res<super::tilemap::DebugOptions>,
    cycle: Res<crate::daynight::DayNightCycle>,
    scene: Res<super::night_lights::SceneLights>,
    mut materials: ResMut<Assets<WaterMaterial>>,
    q: Query<&MeshMaterial2d<WaterMaterial>>,
) {
    let t = time.elapsed_secs();
    // Sun position priority:
    //   1. F8 debug override (user dragging it with the mouse)
    //   2. Day/night cycle (sun during day, moon during night, same arc)
    // No static fallback — the cycle resource always exists.
    let sun_pos = if debug.debug_sun_enabled {
        Vec4::new(debug.debug_sun_x, debug.debug_sun_y, debug.debug_sun_z, 0.0)
    } else {
        // World center matches the map/lighting sprite center.
        let w = crate::terrain::world::world_w() as f32 * crate::terrain::world::TILE_PX;
        let h = crate::terrain::world::world_h() as f32 * crate::terrain::world::TILE_PX;
        let center = Vec2::new(w / 2.0, -h / 2.0);
        let p = cycle.light_pos(center);
        Vec4::new(p.x, p.y, p.z, 0.0)
    };
    let show_normals = if debug.debug_show_normals { 1.0 } else { 0.0 };
    for handle in &q {
        if let Some(mat) = materials.get_mut(&handle.0) {
            mat.params.time = t;
            mat.params.sun_pos = sun_pos;
            mat.params.show_normals = show_normals;
            mat.params.num_lights = scene.count as f32;
            mat.params.night_alpha = scene.night_alpha;
            mat.params.lights = scene.lights;
        }
    }
}

// ── Procedural water normal-map generator ──────────────────────────
//
// Builds a 256×256 tileable normal-map texture from fBm value noise:
//   1. Multi-octave value noise → heightmap (tileable via wrap-hashing).
//   2. Central-difference Sobel on wrapped coords → (dx, dy) gradient.
//   3. Normal = normalize(-dx*AMP, -dy*AMP, 1).
//   4. Pack into RGBA8 via rgb = n*0.5+0.5. A = 255.
//
// The classic blue-purple water-normal look: flat regions are
// (0, 0, 1) → packed as (128, 128, 255), tilted spots shift hue.

const NORMAL_MAP_SIZE: u32 = 256;

fn generate_water_normal_map() -> Image {
    let size = NORMAL_MAP_SIZE as usize;
    // Heightmap: sum of 5 octaves, each with a period that divides
    // NORMAL_MAP_SIZE cleanly so the result tiles seamlessly. Higher
    // periods (more cells per axis) put more detail into high-freq
    // bands — that's what creates visible per-pixel gradient variation
    // in the resulting normal map (low-period noise only changes over
    // dozens of pixels, so per-pixel central differences are tiny).
    let mut height = vec![0.0_f32; size * size];
    let octaves: &[(u32, f32)] = &[
        (16, 0.50),
        (32, 0.30),
        (64, 0.20),
        (128, 0.12),
        (256, 0.08),
    ];
    for y in 0..size {
        for x in 0..size {
            let u = x as f32 / NORMAL_MAP_SIZE as f32;
            let v = y as f32 / NORMAL_MAP_SIZE as f32;
            let mut h = 0.0_f32;
            for &(period, amp) in octaves {
                h += tileable_value_noise(u, v, period) * amp;
            }
            height[y * size + x] = h;
        }
    }

    // Convert heights → packed RGBA normal map via central-difference
    // Sobel + bump amplification. AMP is a tuning dial:
    //   too low  → mostly flat normals, sparkles rare, specular is tight
    //              but barely differs as the sun moves
    //   too high → normals point every direction, specular fires almost
    //              everywhere, sun movement doesn't visibly change output
    // 15 is a reasonable middle ground — enough tilt variety to get
    // pinpoint sparkle distribution, not so much that the sun's position
    // becomes irrelevant.
    const AMP: f32 = 15.0;
    let mut data = Vec::with_capacity(size * size * 4);
    for y in 0..size {
        let yp = (y + 1) % size;
        let ym = (y + size - 1) % size;
        for x in 0..size {
            let xp = (x + 1) % size;
            let xm = (x + size - 1) % size;
            let dx = height[y * size + xp] - height[y * size + xm];
            let dy = height[yp * size + x] - height[ym * size + x];
            let nx = -dx * AMP;
            let ny = -dy * AMP;
            let nz = 1.0_f32;
            let len = (nx * nx + ny * ny + nz * nz).sqrt();
            let (nx, ny, nz) = (nx / len, ny / len, nz / len);
            data.push(((nx + 1.0) * 127.5) as u8);
            data.push(((ny + 1.0) * 127.5) as u8);
            data.push(((nz + 1.0) * 127.5) as u8);
            data.push(255);
        }
    }

    let mut img = Image::new(
        Extent3d {
            width: NORMAL_MAP_SIZE,
            height: NORMAL_MAP_SIZE,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        // Linear color space — normals are direction data, not color.
        TextureFormat::Rgba8Unorm,
        bevy::render::render_asset::RenderAssetUsages::all(),
    );
    // CRITICAL: wrap the texture when sampling past uv=1, otherwise the
    // GPU uses ClampToEdge and the edge column/row stretches to infinity —
    // visible as rainbow vertical/horizontal stripes at the water sprite's
    // far edges. Tileable noise generation is pointless without this.
    use bevy::image::{ImageSampler, ImageSamplerDescriptor, ImageAddressMode, ImageFilterMode};
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::Repeat,
        address_mode_v: ImageAddressMode::Repeat,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..Default::default()
    });
    img
}

/// Tileable value noise: deterministic hash on wrapped integer coords,
/// bilinear smoothstep interpolation. `period` = number of grid cells
/// across [0, 1], must divide NORMAL_MAP_SIZE evenly for seamless tiling.
fn tileable_value_noise(u: f32, v: f32, period: u32) -> f32 {
    let p = period as f32;
    let x = u * p;
    let y = v * p;
    let ix = x.floor() as i32;
    let iy = y.floor() as i32;
    let fx = x - ix as f32;
    let fy = y - iy as f32;
    let sx = fx * fx * (3.0 - 2.0 * fx);
    let sy = fy * fy * (3.0 - 2.0 * fy);
    let wrap = |i: i32| -> u32 {
        let p = period as i32;
        (((i % p) + p) % p) as u32
    };
    let h = |x: u32, y: u32| -> f32 {
        // Simple hash → [0, 1).
        let mut n = x.wrapping_mul(374_761_393).wrapping_add(y.wrapping_mul(668_265_263));
        n ^= n >> 13;
        n = n.wrapping_mul(1_274_126_177);
        n ^= n >> 16;
        (n & 0x00FF_FFFF) as f32 / (1u32 << 24) as f32
    };
    let a = h(wrap(ix),     wrap(iy));
    let b = h(wrap(ix + 1), wrap(iy));
    let c = h(wrap(ix),     wrap(iy + 1));
    let d = h(wrap(ix + 1), wrap(iy + 1));
    let ab = a + (b - a) * sx;
    let cd = c + (d - c) * sx;
    ab + (cd - ab) * sy
}
