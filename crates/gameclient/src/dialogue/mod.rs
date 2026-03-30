pub mod event_poll;

use bevy::prelude::*;

use crate::states::AppState;
use crate::GameFont;

pub struct DialoguePlugin;

impl Plugin for DialoguePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DialogueState::default())
            .insert_resource(NotificationQueue::default())
            .add_systems(
                Update,
                (
                    event_poll::poll_active_events,
                    update_dialogue,
                    update_notifications,
                    handle_dialogue_input,
                )
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

// ── Dialogue Box ──────────────────────────────────────

#[derive(Resource, Default)]
pub struct DialogueState {
    pub active: bool,
    pub event_id: String,
    pub speaker: String,
    pub lines: Vec<String>,
    pub current_line: usize,
    pub typewriter_index: usize,
    pub typewriter_timer: f32,
    pub choices: Vec<String>,
}

#[derive(Component)]
struct DialogueBox;

#[derive(Component)]
struct DialogueSpeaker;

#[derive(Component)]
struct DialogueText;

#[derive(Component)]
struct DialogueContinue;

fn update_dialogue(
    mut commands: Commands,
    font: Res<GameFont>,
    mut state: ResMut<DialogueState>,
    time: Res<Time>,
    existing: Query<Entity, With<DialogueBox>>,
) {
    if !state.active {
        // Remove dialogue box if it exists
        for entity in &existing {
            commands.entity(entity).despawn_recursive();
        }
        return;
    }

    // Typewriter effect
    state.typewriter_timer += time.delta_secs();
    if state.typewriter_timer > 0.03 {
        state.typewriter_timer = 0.0;
        if state.current_line < state.lines.len() {
            let full_line = &state.lines[state.current_line];
            if state.typewriter_index < full_line.len() {
                state.typewriter_index += 1;
            }
        }
    }

    // Don't rebuild UI every frame — only when needed
    if !existing.is_empty() {
        // Update text content
        return;
    }

    // Build dialogue box UI
    let speaker = state.speaker.clone();
    let line_text = if state.current_line < state.lines.len() {
        let full = &state.lines[state.current_line];
        full[..state.typewriter_index.min(full.len())].to_string()
    } else {
        String::new()
    };

    // Container centered on screen
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Percent(30.0),
            left: Val::Percent(15.0),
            right: Val::Percent(15.0),
            min_height: Val::Px(120.0),
            padding: UiRect::all(Val::Px(16.0)),
            border: UiRect::all(Val::Px(2.0)),
            flex_direction: FlexDirection::Column,
            ..default()
        },
        BackgroundColor(Color::srgba(0.02, 0.02, 0.08, 0.92)),
        BorderColor(Color::srgb(0.4, 0.35, 0.2)),
        BorderRadius::all(Val::Px(6.0)),
        DialogueBox,
    )).with_children(|parent| {
        // Speaker name
        parent.spawn((
            Text::new(speaker),
            TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(1.0, 0.85, 0.3)),
            DialogueSpeaker,
        ));

        // Dialogue text
        parent.spawn((
            Text::new(line_text),
            TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
            TextColor(Color::srgb(0.9, 0.9, 0.9)),
            Node { margin: UiRect::top(Val::Px(8.0)), ..default() },
            DialogueText,
        ));

        // Continue prompt
        parent.spawn((
            Text::new("[Enter / Click to continue]"),
            TextFont { font: font.0.clone(), font_size: 7.0, ..default() },
            TextColor(Color::srgb(0.5, 0.5, 0.5)),
            Node { margin: UiRect::top(Val::Px(12.0)), ..default() },
            DialogueContinue,
        ));
    });
}

