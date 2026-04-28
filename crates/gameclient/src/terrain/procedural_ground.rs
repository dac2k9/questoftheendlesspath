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
use bevy::render::render_resource::{
    AsBindGroup, Extent3d, ShaderRef, TextureDimension, TextureFormat,
};
use bevy::sprite::{AlphaMode2d, Material2d, Material2dPlugin};

use crate::states::AppState;
use crate::terrain::world::{WorldGrid, TILE_PX, WORLD_H, WORLD_W};

pub struct ProceduralGroundPlugin;

impl Plugin for ProceduralGroundPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<GroundMaterial>::default())
            .init_resource::<ProceduralGroundState>()
            .add_systems(
                Update,
                (toggle_key, toggle_and_manage)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Tracks the test-mode flag at the time of last spawn so we can
/// detect a change (F5 press) and force despawn → respawn with new
/// biome data.
#[derive(Resource, Default)]
struct ProceduralGroundState {
    spawned_test_mode: bool,
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

    // If F5 was pressed while procedural is on, despawn so we re-spawn
    // with the new biome texture (test grid vs world).
    if test_mode_changed {
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
    let material = materials.add(GroundMaterial {
        params: GroundParams {
            world_w: WORLD_W as f32,
            world_h: WORLD_H as f32,
            tile_px: TILE_PX,
            test_mode: if debug.procedural_test_mode { 1.0 } else { 0.0 },
        },
        biome_tex,
        ground_tex: ground.0.clone(),
        overlays_tex: overlays.0.clone(),
    });
    state.spawned_test_mode = debug.procedural_test_mode;

    let w = WORLD_W as f32 * TILE_PX;
    let h = WORLD_H as f32 * TILE_PX;
    // Match the tile atlas's centering convention so world coords line
    // up exactly — sampler offsets in the shader assume this.
    let cx = w / 2.0 - TILE_PX / 2.0;
    let cy = -h / 2.0 + TILE_PX / 2.0;
    let mesh_handle = meshes.add(Rectangle::new(w, h));

    commands.spawn((
        Mesh2d(mesh_handle),
        MeshMaterial2d(material),
        Transform::from_xyz(cx, cy, 0.05),
        ProceduralGroundSprite,
    ));
}

/// Bake a 100×80 R8Unorm texture where each texel holds the biome ID
/// (as the integer index into `Biome` enum, encoded as `id / 255`).
/// Sampler is NEAREST so the shader never sees fractional IDs.
fn generate_biome_texture(world: &WorldGrid) -> Image {
    use questlib::mapgen::Biome::*;
    let w = WORLD_W;
    let h = WORLD_H;
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

    let w = WORLD_W;
    let h = WORLD_H;
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
