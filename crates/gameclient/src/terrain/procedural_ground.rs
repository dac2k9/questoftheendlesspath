//! Procedural ground rendering — Phase 1 prototype.
//!
//! Replaces the tile-atlas blit with a single Material2d shader pass
//! that bilinearly blends per-biome flat colors based on the 4
//! nearest tile-center biome IDs. Smooth transitions between biomes
//! at every tile boundary, no hand-crafted transition tiles needed.
//!
//! Toggled via F4 (DebugOptions.procedural_terrain_enabled). When on,
//! a world-sized Mesh2d is spawned at z=0.05, which sits above the
//! existing tile atlas (z=0) and below the lighting overlay (z=0.3),
//! visually replacing the tilemap. The tile atlas keeps rendering
//! underneath but is fully obscured.
//!
//! Phase 2 expansion ideas (parked): per-biome procedural noise
//! textures, separate road mask channel, river stylization.

use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::mesh::{Indices, PrimitiveTopology};
use bevy::render::render_asset::RenderAssetUsages;
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, ShaderRef, TextureDimension, TextureFormat,
};
use bevy::sprite::{AlphaMode2d, Material2d, Material2dPlugin};

use crate::states::AppState;
use crate::terrain::world::{WorldGrid, TILE_PX, world_h, world_w};

/// Returns 0 — sprites no longer offset their world Y. Kept as a
/// stable callable so the existing call sites in tilemap.rs don't
/// need a code change when we toggle Y-lift back on later (just
/// change this body to return `biome_height_factor * LIFT_PX`).
pub fn tile_lift(_world: &WorldGrid, _x: usize, _y: usize) -> f32 {
    0.0
}

pub struct ProceduralGroundPlugin;

impl Plugin for ProceduralGroundPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<GroundMaterial>::default())
            .init_resource::<ProceduralGroundState>()
            .add_systems(
                Update,
                (toggle_key, tune_tile_z, toggle_and_manage, update_lighting)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Tracks the test-mode flag and the live PgUp/PgDn-tunable Z factor
/// at the time of last spawn so we can detect changes (F5 press,
/// PgUp/PgDn keypress) and despawn → respawn with new mesh data.
#[derive(Resource, Default)]
struct ProceduralGroundState {
    spawned_test_mode: bool,
    spawned_z_factor: f32,
}

/// PageUp / PageDown adjust the tile-Z factor live by 0.05 per press,
/// clamped to [0, 5]. The factor multiplies each vertex's biome
/// height-factor (0..1) to set its Z position in world units. In a
/// pure 2D ortho view this only affects depth ordering vs other
/// layers; it becomes visually meaningful once we go to a tilted /
/// 3D camera.
fn tune_tile_z(
    keys: Res<ButtonInput<KeyCode>>,
    mut debug: ResMut<super::tilemap::DebugOptions>,
) {
    if keys.just_pressed(KeyCode::PageUp) {
        debug.tile_z_factor = (debug.tile_z_factor + 0.05).min(5.0);
    }
    if keys.just_pressed(KeyCode::PageDown) {
        debug.tile_z_factor = (debug.tile_z_factor - 0.05).max(0.0);
    }
}

/// The world tilemap baked WITHOUT overlay sprites (no trees, rocks,
/// etc.) — just the biome ground. Used by the procedural shader for
/// sampling at jittered UVs so tile-edge borders don't drag in
/// silhouette pixels from a neighboring tree.
#[derive(Resource)]
pub struct BakedGroundTexture(pub Handle<Image>);

/// Overlays (trees / rocks / chests / etc.) on a transparent
/// background. Composited back on top of the jittered ground at the
/// fragment's UN-shifted UV so overlays remain anchored to their
/// actual tile while the ground underneath mixes with neighbors.
#[derive(Resource)]
pub struct BakedOverlaysTexture(pub Handle<Image>);

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct GroundMaterial {
    #[uniform(0)]
    pub params: GroundParams,
    #[texture(1)]
    #[sampler(2)]
    pub biome_tex: Handle<Image>,
    #[texture(3)]
    #[sampler(4)]
    pub ground_tex: Handle<Image>,
    #[texture(5)]
    #[sampler(6)]
    pub overlays_tex: Handle<Image>,
    /// 100×80 R8 blurred heightmap. Sampled per-fragment by the Phong
    /// pass to derive per-pixel normals.
    #[texture(7)]
    #[sampler(8)]
    pub heightmap: Handle<Image>,
    /// 1600×1280 R8 distance-to-water field, used for the shoreline
    /// bevel.
    #[texture(9)]
    #[sampler(10)]
    pub water_dist: Handle<Image>,
}

#[derive(bevy::render::render_resource::ShaderType, Clone, Copy, Debug, Default)]
pub struct GroundParams {
    pub world_w: f32,
    pub world_h: f32,
    pub tile_px: f32,
    /// 1.0 → flat-color rendering for the F5 test grid (each biome
    /// gets a fixed color; baked map sampling is bypassed). 0.0 →
    /// normal rendering using the actual tilemap content.
    pub test_mode: f32,
    /// xy = sun world px, z = sun height in world px, w unused.
    pub sun_pos: Vec4,
    /// xyz = sun-side highlight tint (warm at noon, amber at dusk,
    /// cool blue at midnight). w unused.
    pub sun_tint: Vec4,
    /// 1.0 → run the Phong pass; 0.0 → output bare biome colors.
    /// Driven by `debug.lighting_enabled` (F6).
    pub lighting_enabled: f32,
    /// Multiplier on the heightmap gradient → normal. Linked to
    /// `tile_z_factor` so PgUp/PgDn drives both polygon Z and shading
    /// strength from one knob.
    pub height_amp: f32,
    /// Phong ambient floor — darkest in-shade pixel never goes below.
    pub ambient: f32,
    /// Peak alpha applied on the dark side; highlights use ~0.6× this.
    pub max_alpha: f32,
    /// 1.0 → render the per-pixel normal as RGB (F9 debug).
    pub show_normals: f32,
    /// 1.0 → render the raw heightmap as grayscale (F10 debug).
    pub show_heightmap: f32,
    pub _pad0: f32,
    pub _pad1: f32,
}

impl Material2d for GroundMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/procedural_ground.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Opaque
    }
}

