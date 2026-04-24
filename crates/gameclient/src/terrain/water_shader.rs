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
use crate::terrain::world::{WorldGrid, TILE_PX, WORLD_W, WORLD_H};

pub struct WaterShaderPlugin;

impl Plugin for WaterShaderPlugin {
    fn build(&self, app: &mut App) {
        app
            .add_plugins(Material2dPlugin::<WaterMaterial>::default())
            .add_systems(
                Update,
                (toggle_and_manage, update_time).run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct WaterMaterial {
    /// `time` in seconds, plus 12 bytes of pad so the uniform block
    /// matches the WGSL struct's 16-byte alignment.
    #[uniform(0)]
    pub params: WaterParams,
    #[texture(1)]
    #[sampler(2)]
    pub mask: Handle<Image>,
}

#[derive(bevy::render::render_resource::ShaderType, Clone, Copy, Debug)]
pub struct WaterParams {
    pub time: f32,
    pub _pad0: f32,
    pub _pad1: f32,
    pub _pad2: f32,
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

    // Build the water-mask texture: 100×80 R8, 1 on water tiles.
    let mut mask_bytes = vec![0u8; WORLD_W * WORLD_H];
    for y in 0..WORLD_H {
        for x in 0..WORLD_W {
            if matches!(
                world.map.biome_at(x, y),
                questlib::mapgen::Biome::Water | questlib::mapgen::Biome::DeepWater
            ) {
                mask_bytes[y * WORLD_W + x] = 255;
            }
        }
    }
    let mut mask_img = Image::new(
        Extent3d { width: WORLD_W as u32, height: WORLD_H as u32, depth_or_array_layers: 1 },
        TextureDimension::D2,
        mask_bytes,
        TextureFormat::R8Unorm,
        bevy::render::render_asset::RenderAssetUsages::all(),
    );
    // Nearest filtering so water/land boundary stays crisp rather than
    // bleeding the shimmer onto shoreline land pixels.
    mask_img.sampler = bevy::image::ImageSampler::nearest();
    let mask_handle = images.add(mask_img);

    let material = materials.add(WaterMaterial {
        params: WaterParams { time: 0.0, _pad0: 0.0, _pad1: 0.0, _pad2: 0.0 },
        mask: mask_handle,
    });

    // World rectangle in pixel space, matching the ground sprite's
    // transform convention (tile (0,0) centered at world origin).
    let w = WORLD_W as f32 * TILE_PX;
    let h = WORLD_H as f32 * TILE_PX;
    let cx = w / 2.0 - TILE_PX / 2.0;
    let cy = -h / 2.0 + TILE_PX / 2.0;
    let mesh_handle = meshes.add(Rectangle::new(w, h));

    commands.spawn((
        Mesh2d(mesh_handle),
        MeshMaterial2d(material),
        // z=0.15 puts it above ground (0) and the lighting overlay
        // (0.3 — actually below lighting so lighting can darken it;
        // let me put it at 0.35 so it appears on top of darkness).
        Transform::from_xyz(cx, cy, 0.35),
        WaterShaderSprite,
    ));
}

/// Push current time into every WaterMaterial's uniform.
fn update_time(
    time: Res<Time>,
    mut materials: ResMut<Assets<WaterMaterial>>,
    q: Query<&MeshMaterial2d<WaterMaterial>>,
) {
    let t = time.elapsed_secs();
    for handle in &q {
        if let Some(mat) = materials.get_mut(&handle.0) {
            mat.params.time = t;
        }
    }
}
