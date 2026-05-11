//! Fog of war material — see `assets/shaders/fog.wgsl`.
//!
//! Replaces the old per-pixel CPU-baked fog texture (1600×1280 RGBA,
//! one byte per pixel, hard tile-edged) with a tiny 100×80 R8 mask
//! sampled by the GPU with linear filter. The interpolation gives a
//! smooth half-tile fade at every revealed/unrevealed boundary at
//! ~no cost — and the per-frame update only writes 8000 bytes
//! instead of 8 MB.
//!
//! Also drives a subtle shadow effect — the shader samples the mask
//! a few pixels toward the sun and darkens revealed pixels that have
//! fog between them and the light source. The illusion: walls of
//! unexplored fog cast a soft shadow onto the explored ground in
//! front of them, shifting through the day with the sun's arc.
//!
//! tilemap.rs still owns spawning the fog sprite and updating the
//! mask when `FogOfWar.dirty` flips; this module registers the
//! material so the shader can be used as a Material2d, and runs a
//! per-frame system that pushes the current sun position into the
//! material uniform.
use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, ShaderRef};
use bevy::sprite::{AlphaMode2d, Material2d, Material2dPlugin};

use crate::states::AppState;

pub struct FogShaderPlugin;

impl Plugin for FogShaderPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<FogMaterial>::default())
            .add_systems(
                Update,
                update_fog_material.run_if(in_state(AppState::InGame)),
            );
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
    /// rgb = fog color, a = max alpha (1.0 = fully opaque fogged tiles).
    pub color: Vec4,
    /// Sun position in world pixel coords. xy = world px (matches the
    /// space `DayNightCycle::light_pos` returns), z = sun elevation
    /// (>0 above horizon, <0 below — shadow disappears at night), w
    /// is unused. Updated every frame by `update_fog_material`.
    pub sun_pos: Vec4,
    /// World metrics: x = mesh:world ratio (the fog mesh is scaled
    /// up so it covers area outside the world rectangle when zoomed
    /// out), y = world width in px, z = world height in px, w =
    /// effective fog "height" in px (drives shadow length; 0 = off).
    pub world: Vec4,
}

impl Material2d for FogMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/fog.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

/// Push the current sun position + fog shadow height into the fog
/// material every frame so the shadow direction tracks the day/night
/// cycle and the live PgUp/PgDn knob. F8 debug sun takes priority —
/// same convention as the water and lighting shaders.
fn update_fog_material(
    debug: Res<super::tilemap::DebugOptions>,
    cycle: Res<crate::daynight::DayNightCycle>,
    fog_q: Query<&MeshMaterial2d<FogMaterial>, With<super::tilemap::FogSprite>>,
    mut materials: ResMut<Assets<FogMaterial>>,
) {
    let Ok(handle) = fog_q.get_single() else { return };
    let Some(mat) = materials.get_mut(&handle.0) else { return };
    // Pack the sun's xy in world px and a normalized "day strength"
    // (0..1) into z. The z is what the shader uses to fade the shadow
    // on/off across dusk/dawn — `cycle.sun_elevation()` gives sin(t·TAU)
    // ∈ [−1, 1], we clamp the negative half (night) to 0. F8 debug
    // mode acts as "noon" so the shadow stays on while the user is
    // dragging the sun around with the cursor.
    let sun_pos = if debug.debug_sun_enabled {
        Vec4::new(debug.debug_sun_x, debug.debug_sun_y, 1.0, 0.0)
    } else {
        let w = super::world::world_w() as f32 * super::world::TILE_PX;
        let h = super::world::world_h() as f32 * super::world::TILE_PX;
        let center = Vec2::new(w / 2.0, -h / 2.0);
        let p = cycle.light_pos(center);
        let day_strength = cycle.sun_elevation().max(0.0);
        Vec4::new(p.x, p.y, day_strength, 0.0)
    };
    mat.params.sun_pos = sun_pos;
    // world.w carries the fog shadow height in world px — the shader
    // divides by SHADOW_SAMPLES to get its per-tap step length, so
    // 0 px means "no shadow" naturally.
    mat.params.world.w = debug.fog_shadow_height_px;
}
