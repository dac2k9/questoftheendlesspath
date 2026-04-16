use std::sync::{Arc, Mutex};

use bevy::prelude::*;

use crate::states::AppState;
use crate::{GameFont, GameSession};

pub struct TitlePlugin;

impl Plugin for TitlePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(PendingLogin::default())
            .insert_resource(WalkerPlayers::default())
            .add_systems(Startup, (setup_font, spawn_title).chain())
            .add_systems(
                Update,
                (fetch_walker_players, handle_player_click, check_login_result)
                    .run_if(in_state(AppState::Title)),
            )
            .add_systems(OnExit(AppState::Title), cleanup_title);
    }
}

// ── Resources ───────────────────────────────────────

#[derive(Resource, Default)]
struct PendingLogin {
    result: Arc<Mutex<Option<(String, String)>>>,
    waiting: bool,
}

#[derive(Clone)]
struct WalkerPlayer {
    id: String,
    name: String,
    status: String,
    speed: f32,
}

#[derive(Resource, Default)]
struct WalkerPlayers {
    fetched: Arc<Mutex<Option<Vec<WalkerPlayer>>>>,
    players: Vec<WalkerPlayer>,
    loaded: bool,
    fetch_started: bool,
}

// ── Components ──────────────────────────────────────

#[derive(Component)]
struct TitleScreen;

#[derive(Component)]
struct PlayerListContainer;

#[derive(Component)]
struct PlayerButton(String, String); // (walker_id, name)

#[derive(Component)]
struct StatusText;

// ── Setup ───────────────────────────────────────────

fn setup_font(mut commands: Commands, mut fonts: ResMut<Assets<Font>>) {
    commands.spawn(Camera2d);
    let font_bytes = include_bytes!("../../assets/fonts/PressStart2P.ttf");
    let font = fonts.add(Font::try_from_bytes(font_bytes.to_vec()).expect("valid font"));
    commands.insert_resource(GameFont(font));
}

fn spawn_title(mut commands: Commands, font: Res<GameFont>) {
    let f = font.0.clone();

    commands.insert_resource(GameSession::default());

    commands.spawn((
        Node {
            width: Val::Percent(100.0),
            height: Val::Percent(100.0),
            flex_direction: FlexDirection::Column,
            justify_content: JustifyContent::Center,
            align_items: AlignItems::Center,
            row_gap: Val::Px(16.0),
            ..default()
        },
        BackgroundColor(Color::srgb(0.06, 0.06, 0.12)),
        TitleScreen,
    )).with_children(|parent| {
        // Title
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

        // "Select your walker" label
        parent.spawn((
            Text::new("Select your Walker profile:"),
            TextFont { font: f.clone(), font_size: 9.0, ..default() },
            TextColor(Color::srgb(0.6, 0.6, 0.6)),
        ));

        // Player list container (populated by fetch_walker_players)
        parent.spawn((
            Node {
                flex_direction: FlexDirection::Column,
                row_gap: Val::Px(8.0),
                align_items: AlignItems::Center,
                min_height: Val::Px(60.0),
                ..default()
            },
            PlayerListContainer,
        )).with_children(|list| {
            list.spawn((
                Text::new("Loading walkers..."),
                TextFont { font: f.clone(), font_size: 8.0, ..default() },
                TextColor(Color::srgb(0.4, 0.4, 0.4)),
                StatusText,
            ));
        });

        // Footer hint
        parent.spawn((
            Text::new("Walk on your treadmill to play"),
            TextFont { font: f.clone(), font_size: 7.0, ..default() },
            TextColor(Color::srgb(0.3, 0.3, 0.3)),
            Node { margin: UiRect::top(Val::Px(30.0)), ..default() },
        ));
    });
}

// ── Fetch Walker Leaderboard ────────────────────────

