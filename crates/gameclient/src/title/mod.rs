use bevy::prelude::*;

use std::sync::{Arc, Mutex};

use crate::states::AppState;
use crate::supabase::SupabaseConfig;
use crate::{GameFont, GameSession};

/// Holds async lookup result — checked each frame to trigger transition.
#[derive(Resource, Default)]
struct PendingLogin {
    result: Arc<Mutex<Option<(String, String)>>>, // (game_id, player_id)
    waiting: bool,
}

#[derive(Component)]
struct ClickZone {
    field: ActiveField,
    half_width: f32,
    half_height: f32,
}

pub struct TitlePlugin;

impl Plugin for TitlePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(PendingLogin::default())
            .add_systems(Startup, (setup_font, spawn_title).chain())
            .add_systems(Update, (handle_input, handle_mouse, check_login_result).run_if(in_state(AppState::Title)))
            .add_systems(OnExit(AppState::Title), cleanup_title);
    }
}

/// Which field the cursor is on.
#[derive(Resource, Default)]
struct TitleForm {
    active_field: ActiveField,
    username: String,
    join_code: String,
}

#[derive(Default, PartialEq, Clone, Copy)]
enum ActiveField {
    #[default]
    Username,
    JoinCode,
}

// Marker components
#[derive(Component)]
struct TitleScreen;

#[derive(Component)]
struct UsernameText;

#[derive(Component)]
struct JoinCodeText;

#[derive(Component)]
struct StatusText;

fn setup_font(mut commands: Commands, asset_server: Res<AssetServer>) {
    commands.spawn(Camera2d);
    let font = asset_server.load("fonts/PressStart2P.ttf");
    commands.insert_resource(GameFont(font));
}

fn spawn_title(mut commands: Commands, font: Res<GameFont>, asset_server: Res<AssetServer>) {
    let f = font.0.clone();

    // Default values for quick testing
    commands.insert_resource(TitleForm {
        active_field: ActiveField::Username,
        username: "Dac".to_string(),
        join_code: "QUEST1".to_string(),
    });

    commands.insert_resource(GameSession::default());

    // Background image
    commands.spawn((
        Sprite {
            image: asset_server.load("background.png"),
            custom_size: Some(Vec2::new(1200.0, 700.0)), // oversized to fill any aspect ratio
            ..default()
        },
        Transform::from_xyz(0.0, 0.0, -1.0),
        TitleScreen,
    ));

    // Semi-transparent dark overlay at bottom for login form
    commands.spawn((
        Sprite {
            color: Color::srgba(0.0, 0.0, 0.0, 0.5),
            custom_size: Some(Vec2::new(960.0, 200.0)),
            ..default()
        },
        Transform::from_xyz(0.0, -170.0, 0.0),
        TitleScreen,
    ));

    // Username label
    commands.spawn((
        Text2d::new("Name:"),
        TextFont { font: f.clone(), font_size: 16.0, ..default() },
        TextColor(Color::srgb(0.6, 0.6, 0.6)),
        Transform::from_xyz(-160.0, -128.0, 1.0),
        TitleScreen,
    ));

    // Username value + click zone
    commands.spawn((
        Text2d::new("> Dac_"),
        TextFont { font: f.clone(), font_size: 16.0, ..default() },
        TextColor(Color::srgb(0.77, 0.64, 0.35)),
        Transform::from_xyz(-160.0, -160.0, 1.0),
        UsernameText,
        ClickZone { field: ActiveField::Username, half_width: 200.0, half_height: 24.0 },
        TitleScreen,
    ));

    // Join code label
    commands.spawn((
        Text2d::new("Game code:"),
        TextFont { font: f.clone(), font_size: 16.0, ..default() },
        TextColor(Color::srgb(0.6, 0.6, 0.6)),
        Transform::from_xyz(-160.0, -200.0, 1.0),
        TitleScreen,
    ));

    // Join code value + click zone
    commands.spawn((
        Text2d::new("  QUEST1"),
        TextFont { font: f.clone(), font_size: 16.0, ..default() },
        TextColor(Color::srgb(0.5, 0.5, 0.5)),
        Transform::from_xyz(-160.0, -232.0, 1.0),
        JoinCodeText,
        ClickZone { field: ActiveField::JoinCode, half_width: 200.0, half_height: 24.0 },
        TitleScreen,
    ));

    // Instructions
    commands.spawn((
        Text2d::new("TAB to switch  |  ENTER to join"),
        TextFont { font: f.clone(), font_size: 8.0, ..default() },
        TextColor(Color::srgb(0.35, 0.35, 0.35)),
        Transform::from_xyz(0.0, -260.0, 1.0),
        TitleScreen,
    ));

    // Status text (for errors / welcome)
    commands.spawn((
        Text2d::new(""),
        TextFont { font: f, font_size: 16.0, ..default() },
        TextColor(Color::srgb(0.3, 0.8, 0.3)),
        Transform::from_xyz(0.0, -100.0, 1.0),
        StatusText,
        TitleScreen,
    ));
}

