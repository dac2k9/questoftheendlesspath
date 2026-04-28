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

/// Sun direction (from surface → light source), NOT normalized here —
/// normalized at use. Upper-left, slightly above. Matches cloud shadow.
const SUN_X: f32 = -0.65;
const SUN_Y: f32 = -0.65;
const SUN_Z: f32 = 0.80;

/// Phong-style lighting constants.
/// ambient floor so the darkest fully-in-shade pixel doesn't go pitch black.
const AMBIENT: f32 = 0.30;
/// How many alpha steps to quantize into — bigger = smoother, smaller =
/// more stylized. 5 reads nicely against the pixel art.
const QUANT_STEPS: f32 = 5.0;
/// Peak darkness the overlay can apply at max shade.
const MAX_ALPHA: f32 = 0.40;

/// Shoreline bevel — the last N land pixels before a water tile's edge
/// get their normal interpolated from flat (0, 0, 1) at the bevel's
/// inland side to `(nx, ny, small_z)` at the water edge, creating a
/// curved slope. When the Phong shader evaluates this normal, the
/// surface reads as rolling down into the water.
const SHORELINE_BEVEL_WIDTH_PX: f32 = 5.0;
/// How far the normal tilts at the very water edge. 0.9 ≈ 64 ° off
/// vertical — a steep, almost-sideways facing so the shading reads
/// clearly. Lower values flatten the bevel.
const SHORELINE_TILT: f32 = 0.9;

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
    _world: Option<Res<WorldGrid>>,
    _images: ResMut<Assets<Image>>,
    existing: Query<Entity, With<LightingOverlay>>,
) {
    // F6 toggle still managed here so the keybind works, but the
    // actual overlay is now spawned by `terrain_lighting.rs` as a
    // GPU-lit Material2d that responds to the day/night cycle.
    // This function just handles the key press + cleans up any
    // leftover CPU overlay entities (shouldn't exist after the
    // switch, but harmless if they do).
    if keys.just_pressed(KeyCode::F6) {
        debug.lighting_enabled = !debug.lighting_enabled;
    }
    for e in &existing {
        commands.entity(e).despawn_recursive();
    }
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

    // Normalized sun (surface → light). Computed once and reused.
    let sun_len = (SUN_X * SUN_X + SUN_Y * SUN_Y + SUN_Z * SUN_Z).sqrt();
    let sun = [SUN_X / sun_len, SUN_Y / sun_len, SUN_Z / sun_len];
    // Baseline lighting for flat ground (normal = (0, 0, 1)) — the
    // amount we treat as "neutral, no darkening". Anything brighter than
    // this we clamp back to this (we never BRIGHTEN the pixel-art atlas,
    // only darken relative to it).
    let flat_lit = AMBIENT + (0.0 * sun[0] + 0.0 * sun[1] + 1.0 * sun[2]).max(0.0) * (1.0 - AMBIENT);

    // ── 3. Per-pixel pass: compute a normal (heightmap-derived for
    //      terrain slopes, plus a shoreline-bevel override for the last
    //      5 px of land before water), evaluate Phong diffuse, and
    //      output a darkness overlay.
    let pw = w * TILE_PX as usize;
    let ph = h * TILE_PX as usize;
    let mut data = Vec::with_capacity(pw * ph * 4);
    for py in 0..ph {
        let ty = py as f32 / TILE_PX;
        for px in 0..pw {
            let tx = px as f32 / TILE_PX;

            // ── Base normal from heightmap gradient ──
            // Central differences on the blurred heightmap give uphill
            // direction; normal = (-∂h/∂x, -∂h/∂y, z_scale), normalized.
            let hx1 = sample_bilinear(&blurred, w, h, tx - 0.5, ty);
            let hx2 = sample_bilinear(&blurred, w, h, tx + 0.5, ty);
            let hy1 = sample_bilinear(&blurred, w, h, tx, ty - 0.5);
            let hy2 = sample_bilinear(&blurred, w, h, tx, ty + 0.5);
            let dx = hx2 - hx1;
            let dy = hy2 - hy1;
            // z_scale makes the heightmap-driven tilt subtle — biome
            // height deltas are small, a scale of 4 gives visible
            // mountain shading without slamming the normal sideways.
            let mut n = normalize3(-dx * 4.0, -dy * 4.0, 1.0);

            // ── Shoreline bevel override ──
            // If this pixel is land AND close to water, override the
            // normal with a strong tilt toward the nearest water
            // boundary, interpolated by distance. Produces the "rolling
            // down into water" curve we want.
            let near = nearest_water_boundary(world, px as f32, py as f32);
            if let Some(hit) = near {
                if hit.dist_px > 0.0 && hit.dist_px < SHORELINE_BEVEL_WIDTH_PX {
                    // t: 0 at bevel's inland end, 1 at water edge.
                    let t = 1.0 - hit.dist_px / SHORELINE_BEVEL_WIDTH_PX;
                    // Tilt: at t=0 flat (0,0,1), at t=1 heavily tilted
                    // toward water by SHORELINE_TILT.
                    let tilt = SHORELINE_TILT * t;
                    let horiz_len = tilt;
                    let vert = (1.0 - tilt * tilt).max(0.0).sqrt();
                    // In Bevy's world space, Y flips — use image-space
                    // direction directly (water_dir was computed in
                    // image pixel coords; y+ = down on screen).
                    n = normalize3(
                        hit.dir_x * horiz_len,
                        hit.dir_y * horiz_len,
                        vert,
                    );
                }
            }

            // ── Phong diffuse ──
            let n_dot_l = (n[0] * sun[0] + n[1] * sun[1] + n[2] * sun[2]).max(0.0);
            let lit = AMBIENT + n_dot_l * (1.0 - AMBIENT);
            // Only darken — never brighten past flat ground. Keeps the
            // pixel-art atlas readable instead of washing it out.
            let mut shade = (flat_lit - lit).max(0.0);
            shade = (shade * QUANT_STEPS).floor() / QUANT_STEPS;
            let alpha_f = shade * MAX_ALPHA;
            let alpha = (alpha_f * 255.0).clamp(0.0, 255.0) as u8;

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

/// Result of looking for the closest water-tile boundary from a given
/// land pixel. Returns both distance and a unit vector pointing from
/// the pixel toward that boundary — the shoreline bevel needs the
/// direction to tilt the normal.
struct WaterHit {
    dist_px: f32,
    dir_x: f32,
    dir_y: f32,
}

/// Nearest water-tile boundary from a pixel position. Returns `None` if
/// the own tile is water (no bevel), or if no water tile lies within
/// 8-neighbor range (too far from shore — no bevel needed anyway).
fn nearest_water_boundary(world: &WorldGrid, px: f32, py: f32) -> Option<WaterHit> {
    use questlib::mapgen::Biome::*;
    let tile_x = (px / TILE_PX).floor() as i32;
    let tile_y = (py / TILE_PX).floor() as i32;
    if tile_x < 0 || tile_y < 0 || tile_x >= WORLD_W as i32 || tile_y >= WORLD_H as i32 {
        return None;
    }
    if matches!(world.map.biome_at(tile_x as usize, tile_y as usize), Water | DeepWater) {
        return None;
    }

    let mut best: Option<WaterHit> = None;
    for ny in -1i32..=1 {
        for nx in -1i32..=1 {
            if nx == 0 && ny == 0 { continue; }
            let bx = tile_x + nx;
            let by = tile_y + ny;
            if bx < 0 || by < 0 || bx >= WORLD_W as i32 || by >= WORLD_H as i32 { continue; }
            if !matches!(world.map.biome_at(bx as usize, by as usize), Water | DeepWater) {
                continue;
            }
            // Closest point on that neighbor tile's rectangle.
            let n_left   = bx as f32 * TILE_PX;
            let n_right  = n_left + TILE_PX;
            let n_top    = by as f32 * TILE_PX;
            let n_bottom = n_top  + TILE_PX;
            let cx = px.clamp(n_left, n_right);
            let cy = py.clamp(n_top,  n_bottom);
            let dx = cx - px; // from pixel → boundary
            let dy = cy - py;
            let d = (dx * dx + dy * dy).sqrt();
            let better = best.as_ref().map_or(true, |b| d < b.dist_px);
            if better {
                let (ux, uy) = if d > 1e-6 { (dx / d, dy / d) } else { (0.0, 0.0) };
                best = Some(WaterHit { dist_px: d, dir_x: ux, dir_y: uy });
            }
        }
    }
    best
}

fn normalize3(x: f32, y: f32, z: f32) -> [f32; 3] {
    let len = (x * x + y * y + z * z).sqrt().max(1e-6);
    [x / len, y / len, z / len]
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
