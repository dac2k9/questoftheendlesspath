//! Night darkness + point lights (Phase 3 of day/night).
//!
//! A Material2d shader covering the world with a darkness overlay
//! driven by `DayNightCycle::night_alpha`. Point lights at players
//! and optionally POIs carve bright pools through the darkness using
//! a radial cosine-falloff (smoothstep) per-pixel in the shader.
//!
//! Spawned once on InGame enter. Per-frame system collects current
//! light positions and writes them into the material uniform.

use bevy::prelude::*;
use bevy::reflect::TypePath;
use bevy::render::render_resource::{AsBindGroup, ShaderRef};
use bevy::sprite::{AlphaMode2d, Material2d, Material2dPlugin};

use crate::states::AppState;
use crate::terrain::world::{WorldGrid, TILE_PX, world_h, world_w};

/// Must match MAX_LIGHTS in night_lights.wgsl (and in water.wgsl,
/// which consumes the same `SceneLights` resource).
pub const MAX_LIGHTS: usize = 32;
/// Player's light radius in world pixels. 80 px = 5 tiles — tight
/// enough to feel like a small lantern/torch, not a floodlight.
const PLAYER_LIGHT_RADIUS: f32 = 80.0;
/// POI light radius — ~2-tile halo around the POI center. Settlements
/// read as clearly lit from further away, so overworld navigation at
/// night can orient by the warm glow before the player is right on top.
const POI_LIGHT_RADIUS: f32 = 48.0;

pub struct NightLightsPlugin;

