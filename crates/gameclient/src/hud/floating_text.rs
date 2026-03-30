use bevy::prelude::*;

/// A floating text that rises and fades out (e.g., "+50 gold").
#[derive(Component)]
pub struct FloatingText {
    pub velocity: Vec2,
    pub lifetime: Timer,
}

/// Spawn a floating text at a world position.
pub fn spawn_floating_text(
    commands: &mut Commands,
    font: &Handle<Font>,
    text: &str,
    color: Color,
    position: Vec3,
) {
    commands.spawn((
        Text2d::new(text.to_string()),
        TextFont {
            font: font.clone(),
            font_size: 8.0,
            ..default()
        },
        TextColor(color),
        Transform::from_translation(position + Vec3::new(0.0, 8.0, 15.0)),
        FloatingText {
            velocity: Vec2::new(0.0, 20.0), // float upward
            lifetime: Timer::from_seconds(2.0, TimerMode::Once),
        },
    ));
}

/// System: update floating texts — move up, fade out, despawn when done.
pub fn update_floating_texts(
    mut commands: Commands,
    time: Res<Time>,
    mut query: Query<(Entity, &mut Transform, &mut TextColor, &mut FloatingText)>,
) {
    for (entity, mut tf, mut color, mut ft) in &mut query {
        ft.lifetime.tick(time.delta());

        // Move upward
        let dt = time.delta_secs();
        tf.translation.x += ft.velocity.x * dt;
        tf.translation.y += ft.velocity.y * dt;

        // Fade out based on remaining lifetime
        let alpha = 1.0 - ft.lifetime.fraction();
        color.0 = color.0.with_alpha(alpha);

        // Despawn when done
        if ft.lifetime.finished() {
            commands.entity(entity).despawn();
        }
    }
}
