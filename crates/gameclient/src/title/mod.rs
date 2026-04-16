use bevy::prelude::*;

use crate::states::AppState;
use crate::{GameFont, GameSession};

pub struct TitlePlugin;

impl Plugin for TitlePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(PendingLogin::default())
            .add_systems(Startup, (setup_font, spawn_title).chain())
            .add_systems(
                Update,
                (handle_input, handle_start_click, check_login_result)
                    .run_if(in_state(AppState::Title)),
            )
            .add_systems(OnExit(AppState::Title), cleanup_title);
    }
}

#[derive(Resource)]
struct TitleForm {
    walker_uuid: String,
    display_name: String,
    active_field: u8, // 0 = name, 1 = uuid
}

impl Default for TitleForm {
    fn default() -> Self {
        Self { walker_uuid: String::new(), display_name: "Adventurer".to_string(), active_field: 0 }
    }
}

#[derive(Component)]
struct TitleScreen;

#[derive(Component)]
struct NameText;

#[derive(Component)]
struct UuidText;

#[derive(Component)]
struct StartButton;

fn setup_font(mut commands: Commands, mut fonts: ResMut<Assets<Font>>) {
    commands.spawn(Camera2d);
    let font_bytes = include_bytes!("../../assets/fonts/PressStart2P.ttf");
    let font = fonts.add(Font::try_from_bytes(font_bytes.to_vec()).expect("valid font"));
    commands.insert_resource(GameFont(font));
}

fn spawn_title(mut commands: Commands, font: Res<GameFont>) {
    let f = font.0.clone();

    commands.insert_resource(TitleForm::default());
    commands.insert_resource(GameSession::default());

    // Dark background
    commands.spawn((
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            row_gap: Val::Px(20.0),
            ..default()
        },
        BackgroundColor(Color::srgb(0.06, 0.06, 0.12)),
        TitleScreen,
    )).with_children(|parent| {
        // Game title
        parent.spawn((
            Text::new("Quest of the"),
            TextFont { font: f.clone(), font_size: 14.0, ..default() },
            TextColor(Color::srgb(0.5, 0.5, 0.6)),
        ));
        parent.spawn((
            Text::new("Endless Path"),
            TextFont { font: f.clone(), font_size: 20.0, ..default() },
            TextColor(Color::srgb(1.0, 0.85, 0.3)),
            Node { margin: UiRect::bottom(Val::Px(30.0)), ..default() },
        ));

        // Name field
        parent.spawn((
            Text::new("Your name:"),
            TextFont { font: f.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(0.6, 0.6, 0.6)),
        ));
        parent.spawn((
            Text::new("> Adventurer_"),
            TextFont { font: f.clone(), font_size: 12.0, ..default() },
            TextColor(Color::srgb(0.77, 0.64, 0.35)),
            NameText,
            Node { margin: UiRect::bottom(Val::Px(12.0)), ..default() },
        ));

        // Walker UUID field
        parent.spawn((
            Text::new("Walker ID (from walker.akerud.se):"),
            TextFont { font: f.clone(), font_size: 8.0, ..default() },
            TextColor(Color::srgb(0.5, 0.5, 0.5)),
        ));
        parent.spawn((
            Text::new("  (optional)"),
            TextFont { font: f.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(0.4, 0.4, 0.4)),
            UuidText,
            Node { margin: UiRect::bottom(Val::Px(20.0)), ..default() },
        ));

        // Start button
        parent.spawn((
            Button,
            Node {
                padding: UiRect::axes(Val::Px(24.0), Val::Px(12.0)),
                border: UiRect::all(Val::Px(2.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.15, 0.12, 0.05, 0.9)),
            BorderColor(Color::srgb(0.6, 0.5, 0.2)),
            BorderRadius::all(Val::Px(6.0)),
            StartButton,
            TitleScreen,
        )).with_children(|btn| {
            btn.spawn((
                Text::new("Start Journey"),
                TextFont { font: f.clone(), font_size: 12.0, ..default() },
                TextColor(Color::srgb(1.0, 0.85, 0.3)),
            ));
        });

        // Hint
        parent.spawn((
            Text::new("TAB to switch fields | ENTER or click Start"),
            TextFont { font: f.clone(), font_size: 7.0, ..default() },
            TextColor(Color::srgb(0.35, 0.35, 0.35)),
            Node { margin: UiRect::top(Val::Px(20.0)), ..default() },
        ));
    });
}

