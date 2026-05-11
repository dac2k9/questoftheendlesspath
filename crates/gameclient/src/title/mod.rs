use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::states::AppState;
use crate::{GameFont, GameSession, CHAMPIONS};

pub struct TitlePlugin;

impl Plugin for TitlePlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(PendingLogin::default())
            .insert_resource(WalkerPlayers::default())
            .insert_resource(TitleStage::default())
            .add_systems(Startup, (setup_font, spawn_title).chain())
            .add_systems(
                Update,
                (
                    fetch_walker_players,
                    handle_walker_click,
                    handle_champion_click,
                    check_login_result,
                ).run_if(in_state(AppState::Title)),
            )
            .add_systems(OnExit(AppState::Title), cleanup_title);
    }
}

// ── Resources ───────────────────────────────────────

#[derive(Resource, Default)]
struct PendingLogin {
    result: Arc<Mutex<Option<LoginResult>>>,
    waiting: bool,
}

#[derive(Clone)]
struct LoginResult {
    player_id: String,
    name: String,
    champion: String,
    /// Carried through so we can stash it in localStorage for
    /// auto-rejoin on the next page load.
    walker_uuid: String,
    /// World seed for the player's current adventure, returned by
    /// the server. Stored in `GameSession.map_seed` so `spawn_world`
    /// uses the right seed (chaos vs. frost_quest). Defaults to
    /// 12345 if the server didn't include it.
    map_seed: u64,
}

// ── localStorage session persistence ────────────────
//
// Save the four fields needed to rejoin: player_id, name, champion,
// walker_uuid. On the next page load `spawn_title` reads them, fires a
// silent /join with the same data, and skips straight to the InGame
// state so the user doesn't have to pick walker + champion again.

const LS_PLAYER_ID: &str = "qotep.player_id";
const LS_PLAYER_NAME: &str = "qotep.player_name";
const LS_CHAMPION: &str = "qotep.champion";
const LS_WALKER_UUID: &str = "qotep.walker_uuid";

fn save_session(player_id: &str, name: &str, champion: &str, walker_uuid: &str) {
    let Some(window) = web_sys::window() else { return };
    let Ok(Some(storage)) = window.local_storage() else { return };
    let _ = storage.set_item(LS_PLAYER_ID, player_id);
    let _ = storage.set_item(LS_PLAYER_NAME, name);
    let _ = storage.set_item(LS_CHAMPION, champion);
    let _ = storage.set_item(LS_WALKER_UUID, walker_uuid);
}

fn load_session() -> Option<(String, String, String, String)> {
    let window = web_sys::window()?;
    let storage = window.local_storage().ok()??;
    let pid = storage.get_item(LS_PLAYER_ID).ok()??;
    let name = storage.get_item(LS_PLAYER_NAME).ok()??;
    let champion = storage.get_item(LS_CHAMPION).ok()??;
    let walker_uuid = storage.get_item(LS_WALKER_UUID).ok()??;
    if pid.is_empty() || name.is_empty() || champion.is_empty() || walker_uuid.is_empty() {
        return None;
    }
    Some((pid, name, champion, walker_uuid))
}

/// Re-do a /join with the saved walker_uuid + name + champion. Server
/// recognizes the walker_uuid and returns the matching player_id (or a
/// fresh one if state was wiped). On success the result lands in the
/// shared `pending.result` slot, where `check_login_result` picks it up
/// and transitions to InGame just like a manual login.
fn kick_off_auto_login(
    name: String,
    champion: String,
    walker_uuid: String,
    result_ref: Arc<Mutex<Option<LoginResult>>>,
) {
    wasm_bindgen_futures::spawn_local(async move {
        let client = reqwest::Client::new();
        let url = crate::api_url("/join");
        let body = serde_json::json!({
            "name": name,
            "walker_uuid": walker_uuid,
            "champion": champion,
        });
        let Ok(resp) = client.post(&url).json(&body).send().await else { return };
        let Ok(text) = resp.text().await else { return };
        let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) else { return };
        let Some(pid) = data.get("player_id").and_then(|v| v.as_str()) else { return };
        let pname = data.get("name").and_then(|v| v.as_str()).unwrap_or(&name);
        let champ = data.get("champion").and_then(|v| v.as_str()).unwrap_or(&champion);
        let map_seed = data.get("map_seed").and_then(|v| v.as_u64()).unwrap_or(12345);
        if let Ok(mut lock) = result_ref.lock() {
            *lock = Some(LoginResult {
                player_id: pid.to_string(),
                name: pname.to_string(),
                champion: champ.to_string(),
                walker_uuid: walker_uuid.clone(),
                map_seed,
            });
        }
    });
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