#[derive(Component)]
struct ProceduralGroundSprite;

fn toggle_key(
    keys: Res<ButtonInput<KeyCode>>,
    mut debug: ResMut<super::tilemap::DebugOptions>,
) {
    if keys.just_pressed(KeyCode::F4) {
        debug.procedural_terrain_enabled = !debug.procedural_terrain_enabled;
    }
    if keys.just_pressed(KeyCode::F5) {
        debug.procedural_test_mode = !debug.procedural_test_mode;
    }
}

fn toggle_and_manage(
    mut commands: Commands,
    debug: Res<super::tilemap::DebugOptions>,
    mut state: ResMut<ProceduralGroundState>,
    world: Option<Res<WorldGrid>>,
    ground: Option<Res<BakedGroundTexture>>,
    overlays: Option<Res<BakedOverlaysTexture>>,
    mut images: ResMut<Assets<Image>>,
    mut materials: ResMut<Assets<GroundMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
    existing: Query<Entity, With<ProceduralGroundSprite>>,
) {
    let want = debug.procedural_terrain_enabled;
    let has = !existing.is_empty();
    let test_mode_changed = has && state.spawned_test_mode != debug.procedural_test_mode;
    let z_factor_changed = has
        && (state.spawned_z_factor - debug.tile_z_factor).abs() > 1e-4;

    // F5 (test mode) or PgUp/PgDn (z factor) changes mean the mesh
    // needs to be rebuilt — despawn so the next pass spawns fresh.
    if test_mode_changed || z_factor_changed {
        for e in &existing {
            commands.entity(e).despawn_recursive();
        }
        return;
    }
    if want == has { return; }

    if !want {
        for e in &existing {
            commands.entity(e).despawn_recursive();
        }
        return;
    }

    let Some(world) = world else { return };
    let Some(ground) = ground else { return };
    let Some(overlays) = overlays else { return };

    let biome_tex = images.add(if debug.procedural_test_mode {
        generate_test_biome_texture()
    } else {
        generate_biome_texture(&world)
    });
    // Heightmap + water-distance for the in-shader Phong pass — same
    // bakes that the (now retired) terrain_lighting.rs overlay used.
    let heightmap = images.add(generate_heightmap(&world));
    let water_dist = images.add(generate_water_distance(&world));
    let material = materials.add(GroundMaterial {
        params: GroundParams {
            // Read dims off the WorldGrid Resource (authoritative)
            // rather than the world_w()/world_h() atomics — the
            // material's world_w/world_h are baked in at creation and
            // never re-uploaded, so a stale-atomic read here pinned
            // the shader to 100×80 for the whole session. Symptom:
            // the 200×160 chaos world rendered with a hard lighting
            // boundary halfway across (the sun position + biome
            // sampling both clamp on these uniforms).
            world_w: world.width as f32,
            world_h: world.height as f32,
            tile_px: TILE_PX,
            test_mode: if debug.procedural_test_mode { 1.0 } else { 0.0 },
            // Lighting fields filled in each frame by `update_lighting`;
            // sane initial values so the first frame renders correctly.
            sun_pos: Vec4::new(-10_000.0, -10_000.0, 8_000.0, 0.0),
            sun_tint: Vec4::new(1.0, 0.95, 0.80, 0.0),
            lighting_enabled: if debug.lighting_enabled { 1.0 } else { 0.0 },
            height_amp: 0.0,
            ambient: 0.30,
            max_alpha: 0.45,
            show_normals: 0.0,
            show_heightmap: 0.0,
            _pad0: 0.0,
            _pad1: 0.0,
        },
        biome_tex,
        ground_tex: ground.0.clone(),
        overlays_tex: overlays.0.clone(),
        heightmap,
        water_dist,
    });
    state.spawned_test_mode = debug.procedural_test_mode;
    state.spawned_z_factor = debug.tile_z_factor;

    // Build a 100×80 quad grid (16000 tris, 8181 vertices) instead of
    // a single rectangle. Each vertex carries a Z derived from the
    // max-of-4-neighbors biome height × the live tile_z_factor knob
    // (PgUp/PgDn). In top-down ortho the Z only changes depth-order;
    // it becomes visually meaningful when the camera tilts.
    //
    // Test mode (F5) zeroes the Z so the autotile layout reads
    // unambiguously without depth ordering getting in the way.
    let z_factor = if debug.procedural_test_mode { 0.0 } else { debug.tile_z_factor };
    let mesh_handle = meshes.add(build_tile_mesh(&world, z_factor));

    commands.spawn((
        Mesh2d(mesh_handle),
        MeshMaterial2d(material),
        // Vertices are in absolute world coords, so transform sits at
        // origin. z=0.05 keeps it above the tile atlas, below lighting.
        Transform::from_xyz(0.0, 0.0, 0.05),
        ProceduralGroundSprite,
    ));
}