fn handle_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut form: ResMut<TitleForm>,
    mut name_q: Query<(&mut Text, &mut TextColor), (With<NameText>, Without<UuidText>)>,
    mut uuid_q: Query<(&mut Text, &mut TextColor), (With<UuidText>, Without<NameText>)>,
    mut session: ResMut<GameSession>,
    mut next_state: ResMut<NextState<AppState>>,
    mut pending: ResMut<PendingLogin>,
) {
    // TAB switches fields
    if keys.just_pressed(KeyCode::Tab) {
        form.active_field = if form.active_field == 0 { 1 } else { 0 };
    }

    // Backspace
    if keys.just_pressed(KeyCode::Backspace) {
        if form.active_field == 0 { form.display_name.pop(); }
        else { form.walker_uuid.pop(); }
    }

    // Enter — join game
    if keys.just_pressed(KeyCode::Enter) && !form.display_name.is_empty() && !pending.waiting {
        start_game(&form, &mut session, &mut next_state, &mut pending);
        return;
    }

    let letter_keys = [
        (KeyCode::KeyA, 'A'), (KeyCode::KeyB, 'B'), (KeyCode::KeyC, 'C'),
        (KeyCode::KeyD, 'D'), (KeyCode::KeyE, 'E'), (KeyCode::KeyF, 'F'),
        (KeyCode::KeyG, 'G'), (KeyCode::KeyH, 'H'), (KeyCode::KeyI, 'I'),
        (KeyCode::KeyJ, 'J'), (KeyCode::KeyK, 'K'), (KeyCode::KeyL, 'L'),
        (KeyCode::KeyM, 'M'), (KeyCode::KeyN, 'N'), (KeyCode::KeyO, 'O'),
        (KeyCode::KeyP, 'P'), (KeyCode::KeyQ, 'Q'), (KeyCode::KeyR, 'R'),
        (KeyCode::KeyS, 'S'), (KeyCode::KeyT, 'T'), (KeyCode::KeyU, 'U'),
        (KeyCode::KeyV, 'V'), (KeyCode::KeyW, 'W'), (KeyCode::KeyX, 'X'),
        (KeyCode::KeyY, 'Y'), (KeyCode::KeyZ, 'Z'),
        (KeyCode::Digit0, '0'), (KeyCode::Digit1, '1'), (KeyCode::Digit2, '2'),
        (KeyCode::Digit3, '3'), (KeyCode::Digit4, '4'), (KeyCode::Digit5, '5'),
        (KeyCode::Digit6, '6'), (KeyCode::Digit7, '7'), (KeyCode::Digit8, '8'),
        (KeyCode::Digit9, '9'),
    ];

    for (key, ch) in letter_keys {
        if keys.just_pressed(key) {
            let shift = keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight);
            if form.active_field == 0 {
                // Name field — any letter/number
                if form.display_name.len() < 20 {
                    form.display_name.push(if shift { ch } else { ch.to_ascii_lowercase() });
                }
            } else {
                // UUID field — hex only
                if form.walker_uuid.len() < 36 {
                    let c = ch.to_ascii_lowercase();
                    if c.is_ascii_hexdigit() {
                        form.walker_uuid.push(c);
                    }
                }
            }
        }
    }
    // Space for name, hyphen for UUID
    if keys.just_pressed(KeyCode::Space) && form.active_field == 0 && form.display_name.len() < 20 {
        form.display_name.push(' ');
    }
    if keys.just_pressed(KeyCode::Minus) && form.active_field == 1 && form.walker_uuid.len() < 36 {
        form.walker_uuid.push('-');
    }

    // Update displays
    let active_color = Color::srgb(0.77, 0.64, 0.35);
    let inactive_color = Color::srgb(0.4, 0.4, 0.4);
    if let Ok((mut text, mut color)) = name_q.get_single_mut() {
        let cursor = if form.active_field == 0 { "_" } else { "" };
        let prefix = if form.active_field == 0 { "> " } else { "  " };
        **text = format!("{}{}{}", prefix, form.display_name, cursor);
        *color = TextColor(if form.active_field == 0 { active_color } else { inactive_color });
    }
    if let Ok((mut text, mut color)) = uuid_q.get_single_mut() {
        let cursor = if form.active_field == 1 { "_" } else { "" };
        let prefix = if form.active_field == 1 { "> " } else { "  " };
        if form.walker_uuid.is_empty() && form.active_field != 1 {
            **text = "  (optional)".to_string();
        } else {
            **text = format!("{}{}{}", prefix, form.walker_uuid, cursor);
        }
        *color = TextColor(if form.active_field == 1 { active_color } else { inactive_color });
    }
}

