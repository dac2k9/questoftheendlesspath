//! GPU terrain lighting — Phase 2 of the day/night effort.
//!
//! Replaces the CPU-baked F6 overlay with a `Material2d` shader that
//! samples a per-tile heightmap and applies Phong lighting with the
//! day/night cycle's live sun position. Result: mountains and slopes
//! everywhere in the world brighten/darken as the sun tracks across
//! the sky over its 1-2 min cycle.
//!
//! F6 still toggles. When on, a world-size Mesh2d with this material
//! renders at z=0.3 (same slot the old CPU overlay used).
//!
//! Pipeline:
//!   1. At toggle-on: bake a 100×80 R8 heightmap from biomes (blurred
//!      3× for smooth slopes).
//!   2. Upload as a texture with linear filtering.
//!   3. Spawn the lit-overlay sprite.
//!   4. `update_material` every frame pushes the live sun position
//!      from DayNightCycle (or F8 debug override).

use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, ShaderRef, TextureDimension, TextureFormat,
};
use bevy::sprite::{AlphaMode2d, Material2d, Material2dPlugin};

use crate::states::AppState;
use crate::terrain::world::{WorldGrid, TILE_PX, WORLD_H, WORLD_W};

pub struct TerrainLightingPlugin;

impl Plugin for TerrainLightingPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<TerrainLightingMaterial>::default())
            .add_systems(
                Update,
                (toggle_and_manage, update_material)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}


#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct TerrainLightingMaterial {
    #[uniform(0)]
    pub params: TerrainParams,
    #[texture(1)]
    #[sampler(2)]
    pub heightmap: Handle<Image>,
    /// R8 per-pixel distance-to-nearest-water, in pixels, clamped
    /// into 0..=255. Shader samples this + its gradient to apply
    /// the 5-pixel shoreline bevel (tilt normal toward water).
    #[texture(3)]
    #[sampler(4)]
    pub water_dist: Handle<Image>,
}

#[derive(bevy::render::render_resource::ShaderType, Clone, Copy, Debug)]
pub struct TerrainParams {
    pub time: f32,
    pub ambient: f32,
    pub max_alpha: f32,
    /// 1.0 → render the per-pixel normal as RGB instead of darkening.
    /// Shared with the water shader's F9 toggle so both visualize at
    /// once. Shoreline bevel shows up strongly here since the bevel
    /// modifies the normal significantly along the 2.5-px coastal band.
    pub show_normals: f32,
    pub sun_pos: Vec4,
    /// Highlight tint for sun-facing slopes (hillshade bright pass).
    /// Warm white by day, cool blue by night. xyz = color, w unused.
    pub sun_tint: Vec4,
    /// 1.0 → render the raw heightmap as grayscale for debugging
    /// (F10). Takes priority over `show_normals`.
    pub show_heightmap: f32,
    /// Multiplier on the heightmap gradient when deriving the normal.
    /// Higher = stronger slope shading (mountains pop more). Tuned
    /// live via PageUp/PageDown; default in DebugOptions.
    pub height_amp: f32,
    pub _pad1: f32,
    pub _pad2: f32,
}

impl Material2d for TerrainLightingMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/terrain_lighting.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

#[derive(Component)]
struct TerrainLightingSprite;

fn toggle_and_manage(
    mut commands: Commands,
    debug: Res<super::tilemap::DebugOptions>,
    world: Option<Res<WorldGrid>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<TerrainLightingMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    existing: Query<Entity, With<TerrainLightingSprite>>,
) {
    // F6 toggle is handled in the old lighting.rs module — read the
    // same flag here so the two toggles stay linked. (The old CPU
    // overlay now lives alongside this one as a fallback; see
    // lighting.rs::toggle_and_manage_overlay which spawns it only
    // when debug.lighting_enabled and this sprite isn't around.)
    let want = debug.lighting_enabled;
    let has = !existing.is_empty();
    if want == has {
        return;
    }

    if !want {
        for e in &existing {
            commands.entity(e).despawn_recursive();
        }
        return;
    }

    let Some(world) = world else { return };

    let height_handle = images.add(generate_heightmap(&world));
    let water_dist_handle = images.add(generate_water_distance(&world));
    let material = materials.add(TerrainLightingMaterial {
        params: TerrainParams {
            time: 0.0,
            ambient: 0.30,
            max_alpha: 0.45,
            show_normals: 0.0,
            sun_pos: Vec4::new(-10_000.0, -10_000.0, 8_000.0, 0.0),
            sun_tint: Vec4::new(1.0, 0.95, 0.80, 0.0),
            show_heightmap: 0.0,
            height_amp: 0.0, // filled in on first update_material tick
            _pad1: 0.0,
            _pad2: 0.0,
        },
        heightmap: height_handle,
        water_dist: water_dist_handle,
    });

    // World-size mesh, placed at the map sprite's origin convention
    // (tile (0,0) centered at world origin → sprite center offset by
    // half a tile). z=0.3 matches where the old CPU overlay sat.
    let w = WORLD_W as f32 * TILE_PX;
    let h = WORLD_H as f32 * TILE_PX;
    let cx = w / 2.0 - TILE_PX / 2.0;
    let cy = -h / 2.0 + TILE_PX / 2.0;
    let mesh_handle = meshes.add(Rectangle::new(w, h));

    commands.spawn((
        Mesh2d(mesh_handle),
        MeshMaterial2d(material),
        // z=1.0 sits above the ground mesh (which can push vertex z
        // up to 0.95 with the live tile_z_factor knob) but below
        // sprite layers (player/POIs at 1.5+). Was 0.3 originally;
        // bumped so the slope-shading overlay isn't occluded by
        // raised mountain quads.
        Transform::from_xyz(cx, cy, 1.0),
        TerrainLightingSprite,
    ));
}