fn handle_input(
    keys: Res<ButtonInput<KeyCode>>,
    mut form: ResMut<TitleForm>,
    mut username_q: Query<(&mut Text2d, &mut TextColor), (With<UsernameText>, Without<JoinCodeText>, Without<StatusText>)>,
    mut joincode_q: Query<(&mut Text2d, &mut TextColor), (With<JoinCodeText>, Without<UsernameText>, Without<StatusText>)>,
    mut status_q: Query<&mut Text2d, (With<StatusText>, Without<UsernameText>, Without<JoinCodeText>)>,
    mut session: ResMut<GameSession>,
    mut config: ResMut<SupabaseConfig>,
    mut pending: ResMut<PendingLogin>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    // Tab switches fields
    if keys.just_pressed(KeyCode::Tab) {
        form.active_field = match form.active_field {
            ActiveField::Username => ActiveField::JoinCode,
            ActiveField::JoinCode => ActiveField::Username,
        };
    }

    // Backspace
    if keys.just_pressed(KeyCode::Backspace) {
        match form.active_field {
            ActiveField::Username => { form.username.pop(); }
            ActiveField::JoinCode => { form.join_code.pop(); }
        }
    }

    // Enter — join game
    if keys.just_pressed(KeyCode::Enter) {
        if form.username.is_empty() {
            if let Ok(mut text) = status_q.get_single_mut() {
                *text = Text2d::new("Enter a name!");
            }
            return;
        }
        if form.join_code.is_empty() {
            if let Ok(mut text) = status_q.get_single_mut() {
                *text = Text2d::new("Enter a game code!");
            }
            return;
        }

        session.player_name = form.username.clone();
        session.join_code = form.join_code.clone();

        // Set Supabase config
        config.url = "https://nmgvrnyrnnftgyszadzc.supabase.co".to_string();
        config.anon_key = "sb_publishable_Cz1-0kJ2OczX4slHUR0gqg_cSx9Lo5-".to_string();

        if let Ok(mut text) = status_q.get_single_mut() {
            *text = Text2d::new("Connecting...");
        }

        // Launch async lookup — result will be checked by check_login_result system
        let url = config.url.clone();
        let key = config.anon_key.clone();
        let username = form.username.clone();
        let join_code = form.join_code.clone();
        let result_ref = pending.result.clone();
        pending.waiting = true;

        wasm_bindgen_futures::spawn_local(async move {
            let client = reqwest::Client::new();

            #[derive(serde::Deserialize)]
            struct GameRow { id: String }
            #[derive(serde::Deserialize)]
            struct PlayerRow { id: String }

            let mut game_id = String::new();
            let mut player_id = String::new();

            if let Ok(resp) = client
                .get(format!("{}/rest/v1/games?join_code=eq.{}&select=id", url, join_code))
                .header("apikey", &key)
                .header("Authorization", format!("Bearer {}", &key))
                .send().await
            {
                if let Ok(games) = resp.json::<Vec<GameRow>>().await {
                    if let Some(game) = games.first() {
                        game_id = game.id.clone();

                        if let Ok(resp2) = client
                            .get(format!("{}/rest/v1/players?game_id=eq.{}&name=ilike.{}&select=id", url, game.id, username))
                            .header("apikey", &key)
                            .header("Authorization", format!("Bearer {}", &key))
                            .send().await
                        {
                            if let Ok(players) = resp2.json::<Vec<PlayerRow>>().await {
                                if let Some(player) = players.first() {
                                    player_id = player.id.clone();
                                }
                            }
                        }
                    }
                }
            }

            if let Ok(mut lock) = result_ref.lock() {
                *lock = Some((game_id, player_id));
            }
        });
        return;
    }

    // Character input — map pressed keys to chars
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
            let c = if keys.pressed(KeyCode::ShiftLeft) || keys.pressed(KeyCode::ShiftRight) {
                ch
            } else {
                ch.to_ascii_lowercase()
            };
            match form.active_field {
                ActiveField::Username => {
                    if form.username.len() < 16 {
                        form.username.push(c);
                    }
                }
                ActiveField::JoinCode => {
                    if form.join_code.len() < 10 {
                        form.join_code.push(c.to_ascii_uppercase());
                    }
                }
            }
        }
    }

    // Update display
    let active_color = Color::srgb(0.77, 0.64, 0.35);
    let inactive_color = Color::srgb(0.5, 0.5, 0.5);
    let cursor = "_";

    if let Ok((mut text, mut color)) = username_q.get_single_mut() {
        let prefix = if form.active_field == ActiveField::Username { "> " } else { "  " };
        let c = if form.active_field == ActiveField::Username { cursor } else { "" };
        *text = Text2d::new(format!("{}{}{}", prefix, form.username, c));
        *color = TextColor(if form.active_field == ActiveField::Username { active_color } else { inactive_color });
    }

    if let Ok((mut text, mut color)) = joincode_q.get_single_mut() {
        let prefix = if form.active_field == ActiveField::JoinCode { "> " } else { "  " };
        let c = if form.active_field == ActiveField::JoinCode { cursor } else { "" };
        *text = Text2d::new(format!("{}{}{}", prefix, form.join_code, c));
        *color = TextColor(if form.active_field == ActiveField::JoinCode { active_color } else { inactive_color });
    }
}