fn handle_start_click(
    mouse: Res<ButtonInput<MouseButton>>,
    form: Res<TitleForm>,
    btn_q: Query<&Interaction, With<StartButton>>,
    mut session: ResMut<GameSession>,
    mut next_state: ResMut<NextState<AppState>>,
    mut pending: ResMut<PendingLogin>,
) {
    if !mouse.just_pressed(MouseButton::Left) || pending.waiting { return; }
    for interaction in &btn_q {
        if matches!(interaction, Interaction::Hovered | Interaction::Pressed) {
            if !form.display_name.is_empty() {
                start_game(&form, &mut session, &mut next_state, &mut pending);
            }
        }
    }
}

fn check_login_result(
    mut pending: ResMut<PendingLogin>,
    mut session: ResMut<GameSession>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    if !pending.waiting { return; }
    let result = {
        let Ok(mut lock) = pending.result.lock() else { return };
        lock.take()
    };
    if let Some((player_id, name)) = result {
        pending.waiting = false;
        session.player_id = player_id;
        session.player_name = name;
        next_state.set(AppState::InGame);
    }
}

/// Holds async login result.
#[derive(Resource, Default)]
struct PendingLogin {
    result: std::sync::Arc<std::sync::Mutex<Option<(String, String)>>>, // (player_id, name)
    waiting: bool,
}

fn start_game(
    form: &TitleForm,
    session: &mut GameSession,
    _next_state: &mut NextState<AppState>,
    pending: &mut PendingLogin,
) {
    let name = form.display_name.clone();
    let walker_uuid = form.walker_uuid.clone();
    let result_ref = pending.result.clone();
    pending.waiting = true;

    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let body = serde_json::json!({
            "name": name,
            "walker_uuid": if walker_uuid.is_empty() { serde_json::Value::Null } else { serde_json::Value::String(walker_uuid) },
        });
        if let Ok(resp) = client.post("http://localhost:3001/join").json(&body).send().await {
            if let Ok(text) = resp.text().await {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(pid) = data.get("player_id").and_then(|v| v.as_str()) {
                        let pname = data.get("name").and_then(|v| v.as_str()).unwrap_or(&name);
                        if let Ok(mut lock) = result_ref.lock() {
                            *lock = Some((pid.to_string(), pname.to_string()));
                        }
                        return;
                    }
                }
            }
        }
        // Fallback
        if let Ok(mut lock) = result_ref.lock() {
            *lock = Some(("a0000000-0000-0000-0000-000000000001".to_string(), name));
        }
    });
}

fn cleanup_title(mut commands: Commands, query: Query<Entity, With<TitleScreen>>) {
    for entity in &query {
        commands.entity(entity).despawn_recursive();
    }
}
