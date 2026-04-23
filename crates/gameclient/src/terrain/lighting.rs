//! Procedural world-scale lighting overlay (dev preview, F6 to toggle).
//!
//! Pipeline (runs once when the overlay is first enabled):
//!   1. Height per tile from biome (water=0, grass=0.4, mountain=1.0, etc.)
//!   2. Upsample + Gaussian blur → smooth shorelines and slopes
//!   3. Sobel gradient → per-pixel slope vector
//!   4. Lambertian dot with a fixed sun direction → brightness per pixel
//!   5. Map brightness to an alpha-modulated darkness overlay
//!
//! The overlay is one Sprite at z=0.3 covering the whole world. Toggle
//! off → hide the Sprite. Pixel-art friendly: we quantize the alpha into
//! discrete bands so slopes read as stylized shading rather than a
//! gradient blur.

use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::states::AppState;
use crate::terrain::world::{WorldGrid, TILE_PX, WORLD_W, WORLD_H};

/// Sun coming from the upper-left (matches cloud shadow offset).
/// (-1, -1, 0.8) roughly normalized; we dot against (grad.x, grad.y, 0)
/// so only the XY component matters for directional.
const SUN_X: f32 = -0.65;
const SUN_Y: f32 = -0.65;
const LIGHT_Z: f32 = 0.8;
/// How many alpha steps to quantize into — bigger number = smoother,
/// smaller = more stylized. 5 reads nicely against the pixel art.
const QUANT_STEPS: f32 = 5.0;
/// Peak darkness the overlay can apply. 0 = no visible effect; 1 = fully
/// black under the deepest slope.
const MAX_ALPHA: f32 = 0.40;

/// Shoreline bevel: land pixels within this many PIXELS of a water
/// tile's boundary get a cosine-falloff darkening. Measured in world
/// pixels, NOT tiles. Effect is on land only — water pixels don't get
/// bevelled. Simulates the normal rotating from "up" (0,1,0) at the
/// bevel's far side to "horizontal toward water" at the edge, which
/// reads as a curved bevel.
const SHORELINE_BEVEL_WIDTH_PX: f32 = 5.0;
const SHORELINE_BEVEL_STRENGTH: f32 = 0.55;

pub struct LightingPlugin;

impl Plugin for LightingPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (toggle_and_manage_overlay,).run_if(in_state(AppState::InGame)),
        );
    }
}

/// Marker for the overlay sprite entity.
#[derive(Component)]
struct LightingOverlay;

fn toggle_and_manage_overlay(
    mut commands: Commands,
    keys: Res<ButtonInput<KeyCode>>,
    mut debug: ResMut<super::tilemap::DebugOptions>,
    world: Option<Res<WorldGrid>>,
    mut images: ResMut<Assets<Image>>,
    existing: Query<Entity, With<LightingOverlay>>,
) {
    if keys.just_pressed(KeyCode::F6) {
        debug.lighting_enabled = !debug.lighting_enabled;
    }

    let want = debug.lighting_enabled;
    let has = !existing.is_empty();
    if want == has { return; }

    if !want {
        for e in &existing { commands.entity(e).despawn_recursive(); }
        return;
    }

    // Spawn: generate the texture and place one Sprite covering the world.
    let Some(world) = world else { return; };
    let img = generate_lighting_image(&world);
    let handle = images.add(img);
    // World spans [0, WORLD_W * TILE_PX] in x and [-WORLD_H * TILE_PX, 0] in y.
    // Sprite is centered at its Transform; place the center accordingly.
    let w = WORLD_W as f32 * TILE_PX;
    let h = WORLD_H as f32 * TILE_PX;
    commands.spawn((
        Sprite { image: handle, ..default() },
        Transform::from_xyz(w * 0.5, -h * 0.5, 0.3),
        LightingOverlay,
    ));
}