/// Per-biome height factor, 0..1. Same table as
/// `terrain_lighting::biome_height` so vertex lift and Phong lighting
/// agree on which tiles are "tall". Mountain peaks at 1.0.
fn biome_height_factor(world: &WorldGrid, x: usize, y: usize) -> f32 {
    use questlib::mapgen::Biome::*;
    match world.map.biome_at(x, y) {
        Water | DeepWater => 0.00,
        Swamp => 0.15,
        Desert => 0.25,
        Grassland => 0.40,
        Forest => 0.50,
        DenseForest => 0.55,
        Mountain => 1.00,
        Snow => 0.85,
    }
}

/// Build the world ground as a quad grid: (W+1)×(H+1) shared vertices,
/// W×H quads, two triangles per quad. Each vertex's Z = `z_factor ×
/// max(neighbor_heights)` so taller biomes (mountain, snow, dense
/// forest) sit higher on the implicit Z axis. In a top-down ortho
/// camera this only reorders depth; once the camera tilts (or if a
/// future shader reads vertex Z and offsets screen-y), the geometry
/// reads as 3D relief.
///
/// UVs run 0..1 across the world rectangle (matching the previous
/// single-quad mesh) so the autotile fragment shader is unchanged.
fn build_tile_mesh(world: &WorldGrid, z_factor: f32) -> Mesh {
    let w = world_w();
    let h = world_h();
    let nx = w + 1; // 101 vertices wide
    let ny = h + 1; // 81 vertices tall

    // Per-tile heights, indexed [ty * w + tx].
    let mut heights = vec![0.0_f32; w * h];
    for ty in 0..h {
        for tx in 0..w {
            heights[ty * w + tx] = biome_height_factor(world, tx, ty);
        }
    }

    let mut positions = Vec::with_capacity(nx * ny);
    let mut uvs = Vec::with_capacity(nx * ny);
    let mut normals = Vec::with_capacity(nx * ny);
    for vy in 0..ny {
        for vx in 0..nx {
            // Each interior vertex is shared by up to 4 tiles. Sample
            // the heightmap by averaging those neighbors so biome
            // boundaries fade smoothly across the corner instead of
            // letting a single tall neighbor pull the whole corner up.
            // Edge-of-world vertices average over fewer samples, which
            // is fine — those tiles are off-map anyway.
            let mut h_sum = 0.0_f32;
            let mut h_count = 0;
            for &dx in &[-1_i32, 0] {
                for &dy in &[-1_i32, 0] {
                    let tx = vx as i32 + dx;
                    let ty = vy as i32 + dy;
                    if tx >= 0 && ty >= 0 && (tx as usize) < w && (ty as usize) < h {
                        h_sum += heights[(ty as usize) * w + (tx as usize)];
                        h_count += 1;
                    }
                }
            }
            let h_avg = if h_count > 0 { h_sum / (h_count as f32) } else { 0.0 };
            // Tile (tx, ty) has its CENTER at world (tx*TILE_PX, -ty*TILE_PX);
            // corner (vx, vy) sits at the half-tile offset that joins the
            // four surrounding tile centers.
            let world_x = (vx as f32) * TILE_PX - TILE_PX * 0.5;
            let world_y = -(vy as f32) * TILE_PX + TILE_PX * 0.5;
            positions.push([world_x, world_y, h_avg * z_factor]);
            uvs.push([(vx as f32) / (w as f32), (vy as f32) / (h as f32)]);
            normals.push([0.0, 0.0, 1.0]);
        }
    }

    // Two triangles per quad. Winding is CCW in screen space (Bevy
    // default front face) so the mesh isn't culled.
    let mut indices = Vec::with_capacity(w * h * 6);
    for ty in 0..h {
        for tx in 0..w {
            let i_tl = (ty * nx + tx) as u32;
            let i_tr = (ty * nx + tx + 1) as u32;
            let i_bl = ((ty + 1) * nx + tx) as u32;
            let i_br = ((ty + 1) * nx + tx + 1) as u32;
            indices.extend_from_slice(&[i_tl, i_bl, i_tr]);
            indices.extend_from_slice(&[i_tr, i_bl, i_br]);
        }
    }

    let mut mesh = Mesh::new(PrimitiveTopology::TriangleList, RenderAssetUsages::all());
    mesh.insert_attribute(Mesh::ATTRIBUTE_POSITION, positions);
    mesh.insert_attribute(Mesh::ATTRIBUTE_UV_0, uvs);
    mesh.insert_attribute(Mesh::ATTRIBUTE_NORMAL, normals);
    mesh.insert_indices(Indices::U32(indices));
    mesh
}