fn fetch_walker_players(
    mut commands: Commands,
    font: Res<GameFont>,
    mut walkers: ResMut<WalkerPlayers>,
    container_q: Query<Entity, With<PlayerListContainer>>,
    pending: Res<PendingLogin>,
) {
    // Start fetch on first frame
    if !walkers.fetch_started {
        walkers.fetch_started = true;
        let fetched = walkers.fetched.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let client = reqwest::Client::new();
            match client.get("https://walker.akerud.se/api/leaderboard")
                .timeout(std::time::Duration::from_secs(10))
                .send().await
            {
                Ok(resp) => {
                    if let Ok(data) = resp.json::<serde_json::Value>().await {
                        let mut players = Vec::new();
                        let mut seen = std::collections::HashSet::new();
                        for period in ["today", "weekly", "all_time"] {
                            if let Some(entries) = data.get(period).and_then(|v| v.as_array()) {
                                for entry in entries {
                                    let id = entry.get("id").and_then(|v| v.as_str()).unwrap_or("").to_string();
                                    if seen.contains(&id) { continue; }
                                    seen.insert(id.clone());
                                    players.push(WalkerPlayer {
                                        id,
                                        name: entry.get("name").and_then(|v| v.as_str()).unwrap_or("?").to_string(),
                                        status: entry.get("status").and_then(|v| v.as_str()).unwrap_or("offline").to_string(),
                                        speed: entry.get("speed_kmh").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32,
                                    });
                                }
                            }
                        }
                        if let Ok(mut lock) = fetched.lock() {
                            *lock = Some(players);
                        }
                    }
                }
                Err(e) => {
                    log::error!("[title] Failed to fetch Walker leaderboard: {}", e);
                }
            }
        });
    }

    // Check for fetched results
    let new_players = {
        let Ok(mut lock) = walkers.fetched.lock() else { return };
        lock.take()
    };

    if let Some(players) = new_players {
        walkers.players = players;
        walkers.loaded = true;

        // Rebuild player list UI
        let Ok(container) = container_q.get_single() else { return };
        commands.entity(container).despawn_descendants();

        if walkers.players.is_empty() {
            commands.entity(container).with_children(|list| {
                list.spawn((
                    Text::new("No walkers found"),
                    TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                    TextColor(Color::srgb(0.5, 0.3, 0.3)),
                ));
            });
            return;
        }

        let f = font.0.clone();
        commands.entity(container).with_children(|list| {
            for player in &walkers.players {
                let is_walking = player.status == "walking";
                let status_dot = if is_walking { "* " } else { "  " };
                let speed_text = if is_walking { format!(" ({:.1} km/h)", player.speed) } else { String::new() };

                list.spawn((
                    Button,
                    Node {
                        padding: UiRect::axes(Val::Px(20.0), Val::Px(10.0)),
                        border: UiRect::all(Val::Px(2.0)),
                        min_width: Val::Px(300.0),
                        justify_content: JustifyContent::Center,
                        ..default()
                    },
                    BackgroundColor(if is_walking {
                        Color::srgba(0.1, 0.15, 0.05, 0.9)
                    } else {
                        Color::srgba(0.1, 0.1, 0.1, 0.7)
                    }),
                    BorderColor(if is_walking {
                        Color::srgb(0.4, 0.6, 0.2)
                    } else {
                        Color::srgb(0.3, 0.3, 0.3)
                    }),
                    BorderRadius::all(Val::Px(6.0)),
                    PlayerButton(player.id.clone(), player.name.clone()),
                )).with_children(|btn| {
                    btn.spawn((
                        Text::new(format!("{}{}{}", status_dot, player.name, speed_text)),
                        TextFont { font: f.clone(), font_size: 10.0, ..default() },
                        TextColor(if is_walking {
                            Color::srgb(0.5, 0.9, 0.4)
                        } else {
                            Color::srgb(0.6, 0.6, 0.6)
                        }),
                    ));
                });
            }
        });
    }
}

// ── Handle Player Click ─────────────────────────────

fn handle_player_click(
    mouse: Res<ButtonInput<MouseButton>>,
    btn_q: Query<(&Interaction, &PlayerButton)>,
    mut pending: ResMut<PendingLogin>,
) {
    if !mouse.just_pressed(MouseButton::Left) || pending.waiting { return; }

    for (interaction, player_btn) in &btn_q {
        if !matches!(interaction, Interaction::Hovered | Interaction::Pressed) { continue; }

        let walker_id = player_btn.0.clone();
        let name = player_btn.1.clone();
        let result_ref = pending.result.clone();
        pending.waiting = true;

        log::info!("[join] Joining as '{}' (walker: {})", name, walker_id);

        wasm_bindgen_futures::spawn_local(async move {
            let client = reqwest::Client::new();
            let url = crate::api_url("/join");
            let body = serde_json::json!({
                "name": name,
                "walker_uuid": walker_id,
            });
            match client.post(&url).json(&body).send().await {
                Ok(resp) => {
                    if let Ok(text) = resp.text().await {
                        log::info!("[join] Response: {}", text);
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
                Err(e) => {
                    log::error!("[join] Request failed: {}", e);
                }
            }
        });
        return;
    }
}

// ── Check Login Result ──────────────────────────────

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

fn cleanup_title(mut commands: Commands, query: Query<Entity, With<TitleScreen>>) {
    for entity in &query {
        commands.entity(entity).despawn_recursive();
    }
}