fn handle_mouse(
    mouse: Res<ButtonInput<MouseButton>>,
    windows: Query<&Window>,
    camera_q: Query<(&Camera, &GlobalTransform)>,
    click_zones: Query<(&Transform, &ClickZone)>,
    mut form: ResMut<TitleForm>,
) {
    if !mouse.just_pressed(MouseButton::Left) {
        return;
    }

    let Ok(window) = windows.get_single() else { return };
    let Ok((camera, camera_transform)) = camera_q.get_single() else { return };
    let Some(cursor_pos) = window.cursor_position() else { return };
    let Ok(world_pos) = camera.viewport_to_world_2d(camera_transform, cursor_pos) else { return };

    for (transform, zone) in &click_zones {
        let pos = transform.translation;
        if (world_pos.x - pos.x).abs() < zone.half_width
            && (world_pos.y - pos.y).abs() < zone.half_height
        {
            form.active_field = zone.field;
            return;
        }
    }
}

/// Check if the async login lookup completed, then transition to InGame.
fn check_login_result(
    mut pending: ResMut<PendingLogin>,
    mut session: ResMut<GameSession>,
    mut next_state: ResMut<NextState<AppState>>,
) {
    if !pending.waiting {
        return;
    }

    let result = {
        let Ok(mut lock) = pending.result.lock() else { return };
        lock.take()
    };

    if let Some((game_id, player_id)) = result {
        pending.waiting = false;

        if game_id.is_empty() || player_id.is_empty() {
            // Lookup failed — will show error on next frame
            return;
        }

        session.game_id = game_id;
        session.player_id = player_id;
        next_state.set(AppState::InGame);
    }
}

fn cleanup_title(mut commands: Commands, query: Query<Entity, With<TitleScreen>>) {
    for entity in &query {
        commands.entity(entity).despawn_recursive();
    }
}