fn generate_lighting_image(world: &WorldGrid) -> Image {
    let w = WORLD_W;
    let h = WORLD_H;
    // ── 1. Heightmap, one cell per tile
    let mut height = vec![0.0_f32; w * h];
    for y in 0..h {
        for x in 0..w {
            height[y * w + x] = biome_height(world, x, y);
        }
    }
    // ── 2. Smooth across tile boundaries (two 3×3 box blur passes)
    let mut blurred = height.clone();
    for _ in 0..3 {
        blurred = box_blur_3x3(&blurred, w, h);
    }

    // ── 2b. (removed — the tile-level distance field was causing the
    //       bevel to bleed onto water via bilinear interpolation.
    //       Replaced with per-pixel `pixel_distance_to_water_boundary`
    //       that only returns >0 for land pixels.)

    // ── 3. Upsample to world pixel size (TILE_PX per tile) with bilinear
    //      interpolation. Also does implicit smoothing between tile cells.
    let pw = w * TILE_PX as usize;
    let ph = h * TILE_PX as usize;
    let mut data = Vec::with_capacity(pw * ph * 4);
    for py in 0..ph {
        // Tile-space y
        let ty = py as f32 / TILE_PX;
        for px in 0..pw {
            let tx = px as f32 / TILE_PX;

            // Bilinear sample of the blurred heightmap.
            let h_center = sample_bilinear(&blurred, w, h, tx, ty);
            // Gradient via central differences — sample neighbors a half-tile
            // away, then normalize direction.
            let hx1 = sample_bilinear(&blurred, w, h, tx - 0.5, ty);
            let hx2 = sample_bilinear(&blurred, w, h, tx + 0.5, ty);
            let hy1 = sample_bilinear(&blurred, w, h, tx, ty - 0.5);
            let hy2 = sample_bilinear(&blurred, w, h, tx, ty + 0.5);
            let dx = hx2 - hx1; // points uphill in +x
            let dy = hy2 - hy1;
            let _ = h_center; // unused; reserved for future "ambient" term

            // Lambertian dot. Normal ≈ (-dx, -dy, LIGHT_Z / slope_scale).
            // We compare the slope vector against the sun XY to get how
            // much the surface is turned away from / toward the sun.
            // Positive = facing sun (bright), negative = away (dark).
            let lit = -dx * SUN_X - dy * SUN_Y + LIGHT_Z;
            // Shift so bright = 1, dark = 0.
            let brightness = lit.clamp(0.0, 2.0) / 2.0;

            // Darkness overlay: alpha increases where brightness is low.
            // 1.0 - brightness, then clamp + quantize.
            let mut shade = (1.0 - brightness).clamp(0.0, 1.0);
            shade = (shade * QUANT_STEPS).floor() / QUANT_STEPS;
            let slope_alpha = shade * MAX_ALPHA;

            // Shoreline bevel: cosine-falloff darkening on land pixels
            // within SHORELINE_BEVEL_WIDTH_PX of a water tile's edge.
            // Euclidean pixel distance — returns 0 for water itself,
            // positive for land. So the effect only shows on land.
            let d_px = pixel_distance_to_water_boundary(world, px as f32, py as f32);
            let bevel = if d_px > 0.0 && d_px < SHORELINE_BEVEL_WIDTH_PX {
                let t = d_px / SHORELINE_BEVEL_WIDTH_PX; // 0..=1
                0.5 * (1.0 + (t * std::f32::consts::PI).cos()) // 1 at edge → 0 inland
            } else {
                0.0
            };
            let bevel_alpha = bevel * SHORELINE_BEVEL_STRENGTH;

            // Combine: take the stronger of the two darkenings rather
            // than adding (would stack too heavily near water under
            // sun-away slopes).
            let alpha_f = slope_alpha.max(bevel_alpha);
            let alpha = (alpha_f * 255.0) as u8;

            // RGB = black; alpha modulated. A warmer tint could be picked
            // if we want sunlit side to feel golden, but keep it simple
            // as a darkness pass for now.
            data.push(0);
            data.push(0);
            data.push(8);
            data.push(alpha);
        }
    }

    Image::new(
        Extent3d { width: pw as u32, height: ph as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::Rgba8UnormSrgb,
        bevy::render::render_asset::RenderAssetUsages::all(),
    )
}

/// Map a biome to a height value in [0, 1].
fn biome_height(world: &WorldGrid, x: usize, y: usize) -> f32 {
    use questlib::mapgen::Biome::*;
    let biome = world.map.biome_at(x, y);
    match biome {
        Water     => 0.00,
        DeepWater => 0.00,
        Swamp     => 0.15,
        Desert    => 0.25,
        Grassland => 0.40,
        Forest    => 0.50,
        DenseForest => 0.55,
        Mountain  => 1.00,
        Snow      => 0.85,
    }
}

/// Euclidean pixel distance from a land pixel `(px, py)` to the nearest
/// water-tile boundary. Returns 0 for pixels whose own tile is water
/// (so bevel never applies on water). Returns `f32::INFINITY` for land
/// pixels whose tile has no water neighbor in 8-connectivity — those
/// are too far from shore to matter.
fn pixel_distance_to_water_boundary(world: &WorldGrid, px: f32, py: f32) -> f32 {
    use questlib::mapgen::Biome::*;
    let tile_x = (px / TILE_PX).floor() as i32;
    let tile_y = (py / TILE_PX).floor() as i32;
    if tile_x < 0 || tile_y < 0 || tile_x >= WORLD_W as i32 || tile_y >= WORLD_H as i32 {
        return f32::INFINITY;
    }
    let own_biome = world.map.biome_at(tile_x as usize, tile_y as usize);
    if matches!(own_biome, Water | DeepWater) { return 0.0; }

    // For each of our 8 neighbor tiles that's water, compute the
    // distance from this pixel to the nearest point on that neighbor
    // tile's rectangle. The minimum across all water neighbors is the
    // pixel's distance to the nearest water boundary.
    let mut min_d = f32::INFINITY;
    for ny in -1i32..=1 {
        for nx in -1i32..=1 {
            if nx == 0 && ny == 0 { continue; }
            let bx = tile_x + nx;
            let by = tile_y + ny;
            if bx < 0 || by < 0 || bx >= WORLD_W as i32 || by >= WORLD_H as i32 { continue; }
            if !matches!(world.map.biome_at(bx as usize, by as usize), Water | DeepWater) {
                continue;
            }
            // Neighbor tile occupies rect [bx*TILE_PX, (bx+1)*TILE_PX] × same for y.
            let n_left   = bx as f32 * TILE_PX;
            let n_right  = n_left + TILE_PX;
            let n_top    = by as f32 * TILE_PX;
            let n_bottom = n_top  + TILE_PX;
            // Closest point on the rectangle to (px, py):
            let cx = px.clamp(n_left, n_right);
            let cy = py.clamp(n_top,  n_bottom);
            let dx = px - cx;
            let dy = py - cy;
            let d = (dx * dx + dy * dy).sqrt();
            if d < min_d { min_d = d; }
        }
    }
    min_d
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
                    if nx < 0 || ny < 0 || nx >= w as i32 || ny >= h as i32 { continue; }
                    sum += input[ny as usize * w + nx as usize];
                    count += 1;
                }
            }
            out[y * w + x] = sum / count as f32;
        }
    }
    out
}

fn sample_bilinear(buf: &[f32], w: usize, h: usize, x: f32, y: f32) -> f32 {
    let x = x.clamp(0.0, (w - 1) as f32);
    let y = y.clamp(0.0, (h - 1) as f32);
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(w - 1);
    let y1 = (y0 + 1).min(h - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let v00 = buf[y0 * w + x0];
    let v10 = buf[y0 * w + x1];
    let v01 = buf[y1 * w + x0];
    let v11 = buf[y1 * w + x1];
    let a = v00 + (v10 - v00) * fx;
    let b = v01 + (v11 - v01) * fx;
    a + (b - a) * fy
}