impl Plugin for NightLightsPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugins(Material2dPlugin::<NightMaterial>::default())
            .init_resource::<SceneLights>()
            .add_systems(OnEnter(AppState::InGame), spawn_night_overlay)
            .add_systems(
                Update,
                (gather_scene_lights, update_night_material)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Shared list of active point lights in the scene. Populated once per
/// frame by `gather_scene_lights` and consumed by multiple shaders —
/// night_lights (carves darkness), water (warm shimmer from nearby
/// lanterns). Centralizing keeps the two visuals in sync and avoids
/// duplicating the player/POI/fog-gate logic in two places.
#[derive(Resource, Default)]
pub struct SceneLights {
    /// xyz = world position, w = radius in world px. Only entries
    /// `[0..count)` are valid.
    pub lights: [Vec4; MAX_LIGHTS],
    pub count: usize,
    /// Cached `night_alpha` at gather time so callers don't each
    /// re-query DayNightCycle. Also drives the water shader's
    /// lantern-shimmer fade-in.
    pub night_alpha: f32,
}

#[derive(Asset, TypePath, AsBindGroup, Clone)]
pub struct NightMaterial {
    #[uniform(0)]
    pub params: NightParams,
}

#[derive(bevy::render::render_resource::ShaderType, Clone, Copy, Debug)]
pub struct NightParams {
    pub night_alpha: f32,
    pub num_lights: u32,
    pub _pad0: f32,
    pub _pad1: f32,
    /// Each entry: xy = world pos, z = radius (world px), w = reserved.
    pub lights: [Vec4; MAX_LIGHTS],
}

impl Default for NightParams {
    fn default() -> Self {
        Self {
            night_alpha: 0.0,
            num_lights: 0,
            _pad0: 0.0,
            _pad1: 0.0,
            lights: [Vec4::ZERO; MAX_LIGHTS],
        }
    }
}

impl Material2d for NightMaterial {
    fn fragment_shader() -> ShaderRef {
        "shaders/night_lights.wgsl".into()
    }
    fn alpha_mode(&self) -> AlphaMode2d {
        AlphaMode2d::Blend
    }
}

#[derive(Component)]
struct NightOverlaySprite;

fn spawn_night_overlay(
    mut commands: Commands,
    mut materials: ResMut<Assets<NightMaterial>>,
    mut meshes: ResMut<Assets<Mesh>>,
) {
    let w = world_w() as f32 * TILE_PX;
    let h = world_h() as f32 * TILE_PX;
    let cx = w / 2.0 - TILE_PX / 2.0;
    let cy = -h / 2.0 + TILE_PX / 2.0;
    let mesh = meshes.add(Rectangle::new(w, h));
    let mat = materials.add(NightMaterial {
        params: NightParams::default(),
    });
    commands.spawn((
        Mesh2d(mesh),
        MeshMaterial2d(mat),
        // Above clouds / player / UI-world-layer stuff. Below HUD UI
        // pass. Matches the old darkness sprite's z.
        Transform::from_xyz(cx, cy, 50.0),
        NightOverlaySprite,
    ));
}

/// Populate `SceneLights` once per frame: player, other players (on
/// the overworld only), and POIs the local player has revealed. F3's
/// fog_disabled debug skips the reveal gate so every POI glows.
fn gather_scene_lights(
    cycle: Res<crate::daynight::DayNightCycle>,
    my: Res<crate::terrain::tilemap::MyPlayerState>,
    polled: Res<crate::supabase::PolledPlayerState>,
    session: Res<crate::GameSession>,
    world: Option<Res<WorldGrid>>,
    fog: Res<crate::terrain::tilemap::FogOfWar>,
    debug: Res<crate::terrain::tilemap::DebugOptions>,
    mut scene: ResMut<SceneLights>,
) {
    scene.night_alpha = cycle.night_alpha();
    scene.count = 0;
    scene.lights = [Vec4::ZERO; MAX_LIGHTS];

    if my.initialized && my.location.is_none() {
        let p = tile_to_world(my.tile_x, my.tile_y);
        {
                let s = &mut *scene;
                push_light(&mut s.lights, &mut s.count, p, PLAYER_LIGHT_RADIUS);
            }
    }
    if let Ok(lock) = polled.players.lock() {
        for pr in lock.iter() {
            if pr.id == session.player_id { continue; }
            if pr.location.is_some() { continue; }
            if let (Some(tx), Some(ty)) = (pr.map_tile_x, pr.map_tile_y) {
                let p = tile_to_world(tx, ty);
                {
                let s = &mut *scene;
                push_light(&mut s.lights, &mut s.count, p, PLAYER_LIGHT_RADIUS);
            }
            }
        }
    }
    if let Some(world) = &world {
        for poi in &world.map.pois {
            // Skip POIs the player hasn't discovered yet — otherwise
            // their light halo carves a bright pool through the night
            // overlay at an unrevealed POI's position, leaking the
            // location through fog of war. F3's fog_disabled debug
            // toggle overrides this gate so the debug view still shows
            // every light everywhere.
            if !debug.fog_disabled {
                let idx = poi.y as usize * world_w() + poi.x as usize;
                if fog.revealed.get(idx).copied() != Some(true) {
                    continue;
                }
            }
            let p = tile_to_world(poi.x as i32, poi.y as i32);
            let s = &mut *scene;
            push_light(&mut s.lights, &mut s.count, p, POI_LIGHT_RADIUS);
        }
    }
}

fn update_night_material(
    scene: Res<SceneLights>,
    mut materials: ResMut<Assets<NightMaterial>>,
    q: Query<&MeshMaterial2d<NightMaterial>>,
) {
    for handle in &q {
        if let Some(mat) = materials.get_mut(&handle.0) {
            mat.params.night_alpha = scene.night_alpha;
            mat.params.num_lights = scene.count as u32;
            mat.params.lights = scene.lights;
        }
    }
}

fn tile_to_world(tx: i32, ty: i32) -> Vec2 {
    Vec2::new(tx as f32 * TILE_PX, -(ty as f32) * TILE_PX)
}

fn push_light(lights: &mut [Vec4; MAX_LIGHTS], n: &mut usize, pos: Vec2, radius: f32) {
    if *n >= MAX_LIGHTS { return; }
    lights[*n] = Vec4::new(pos.x, pos.y, radius, 0.0);
    *n += 1;
}
