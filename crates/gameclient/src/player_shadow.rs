//! Per-player shadow on the ground using the player's own animated
//! sprite as the shadow shape.
//!
//! Approach: spawn one extra Sprite entity per player, sharing the
//! player's image + texture atlas, tinted dark with alpha. Each frame
//! its Transform is set so the sprite's bottom edge sits at the
//! player's feet and the rest of the sprite "lays down" along the
//! ground in the direction opposite the sun. The result is a
//! pixel-perfect silhouette projection that walks/animates/turns with
//! the player at zero shader cost.
//!
//! Geometry (sun-far-away parallel projection):
//!   shadow_offset_per_unit_height = -(sun.xy) / sun.z
//!   shadow_total_for_h_pixels     = shadow_offset * h
//!
//! Because the sprite is rendered with Anchor::BottomCenter, rotating
//! about the bottom keeps the feet planted; rotating local +Y to
//! match shadow_offset, then scaling local Y by |shadow_offset|,
//! puts the head at (player_feet + sprite_height * shadow_offset).

use bevy::prelude::*;
use bevy::sprite::Anchor;

use crate::daynight::DayNightCycle;
use crate::states::AppState;
use crate::terrain::tilemap::{DebugOptions, PlayerSprite};
use crate::terrain::world::{TILE_PX, world_h, world_w};

pub struct PlayerShadowPlugin;

impl Plugin for PlayerShadowPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(
            Update,
            (spawn_shadows, update_shadows)
                .chain()
                .run_if(in_state(AppState::InGame)),
        );
    }
}

#[derive(Component)]
struct PlayerShadow;

/// Links a shadow entity back to the player it follows.
#[derive(Component)]
struct ShadowFor(Entity);

/// Multiplicative tint for shadow pixels: black, 45 % alpha.
const SHADOW_TINT: Color = Color::srgba(0.0, 0.0, 0.05, 0.45);
/// Cap on the shadow_offset length — without this, low sun (sz near
/// 0) sends the shadow trailing across the whole map. 3 = at most
/// 3 sprite-heights of horizontal projection (~48 px for the player).
const MAX_SHADOW_LENGTH_FACTOR: f32 = 3.0;

/// Champion sprites are 16×16 with the visual feet at roughly row 13
/// (~6 px below the sprite's center), and 2 empty rows below the feet
/// to the frame bottom. The shadow needs to compensate for that
/// padding — without compensation, scaling the empty rows along with
/// the rest of the sprite leaves a visible gap between the player's
/// feet and the visible head of the shadow that grows with scale.
const VISUAL_FEET_FROM_CENTER_PX: f32 = 6.0;
const SPRITE_BOTTOM_PADDING_PX: f32 = 2.0;

/// Spawn a shadow companion the first time we see each player sprite.
fn spawn_shadows(
    mut commands: Commands,
    player_q: Query<(Entity, &Sprite), With<PlayerSprite>>,
    shadow_q: Query<&ShadowFor, With<PlayerShadow>>,
) {
    let already_shadowed: std::collections::HashSet<Entity> =
        shadow_q.iter().map(|s| s.0).collect();
    for (entity, sprite) in &player_q {
        if already_shadowed.contains(&entity) {
            continue;
        }
        commands.spawn((
            Sprite {
                image: sprite.image.clone(),
                texture_atlas: sprite.texture_atlas.clone(),
                color: SHADOW_TINT,
                anchor: Anchor::BottomCenter,
                ..default()
            },
            // z=0.7 sits above the procedural ground mesh (~0.5 max
            // with tile_z_factor=0.5) and below the water shader (0.99)
            // and all character/POI sprites (1.5+).
            Transform::from_xyz(0.0, 0.0, 0.7),
            Visibility::Hidden,
            PlayerShadow,
            ShadowFor(entity),
        ));
    }
}