/// Two-stage title flow: pick walker, then pick champion.
#[derive(Resource, Default)]
struct TitleStage {
    /// Once set, the walker picker is replaced by the champion picker.
    selected_walker: Option<(String, String)>, // (walker_id, display_name)
    /// Tracks whether the champion UI has been built so we only build it once.
    champion_ui_built: bool,
}

// ── Components ──────────────────────────────────────

#[derive(Component)]
struct TitleScreen;

#[derive(Component)]
struct WalkerStageRoot;

#[derive(Component)]
struct ChampionStageRoot;

#[derive(Component)]
struct PlayerListContainer;

#[derive(Component)]
struct WalkerButton(String, String); // (walker_id, name)

#[derive(Component)]
struct ChampionButton(String); // champion name

#[derive(Component)]
struct StatusText;

// ── Setup ───────────────────────────────────────────

fn setup_font(mut commands: Commands, mut fonts: ResMut<Assets<Font>>) {
    commands.spawn(Camera2d);
    let font_bytes = include_bytes!("../../assets/fonts/PressStart2P.ttf");
    let font = fonts.add(Font::try_from_bytes(font_bytes.to_vec()).expect("valid font"));
    commands.insert_resource(GameFont(font));
}

fn spawn_title(mut commands: Commands, font: Res<GameFont>, mut pending: ResMut<PendingLogin>) {
    let f = font.0.clone();

    commands.insert_resource(GameSession::default());

    // If we have a saved session in localStorage from a previous play
    // session, skip the walker + champion pickers entirely. Kick off a
    // silent /join with the saved data and show a "Reconnecting…"
    // screen; check_login_result will transition us to InGame as soon
    // as the response lands.
    if let Some((pid, name, champion, walker_uuid)) = load_session() {
        pending.waiting = true;
        kick_off_auto_login(
            name.clone(),
            champion.clone(),
            walker_uuid,
            pending.result.clone(),
        );
        commands.spawn((
            Node {
                width: Val::Percent(100.0),
                height: Val::Percent(100.0),
                flex_direction: FlexDirection::Column,
                justify_content: JustifyContent::Center,
                align_items: AlignItems::Center,
                row_gap: Val::Px(12.0),
                ..default()
            },
            BackgroundColor(Color::srgb(0.06, 0.06, 0.12)),
            TitleScreen,
        )).with_children(|parent| {
            parent.spawn((
                Text::new(format!("Welcome back, {}", name)),
                TextFont { font: f.clone(), font_size: 14.0, ..default() },
                TextColor(Color::srgb(1.0, 0.85, 0.3)),
            ));
            parent.spawn((
                Text::new(format!("({})", champion)),
                TextFont { font: f.clone(), font_size: 9.0, ..default() },
                TextColor(Color::srgb(0.55, 0.55, 0.65)),
            ));
            parent.spawn((
                Text::new("Reconnecting..."),
                TextFont { font: f.clone(), font_size: 8.0, ..default() },
                TextColor(Color::srgb(0.4, 0.4, 0.4)),
                Node { margin: UiRect::top(Val::Px(20.0)), ..default() },
            ));
        });
        log::info!("[auto-login] resuming session player_id={} champion={}", pid, champion);
        return;
    }

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
        WalkerStageRoot,
    )).with_children(|parent| {
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
        parent.spawn((
            Text::new("Select your Walker profile:"),
            TextFont { font: f.clone(), font_size: 9.0, ..default() },
            TextColor(Color::srgb(0.6, 0.6, 0.6)),
        ));
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
    stage: Res<TitleStage>,
) {
    // Only run while we're on the walker stage.
    if stage.selected_walker.is_some() { return; }

    if !walkers.fetch_started {
        walkers.fetch_started = true;
        let fetched = walkers.fetched.clone();
        wasm_bindgen_futures::spawn_local(async move {
            let client = reqwest::Client::new();
            let url = crate::api_url("/leaderboard");
            match client.get(&url)
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

    let new_players = {
        let Ok(mut lock) = walkers.fetched.lock() else { return };
        lock.take()
    };

    if let Some(players) = new_players {
        walkers.players = players;
        walkers.loaded = true;

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
                    WalkerButton(player.id.clone(), player.name.clone()),
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

// ── Handle Walker Click → switch to champion stage ──

fn handle_walker_click(
    mut commands: Commands,
    mouse: Res<ButtonInput<MouseButton>>,
    btn_q: Query<(&Interaction, &WalkerButton)>,
    walker_stage_q: Query<Entity, With<WalkerStageRoot>>,
    mut stage: ResMut<TitleStage>,
    font: Res<GameFont>,
    mut images: ResMut<Assets<Image>>,
    mut atlases: ResMut<Assets<TextureAtlasLayout>>,
) {
    if stage.selected_walker.is_some() { return; }
    if !mouse.just_pressed(MouseButton::Left) { return; }

    for (interaction, btn) in &btn_q {
        if !matches!(interaction, Interaction::Hovered | Interaction::Pressed) { continue; }

        stage.selected_walker = Some((btn.0.clone(), btn.1.clone()));

        // Despawn walker stage UI
        for entity in &walker_stage_q {
            commands.entity(entity).despawn_recursive();
        }

        // Build champion picker
        build_champion_stage(&mut commands, font.0.clone(), &btn.1, &mut images, &mut atlases);
        stage.champion_ui_built = true;
        return;
    }
}

fn build_champion_stage(
    commands: &mut Commands,
    font: Handle<Font>,
    player_name: &str,
    images: &mut ResMut<Assets<Image>>,
    atlases: &mut ResMut<Assets<TextureAtlasLayout>>,
) {
    // Build each champion's atlas handle once up-front so the image display nodes
    // all share one layout handle per sprite sheet.
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
        ChampionStageRoot,
    )).with_children(|parent| {
        parent.spawn((
            Text::new(format!("Welcome, {}", player_name)),
            TextFont { font: font.clone(), font_size: 12.0, ..default() },
            TextColor(Color::srgb(0.77, 0.64, 0.35)),
            Node { margin: UiRect::bottom(Val::Px(8.0)), ..default() },
        ));
        parent.spawn((
            Text::new("Choose your champion:"),
            TextFont { font: font.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(0.6, 0.6, 0.6)),
            Node { margin: UiRect::bottom(Val::Px(20.0)), ..default() },
        ));

        // Grid of champion buttons (4 per row, 2 rows).
        parent.spawn(Node {
            display: Display::Grid,
            grid_template_columns: vec![RepeatedGridTrack::flex(4, 1.0)],
            column_gap: Val::Px(12.0),
            row_gap: Val::Px(12.0),
            ..default()
        }).with_children(|grid| {
            for champ in CHAMPIONS {
                let info = crate::terrain::tilemap::champion_info(champ);
                let dyn_img = image::load_from_memory(info.bytes).expect("champion sprite");
                let rgba = dyn_img.to_rgba8();
                let (w, h) = rgba.dimensions();
                let tex = images.add(Image::new(
                    Extent3d { width: w, height: h, depth_or_array_layers: 1 },
                    TextureDimension::D2, rgba.into_raw(),
                    TextureFormat::Rgba8UnormSrgb, default(),
                ));
                let layout = atlases.add(crate::terrain::tilemap::champion_atlas_layout(&info));

                grid.spawn((
                    Button,
                    Node {
                        width: Val::Px(96.0),
                        height: Val::Px(112.0),
                        flex_direction: FlexDirection::Column,
                        justify_content: JustifyContent::Center,
                        align_items: AlignItems::Center,
                        padding: UiRect::all(Val::Px(8.0)),
                        border: UiRect::all(Val::Px(2.0)),
                        ..default()
                    },
                    BackgroundColor(Color::srgba(0.1, 0.1, 0.15, 0.9)),
                    BorderColor(Color::srgb(0.3, 0.3, 0.3)),
                    BorderRadius::all(Val::Px(6.0)),
                    ChampionButton(champ.to_string()),
                )).with_children(|btn| {
                    btn.spawn((
                        ImageNode::from_atlas_image(tex, TextureAtlas { layout, index: 0 }),
                        Node {
                            width: Val::Px(48.0),
                            height: Val::Px(48.0),
                            ..default()
                        },
                    ));
                    btn.spawn((
                        Text::new(*champ),
                        TextFont { font: font.clone(), font_size: 7.0, ..default() },
                        TextColor(Color::srgb(0.8, 0.7, 0.4)),
                        Node { margin: UiRect::top(Val::Px(8.0)), ..default() },
                    ));
                });
            }
        });

        parent.spawn((
            Text::new("Click a champion to begin"),
            TextFont { font: font.clone(), font_size: 7.0, ..default() },
            TextColor(Color::srgb(0.3, 0.3, 0.3)),
            Node { margin: UiRect::top(Val::Px(20.0)), ..default() },
        ));
    });
}

// ── Handle Champion Click → fire /join ──────────────

fn handle_champion_click(
    mouse: Res<ButtonInput<MouseButton>>,
    btn_q: Query<(&Interaction, &ChampionButton)>,
    stage: Res<TitleStage>,
    mut pending: ResMut<PendingLogin>,
) {
    if !mouse.just_pressed(MouseButton::Left) || pending.waiting { return; }
    let Some((walker_id, name)) = stage.selected_walker.clone() else { return };

    for (interaction, btn) in &btn_q {
        if !matches!(interaction, Interaction::Hovered | Interaction::Pressed) { continue; }

        let champion = btn.0.clone();
        let result_ref = pending.result.clone();
        pending.waiting = true;

        log::info!("[join] Joining as '{}' (walker: {}, champion: {})", name, walker_id, champion);

        wasm_bindgen_futures::spawn_local(async move {
            let client = reqwest::Client::new();
            let url = crate::api_url("/join");
            let body = serde_json::json!({
                "name": name,
                "walker_uuid": walker_id,
                "champion": champion,
            });
            match client.post(&url).json(&body).send().await {
                Ok(resp) => {
                    if let Ok(text) = resp.text().await {
                        log::info!("[join] Response: {}", text);
                        if let Ok(data) = serde_json::from_str::<serde_json::Value>(&text) {
                            if let Some(pid) = data.get("player_id").and_then(|v| v.as_str()) {
                                let pname = data.get("name").and_then(|v| v.as_str()).unwrap_or(&name);
                                let champ = data.get("champion").and_then(|v| v.as_str()).unwrap_or(&champion);
                                let map_seed = data.get("map_seed").and_then(|v| v.as_u64()).unwrap_or(12345);
                                if let Ok(mut lock) = result_ref.lock() {
                                    *lock = Some(LoginResult {
                                        player_id: pid.to_string(),
                                        name: pname.to_string(),
                                        champion: champ.to_string(),
                                        walker_uuid: walker_id.clone(),
                                        map_seed,
                                    });
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
    if let Some(r) = result {
        pending.waiting = false;
        // Save before mutating r — avoids cloning the strings twice.
        save_session(&r.player_id, &r.name, &r.champion, &r.walker_uuid);
        session.player_id = r.player_id;
        session.player_name = r.name;
        session.champion = r.champion;
        session.map_seed = r.map_seed;
        next_state.set(AppState::InGame);
    }
}

fn cleanup_title(mut commands: Commands, query: Query<Entity, With<TitleScreen>>) {
    for entity in &query {
        commands.entity(entity).despawn_recursive();
    }
}