/// Bake a 100×80 R8Unorm texture where each texel holds the biome ID
/// (as the integer index into `Biome` enum, encoded as `id / 255`).
/// Sampler is NEAREST so the shader never sees fractional IDs.
fn generate_biome_texture(world: &WorldGrid) -> Image {
    use questlib::mapgen::Biome::*;
    let w = world_w();
    let h = world_h();
    let mut data = Vec::with_capacity(w * h);
    for y in 0..h {
        for x in 0..w {
            // Keep this match in sync with biome_color() in
            // procedural_ground.wgsl. Adding a new biome means
            // editing both sides.
            let id: u8 = match world.map.biome_at(x, y) {
                Water => 0,
                DeepWater => 1,
                Swamp => 2,
                Desert => 3,
                Grassland => 4,
                Forest => 5,
                DenseForest => 6,
                Mountain => 7,
                Snow => 8,
            };
            data.push(id);
        }
    }
    let mut img = Image::new(
        Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::R8Unorm,
        bevy::render::render_asset::RenderAssetUsages::all(),
    );
    use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Nearest,
        ..Default::default()
    });
    img
}

/// Synthetic biome layout for the F5 test grid.
///
/// Lays out a series of test patterns in a regular grid, each 9 tiles
/// wide × 9 tiles tall. Each row exercises a different "shape" (single
/// tile, 2-strip, 2×2 cluster, longer column, etc.) and each column
/// uses a different biome combination (feature in grass, feature in
/// water, feature in sand, etc.). The first 4 rows have GRASS as the
/// background, the last 3 rows have alternative backgrounds so we can
/// see how the surrounding biome affects rendering.
fn generate_test_biome_texture() -> Image {
    const G: u8 = 4;  // Grassland
    const W: u8 = 0;  // Water
    const M: u8 = 2;  // Swamp / marsh
    const S: u8 = 3;  // Desert (sand-like, used as "road")
    const N: u8 = 8;  // Snow (used as "ice")

    let w = world_w();
    let h = world_h();
    let mut data = vec![G; w * h];

    let put = |data: &mut Vec<u8>, x: i32, y: i32, b: u8| {
        if x >= 0 && y >= 0 && (x as usize) < w && (y as usize) < h {
            data[(y as usize) * w + (x as usize)] = b;
        }
    };
    let fill = |data: &mut Vec<u8>, x0: usize, y0: usize, dx: usize, dy: usize, b: u8| {
        for y in y0..(y0 + dy).min(h) {
            for x in x0..(x0 + dx).min(w) {
                data[y * w + x] = b;
            }
        }
    };

    // Each test cell is 9×9. Lay out a 9-col × 8-row grid of cells in
    // a 81×72 area starting at (2, 2), well inside the 100×80 world.
    const CELL: i32 = 9;
    const ORIGIN_X: i32 = 2;
    const ORIGIN_Y: i32 = 2;

    // Helper: place a feature at the center of cell (col, row), given
    // a list of relative offsets describing the feature's tiles.
    let mut place = |data: &mut Vec<u8>, col: i32, row: i32, biome: u8, tiles: &[(i32, i32)]| {
        let cx = ORIGIN_X + col * CELL + CELL / 2;
        let cy = ORIGIN_Y + row * CELL + CELL / 2;
        for &(dx, dy) in tiles {
            put(data, cx + dx, cy + dy, biome);
        }
    };

    // Define the shapes we want to test (relative to cell center).
    let single: &[(i32, i32)] = &[(0, 0)];
    let strip2_h: &[(i32, i32)] = &[(0, 0), (1, 0)];
    let strip2_v: &[(i32, i32)] = &[(0, 0), (0, 1)];
    let cluster2x2: &[(i32, i32)] = &[(0, 0), (1, 0), (0, 1), (1, 1)];
    let column3: &[(i32, i32)] = &[(0, -1), (0, 0), (0, 1)];
    let l_shape: &[(i32, i32)] = &[(0, -1), (0, 0), (1, 0)];
    let plus: &[(i32, i32)] = &[(0, -1), (-1, 0), (0, 0), (1, 0), (0, 1)];

    // Rows 0–3: GRASS background (default), feature in cols.
    // Cols: 0=Water, 1=Sand, 2=Snow.
    let features = [W, S, N];
    let shapes_grass: [(&str, &[(i32, i32)]); 4] = [
        ("single",     single),
        ("strip 2h",   strip2_h),
        ("2×2",        cluster2x2),
        ("column 3v",  column3),
    ];
    for (row, (_label, shape)) in shapes_grass.iter().enumerate() {
        for (col, &biome) in features.iter().enumerate() {
            place(&mut data, col as i32, row as i32, biome, shape);
        }
    }

    // Cols 3+ in rows 0–3: more complex shapes.
    let extra_shapes: [(&str, &[(i32, i32)]); 3] = [
        ("L-shape",    l_shape),
        ("plus",       plus),
        ("strip 2v",   strip2_v),
    ];
    for (col_offset, (_label, shape)) in extra_shapes.iter().enumerate() {
        for (row, &biome) in features.iter().enumerate() {
            place(&mut data, (3 + col_offset) as i32, row as i32, biome, shape);
        }
    }

    // Rows 4–7: alternative backgrounds. Fill each row's cell band
    // with a non-grass background, then place GRASS as the feature
    // (so we can see "grass island in water", etc.) in some cols, and
    // other biome features in others.

    // Row 4: WATER background, sand/snow/grass features.
    let row4_y = ORIGIN_Y + 4 * CELL;
    fill(&mut data, ORIGIN_X as usize, row4_y as usize, (6 * CELL) as usize, CELL as usize, W);
    place(&mut data, 0, 4, S, single);
    place(&mut data, 1, 4, N, single);
    place(&mut data, 2, 4, G, single);
    place(&mut data, 3, 4, S, strip2_v);
    place(&mut data, 4, 4, N, strip2_v);
    place(&mut data, 5, 4, G, strip2_v);

    // Row 5: SAND background, water/snow/grass features (image 51 case).
    let row5_y = ORIGIN_Y + 5 * CELL;
    fill(&mut data, ORIGIN_X as usize, row5_y as usize, (6 * CELL) as usize, CELL as usize, S);
    place(&mut data, 0, 5, W, single);
    place(&mut data, 1, 5, N, single);
    place(&mut data, 2, 5, G, single);
    place(&mut data, 3, 5, W, column3);
    place(&mut data, 4, 5, N, column3);   // <-- the user's "ice column in road" case
    place(&mut data, 5, 5, G, column3);

    // Row 6: SNOW background, water/sand/grass features.
    let row6_y = ORIGIN_Y + 6 * CELL;
    fill(&mut data, ORIGIN_X as usize, row6_y as usize, (6 * CELL) as usize, CELL as usize, N);
    place(&mut data, 0, 6, W, single);
    place(&mut data, 1, 6, S, single);
    place(&mut data, 2, 6, G, single);
    place(&mut data, 3, 6, W, cluster2x2);
    place(&mut data, 4, 6, S, cluster2x2);
    place(&mut data, 5, 6, G, cluster2x2);

    // Row 7: SWAMP coverage. Verifies swamp behaves like water.
    //   Cols 0-2: swamp features in grass background.
    //   Cols 3-5: swamp BACKGROUND with grass / sand / snow features.
    place(&mut data, 0, 7, M, single);
    place(&mut data, 1, 7, M, strip2_h);
    place(&mut data, 2, 7, M, cluster2x2);
    let row7_y = ORIGIN_Y + 7 * CELL;
    fill(&mut data, (ORIGIN_X + 3 * CELL) as usize, row7_y as usize, (3 * CELL) as usize, CELL as usize, M);
    place(&mut data, 3, 7, G, single);
    place(&mut data, 4, 7, S, single);
    place(&mut data, 5, 7, N, single);

    let mut img = Image::new(
        Extent3d { width: w as u32, height: h as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        data,
        TextureFormat::R8Unorm,
        bevy::render::render_asset::RenderAssetUsages::all(),
    );
    use bevy::image::{ImageAddressMode, ImageFilterMode, ImageSampler, ImageSamplerDescriptor};
    img.sampler = ImageSampler::Descriptor(ImageSamplerDescriptor {
        address_mode_u: ImageAddressMode::ClampToEdge,
        address_mode_v: ImageAddressMode::ClampToEdge,
        mag_filter: ImageFilterMode::Nearest,
        min_filter: ImageFilterMode::Nearest,
        ..Default::default()
    });
    img
}

// ── Lighting update + heightmap bakes ────────────────────────────────
//
// Phong is now baked into procedural_ground.wgsl directly so the
// ground mesh shades itself from its own polygon-derived heightmap.
// terrain_lighting.rs's separate overlay is no longer added to the
// app — F6 still flips `debug.lighting_enabled`, which the in-shader
// Phong pass reads via the `lighting_enabled` uniform.
//
// `tile_z_factor` (PgUp/PgDn) drives BOTH polygon Z displacement and
// the lighting's gradient amplitude — a single knob for "how mountainy
// does this look", and the slope shading scales 1:1 with the geometry.

/// Push the live sun position, sky tint, and tunable knobs into the
/// ground material every frame. F8 debug sun overrides the day/night
/// cycle, same as the water shader.
fn update_lighting(
    debug: Res<super::tilemap::DebugOptions>,
    cycle: Res<crate::daynight::DayNightCycle>,
    world: Option<Res<WorldGrid>>,
    q: Query<&MeshMaterial2d<GroundMaterial>>,
    mut materials: ResMut<Assets<GroundMaterial>>,
) {
    let sun_pos = if debug.debug_sun_enabled {
        Vec4::new(debug.debug_sun_x, debug.debug_sun_y, debug.debug_sun_z, 0.0)
    } else {
        // Center the sun arc on the actual world centre. Pull dims
        // from the WorldGrid Resource instead of the atomic getters
        // so a stale-atomic frame doesn't fling the sun to the
        // upper-left quadrant of the chaos world.
        let (w, h) = world
            .as_ref()
            .map(|wg| (wg.width as f32 * TILE_PX, wg.height as f32 * TILE_PX))
            .unwrap_or((world_w() as f32 * TILE_PX, world_h() as f32 * TILE_PX));
        let center = Vec2::new(w / 2.0, -h / 2.0);
        let p = cycle.light_pos(center);
        Vec4::new(p.x, p.y, p.z, 0.0)
    };
    let tint = cycle.sky_tint();
    let sun_tint = Vec4::new(tint.x, tint.y, tint.z, 0.0);
    // Lighting amp is independent from `tile_z_factor` — the heightmap
    // is sampled regardless of polygon Z displacement, so the world
    // gets the same Phong shading whether or not the geometry is
    // actually raised. (Linking them earlier meant `tile_z_factor = 0`
    // killed all shading, which read as a regression.) PgUp/PgDn
    // controls *only* polygon Z now; if you want to also crank the
    // lighting amp, edit `terrain_height_amp` (default 80).
    let height_amp = debug.terrain_height_amp;
    let lighting_enabled = if debug.lighting_enabled { 1.0 } else { 0.0 };
    let show_normals = if debug.debug_show_normals { 1.0 } else { 0.0 };
    let show_heightmap = if debug.debug_show_heightmap { 1.0 } else { 0.0 };
    for handle in &q {
        if let Some(mat) = materials.get_mut(&handle.0) {
            mat.params.sun_pos = sun_pos;
            mat.params.sun_tint = sun_tint;
            mat.params.height_amp = height_amp;
            mat.params.lighting_enabled = lighting_enabled;
            mat.params.show_normals = show_normals;
            mat.params.show_heightmap = show_heightmap;
        }
    }
}

/// Per-biome height in [0, 1]. Mirrors the table the old
/// `terrain_lighting::biome_height` function used so previous shading
/// looks identical.
fn lighting_biome_height(world: &WorldGrid, x: usize, y: usize) -> f32 {
    use questlib::mapgen::Biome::*;
    let base = match world.map.biome_at(x, y) {
        Water | DeepWater => 0.00,
        Swamp => 0.15,
        Desert => 0.25,
        Grassland => 0.40,
        Forest => 0.50,
        DenseForest => 0.55,
        Mountain => 1.00,
        Snow => 0.85,
    };
    if world.map.has_road_at(x, y) {
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

/// Bake a 100×80 R8 heightmap (3× blurred) for the Phong pass.
fn generate_heightmap(world: &WorldGrid) -> Image {
    let w = world_w();
    let h = world_h();
    let mut height = vec![0.0_f32; w * h];
    for y in 0..h {
        for x in 0..w {
            height[y * w + x] = lighting_biome_height(world, x, y);
        }
    }
    let mut blurred = height.clone();
    for _ in 0..3 {
        blurred = box_blur_3x3(&blurred, w, h);
    }
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

/// Bake a per-pixel distance-to-water R8 texture (1600×1280).
fn generate_water_distance(world: &WorldGrid) -> Image {
    use questlib::mapgen::Biome::*;
    let pw = world_w() * TILE_PX as usize;
    let ph = world_h() * TILE_PX as usize;
    let mut data = Vec::with_capacity(pw * ph);
    for py in 0..ph {
        for px in 0..pw {
            let fx = px as f32 + 0.5;
            let fy = py as f32 + 0.5;
            let tile_x = (fx / TILE_PX).floor() as i32;
            let tile_y = (fy / TILE_PX).floor() as i32;
            let in_world = tile_x >= 0 && tile_y >= 0
                && tile_x < world_w() as i32 && tile_y < world_h() as i32;
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
                    if bx < 0 || by < 0 || bx >= world_w() as i32 || by >= world_h() as i32 {
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