fn handle_dialogue_input(
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut state: ResMut<DialogueState>,
    mut commands: Commands,
    existing: Query<Entity, With<DialogueBox>>,
) {
    if !state.active {
        return;
    }

    let advance = keys.just_pressed(KeyCode::Enter)
        || keys.just_pressed(KeyCode::Space)
        || mouse.just_pressed(MouseButton::Left);

    if !advance {
        return;
    }

    // If typewriter hasn't finished, show full line
    if state.current_line < state.lines.len() {
        let full_len = state.lines[state.current_line].len();
        if state.typewriter_index < full_len {
            state.typewriter_index = full_len;
            // Rebuild UI to show full text
            for entity in &existing {
                commands.entity(entity).despawn_recursive();
            }
            return;
        }
    }

    // Advance to next line
    state.current_line += 1;
    state.typewriter_index = 0;

    if state.current_line >= state.lines.len() {
        // Dialogue complete — dismiss and notify server
        let event_id = state.event_id.clone();
        state.active = false;
        state.lines.clear();
        state.current_line = 0;

        // Remove dialogue box
        for entity in &existing {
            commands.entity(entity).despawn_recursive();
        }

        // POST completion to dev server
        if !event_id.is_empty() {
            let url = format!("http://localhost:3001/events/{}/complete", event_id);
            wasm_bindgen_futures::spawn_local(async move {
                let client = reqwest::Client::new();
                let _ = client.post(&url).send().await;
            });
        }
    } else {
        // Rebuild UI for new line
        for entity in &existing {
            commands.entity(entity).despawn_recursive();
        }
    }
}

// ── Notification Banners ──────────────────────────────

#[derive(Resource, Default)]
pub struct NotificationQueue {
    pub pending: Vec<NotificationData>,
}

pub struct NotificationData {
    pub text: String,
    pub duration: f32,
}

#[derive(Component)]
struct NotificationBanner {
    timer: Timer,
}

#[derive(Component)]
struct NotificationDismiss;

fn update_notifications(
    mut commands: Commands,
    font: Res<GameFont>,
    time: Res<Time>,
    keys: Res<ButtonInput<KeyCode>>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut queue: ResMut<NotificationQueue>,
    mut banners: Query<(Entity, &mut NotificationBanner)>,
    dismiss_q: Query<&Interaction, With<NotificationDismiss>>,
) {
    // Dismiss on X key or clicking the X button
    let mut dismiss = keys.just_pressed(KeyCode::KeyX);
    for interaction in &dismiss_q {
        if *interaction == Interaction::Pressed {
            dismiss = true;
        }
    }
    if dismiss {
        for (entity, _) in &banners {
            commands.entity(entity).despawn_recursive();
        }
        return;
    }

    // Update existing banners
    for (entity, mut banner) in &mut banners {
        banner.timer.tick(time.delta());
        if banner.timer.finished() {
            commands.entity(entity).despawn_recursive();
        }
    }

    // Show next notification if no banner active
    if banners.is_empty() {
        if let Some(notif) = queue.pending.pop() {
            commands.spawn((
                Node {
                    position_type: PositionType::Absolute,
                    top: Val::Px(40.0),
                    left: Val::Percent(10.0),
                    right: Val::Percent(10.0),
                    padding: UiRect::all(Val::Px(12.0)),
                    justify_content: JustifyContent::SpaceBetween,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(Color::srgba(0.05, 0.05, 0.15, 0.9)),
                BorderColor(Color::srgb(0.4, 0.35, 0.2)),
                BorderRadius::all(Val::Px(4.0)),
                NotificationBanner {
                    timer: Timer::from_seconds(999.0, TimerMode::Once), // stays until dismissed
                },
            )).with_children(|parent| {
                parent.spawn((
                    Text::new(notif.text),
                    TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                    TextColor(Color::srgb(1.0, 0.95, 0.7)),
                ));
                // Dismiss button — clickable
                parent.spawn((
                    Button,
                    Node {
                        padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.4, 0.2, 0.2, 0.5)),
                    BorderRadius::all(Val::Px(3.0)),
                    NotificationDismiss,
                )).with_children(|btn| {
                    btn.spawn((
                        Text::new("X"),
                        TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                        TextColor(Color::srgb(1.0, 0.6, 0.6)),
                    ));
                });
            });
        }
    }
}
