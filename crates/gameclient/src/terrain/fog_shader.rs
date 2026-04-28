//! Fog of war material — see `assets/shaders/fog.wgsl`.
//!
//! Replaces the old per-pixel CPU-baked fog texture (1600×1280 RGBA,
//! one byte per pixel, hard tile-edged) with a tiny 100×80 R8 mask
//! sampled by the GPU with linear filter. The interpolation gives a
//! smooth half-tile fade at every revealed/unrevealed boundary at
//! ~no cost — and the per-frame update only writes 8000 bytes
//! instead of 8 MB.
//!
//! tilemap.rs still owns spawning the fog sprite and updating the
//! mask when `FogOfWar.dirty` flips; this module just registers the
//! material so the shader can be used as a Material2d.
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, ShaderRef};
use bevy::sprite::{AlphaMode2d, Material2d, Material2dPlugin};

pub struct FogShaderPlugin;

impl Plugin for FogShaderPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<FogMaterial>::default());
    }
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct FogMaterial {
    #[uniform(0)]
    pub params: FogParams,
    #[texture(1)]
    #[sampler(2)]
    pub mask: Handle<Image>,
}

#[derive(bevy::render::render_resource::ShaderType, Clone, Copy, Debug)]
pub struct FogParams {
    /// rgb = fog color, w = max alpha (1.0 = fully opaque fogged tiles).
    pub color: Vec4,
    /// Ratio of fog mesh size to world size. The fog mesh is bigger
    /// than the world so that when the camera zooms out, fog still
    /// covers the area outside the world rectangle (instead of
    /// showing the camera's ClearColor).
    pub world_scale: f32,
    pub _pad0: f32,
    pub _pad1: f32,
    pub _pad2: f32,
}

impl Material2d for FogMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/fog.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}