fn update_material(
    time: Res<Time>,
    debug: Res<super::tilemap::DebugOptions>,
    cycle: Res<crate::daynight::DayNightCycle>,
    mut materials: ResMut<Assets<TerrainLightingMaterial>>,
    q: Query<&MeshMaterial2d<TerrainLightingMaterial>>,
) {
    let t = time.elapsed_secs();
    let sun_pos = if debug.debug_sun_enabled {
        Vec4::new(debug.debug_sun_x, debug.debug_sun_y, debug.debug_sun_z, 0.0)
    } else {
        let w = WORLD_W as f32 * TILE_PX;
        let h = WORLD_H as f32 * TILE_PX;
        let center = Vec2::new(w / 2.0, -h / 2.0);
        let p = cycle.light_pos(center);
        Vec4::new(p.x, p.y, p.z, 0.0)
    };
    let show_normals = if debug.debug_show_normals { 1.0 } else { 0.0 };
    let show_heightmap = if debug.debug_show_heightmap { 1.0 } else { 0.0 };
    // Shared time-of-day color: warm white at noon, amber at
    // dusk/dawn, cool blue at midnight. See DayNightCycle::sky_tint
    // for the curve; clouds use it too so sunsets read as unified.
    let tint = cycle.sky_tint();
    let sun_tint = Vec4::new(tint.x, tint.y, tint.z, 0.0);
    for handle in &q {
        if let Some(mat) = materials.get_mut(&handle.0) {
            mat.params.time = t;
            mat.params.sun_pos = sun_pos;
            mat.params.show_normals = show_normals;
            mat.params.sun_tint = sun_tint;
            mat.params.show_heightmap = show_heightmap;
            mat.params.height_amp = debug.terrain_height_amp;
        }
    }
}

// ── Heightmap generation (100×80 R8) ────────────────────────────────

fn generate_heightmap(world: &WorldGrid) -> Image {
    let w = WORLD_W;
    let h = WORLD_H;
    // Build height per tile from biome, then 3× box blur so slopes
    // transition smoothly across tile boundaries instead of stepping.
    let mut height = vec![0.0_f32; w * h];
    for y in 0..h {
        for x in 0..w {
            height[y * w + x] = biome_height(world, x, y);
        }
    }
    let mut blurred = height.clone();
    for _ in 0..3 {
        blurred = box_blur_3x3(&blurred, w, h);
    }

    // Pack into R8Unorm — single channel, 1 byte per pixel.
    let mut data = Vec::with_capacity(w * h);
    for v in &blurred {
        data.push((v.clamp(0.0, 1.0) * 255.0) as u8);
    }

    let mut img = Image::new(
        Extent3d {
            width: w as u32,
            height: h as u32,
            depth_or_array_layers: 1,
        },
        TextureDimension::D2,
        data,
        TextureFormat::R8Unorm,
        bevy::render::render_asset::RenderAssetUsages::all(),
    );
    // Linear filter + clamp — we WANT smoothing across texels, and
    // clamp at the edges (sampling a wrapped edge would show the
    // opposite side of the map, nonsense).
    use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..Default::default()
    });
    img
}

/// Biome → relative height in [0, 1]. Water is lowest, mountain highest.
/// Road tiles drop a small notch below their surrounding biome so the
/// blur + hillshade together pick the road out as a worn, slightly
/// recessed path. The 3× box blur that runs over this map smooths
/// single-tile dips into a soft channel along the road's length —
/// readable from above as "land that's been walked into".
fn biome_height(world: &WorldGrid, x: usize, y: usize) -> f32 {
    use questlib::mapgen::Biome::*;
    let base = match world.map.biome_at(x, y) {
        Water => 0.00,
        DeepWater => 0.00,
        Swamp => 0.15,
        Desert => 0.25,
        Grassland => 0.40,
        Forest => 0.50,
        DenseForest => 0.55,
        Mountain => 1.00,
        Snow => 0.85,
    };
    if world.map.has_road_at(x, y) {
        // 0.08 is enough to register as a slope after the 3× blur,
        // but small enough that mountain roads don't suddenly look
        // like trenches. Tune by eye if needed.
        (base - 0.08_f32).max(0.0)
    } else {
        base
    }
}