/// Per frame: compute the parallel-projection shadow_offset from the
/// current sun position and update every PlayerShadow's transform so
/// it lands at its player's feet and aligns with that vector.
fn update_shadows(
    debug: Res<DebugOptions>,
    cycle: Res<DayNightCycle>,
    player_q: Query<(&Transform, &Sprite), (With<PlayerSprite>, Without<PlayerShadow>)>,
    mut shadow_q: Query<
        (&mut Transform, &mut Sprite, &mut Visibility, &ShadowFor),
        (With<PlayerShadow>, Without<PlayerSprite>),
    >,
) {
    // Sun position — F8 debug override > day/night cycle.
    let sun_pos = if debug.debug_sun_enabled {
        Vec3::new(debug.debug_sun_x, debug.debug_sun_y, debug.debug_sun_z)
    } else {
        let w = world_w() as f32 * TILE_PX;
        let h = world_h() as f32 * TILE_PX;
        let center = Vec2::new(w / 2.0, -h / 2.0);
        cycle.light_pos(center)
    };

    for (mut shadow_tf, mut shadow_sprite, mut shadow_vis, shadow_for) in &mut shadow_q {
        let Ok((player_tf, player_sprite)) = player_q.get(shadow_for.0) else { continue };
        // Sun direction per-caster, from this player's feet. For the
        // real cycle (sun ~10000 px away) this is barely different
        // from the world-center approximation, but for F8 debug-sun
        // mode (sun is inside the viewport) it's the difference
        // between an intuitive shadow and one that drifts.
        let player_feet = Vec3::new(
            player_tf.translation.x,
            player_tf.translation.y - 8.0,
            0.0,
        );
        let sun_dir = (sun_pos - player_feet).normalize_or_zero();
        // shadow_offset = ground displacement per 1 unit of caster height.
        let shadow_offset = if sun_dir.z > 0.05 {
            let mut o = Vec2::new(-sun_dir.x / sun_dir.z, -sun_dir.y / sun_dir.z);
            let len = o.length();
            if len > MAX_SHADOW_LENGTH_FACTOR {
                o *= MAX_SHADOW_LENGTH_FACTOR / len;
            }
            Some(o)
        } else {
            None
        };

        // Mirror the player's animation frame and facing.
        if let (Some(s_atlas), Some(p_atlas)) = (
            shadow_sprite.texture_atlas.as_mut(),
            player_sprite.texture_atlas.as_ref(),
        ) {
            s_atlas.index = p_atlas.index;
        }
        shadow_sprite.flip_x = player_sprite.flip_x;

        let Some(offset) = shadow_offset else {
            // Sun below horizon — hide the shadow entirely.
            *shadow_vis = Visibility::Hidden;
            continue;
        };
        let len = offset.length();
        if len < 0.001 {
            // Sun directly overhead — no shadow.
            *shadow_vis = Visibility::Hidden;
            continue;
        }
        *shadow_vis = Visibility::Visible;

        // Rotate so the sprite's local +Y axis points along the shadow
        // direction. Sprite's +Y originally points to world +Y, so:
        //   angle = -atan2(offset.x, offset.y)
        let angle = -offset.x.atan2(offset.y);

        // Position the shadow so the visible character's feet (not the
        // frame's bottom edge) coincide with the player's visual feet.
        // The character's feet sit at local (0, padding) before scale.
        // After scale_y * rotation, that point ends up at
        //   T + R(angle) * (0, padding * scale_y)
        // which equals
        //   T + (-padding * scale_y * sin(angle), padding * scale_y * cos(angle)).
        // We want it at (player.x, player.y - visual_feet_offset), so:
        let s = SPRITE_BOTTOM_PADDING_PX * len;
        shadow_tf.translation.x = (player_tf.translation.x + s * angle.sin()).round();
        shadow_tf.translation.y =
            (player_tf.translation.y - VISUAL_FEET_FROM_CENTER_PX - s * angle.cos()).round();
        shadow_tf.translation.z = 0.7;
        shadow_tf.rotation = Quat::from_rotation_z(angle);
        // Stretch local Y by |offset| so the sprite's head ends up at
        // sprite_height × offset away from the feet on the ground.
        shadow_tf.scale = Vec3::new(1.0, len, 1.0);
    }
}