fn box_blur_3x3(input: &[f32], w: usize, h: usize) -> Vec<f32> {
    let mut out = vec![0.0_f32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut sum = 0.0_f32;
            let mut count = 0;
            for dy in -1i32..=1 {
                for dx in -1i32..=1 {
                    let nx = x as i32 + dx;
                    let ny = y as i32 + dy;
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 {
                        continue;
                    }
                    sum += input[ny as usize * w + nx as usize];
                    count += 1;
                }
            }
            out[y * w + x] = sum / count as f32;
        }
    }
    out
}

// ── Water-distance field (per-pixel distance to nearest water) ──────
//
// Baked at world pixel resolution (WORLD_W*TILE_PX × WORLD_H*TILE_PX)
// as an R8 texture where each byte = distance in pixels clamped to
// 0..=255. Shoreline bevel only needs 0..=5 so R8 is plenty.
//
// Per-pixel the algorithm: check the 9 tiles (self + 8 neighbors);
// for any water tile in the neighborhood, compute distance from the
// pixel to that tile's rectangle and take the min. Same routine
// the old CPU lighting used — same result, just baked once into a
// texture so the GPU shader can sample it per fragment.

fn generate_water_distance(world: &WorldGrid) -> Image {
    use questlib::mapgen::Biome::*;
    let pw = WORLD_W * TILE_PX as usize;
    let ph = WORLD_H * TILE_PX as usize;
    let mut data = Vec::with_capacity(pw * ph);
    for py in 0..ph {
        for px in 0..pw {
            // Sample at pixel CENTERS (+0.5). Using the top-left corner
            // put boundary-land pixels at dx=0 from the water tile's
            // edge, making their distance indistinguishable from water
            // (d=0). That leaked into the shader as a flat "hole" in
            // the bevel ramp — see the screenshot bug where one
            // coastline pixel showed the untilted base normal.
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            let tile_x = (fx / TILE_PX).floor() as i32;
            let tile_y = (fy / TILE_PX).floor() as i32;
            let in_world = tile_x >= 0 && tile_y >= 0
                && tile_x < WORLD_W as i32 && tile_y < WORLD_H as i32;
            let own_is_water = in_world && matches!(
                world.map.biome_at(tile_x as usize, tile_y as usize),
                Water | DeepWater
            );
            if own_is_water {
                data.push(0);
                continue;
            }

            let mut min_d = f32::INFINITY;
            for ny in -1i32..=1 {
                for nx in -1i32..=1 {
                    if nx == 0 && ny == 0 { continue; }
                    let bx = tile_x + nx;
                    let by = tile_y + ny;
                    if bx < 0 || by < 0 || bx >= WORLD_W as i32 || by >= WORLD_H as i32 {
                        continue;
                    }
                    if !matches!(
                        world.map.biome_at(bx as usize, by as usize),
                        Water | DeepWater
                    ) {
                        continue;
                    }
                    let n_left   = bx as f32 * TILE_PX;
                    let n_right  = n_left + TILE_PX;
                    let n_top    = by as f32 * TILE_PX;
                    let n_bottom = n_top  + TILE_PX;
                    let cx = fx.clamp(n_left, n_right);
                    let cy = fy.clamp(n_top,  n_bottom);
                    let dx = fx - cx;
                    let dy = fy - cy;
                    let d = (dx * dx + dy * dy).sqrt();
                    if d < min_d { min_d = d; }
                }
            }
            // ceil, not truncate: a land pixel with subpixel distance
            // (e.g. 0.5 from pixel-center sampling) must round UP to 1
            // so the shader's `d_here <= 0.0` water check doesn't
            // falsely snap it back to "water, stay flat".
            let byte = if min_d.is_finite() { min_d.ceil().min(255.0) as u8 } else { 255 };
            data.push(byte);
        }
    }
    let mut img = Image::new(
        Extent3d { width: pw as u32, height: ph as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::R8Unorm,
        bevy::render::render_asset::RenderAssetUsages::all(),
    );
    // Linear filtering so gradient sampling in the shader returns
    // smooth values; clamp edges so sampling past the world edge
    // returns the border pixels (reasonable since nothing exists past).
    use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Linear,
        min_filter: ImageFilterMode::Linear,
        ..Default::default()
    });
    img
}
