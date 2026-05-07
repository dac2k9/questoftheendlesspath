//! Client version check — shows a "New version available" banner with a
//! Refresh button when the server's current cache-bust version (from
//! index.html's `?v=NNN`) differs from this client's compiled-in
//! CLIENT_VERSION.
//!
//! Bump CLIENT_VERSION alongside the `?v=` line in index.html when you
//! rebuild WASM, or stale clients will think they're always out of date.
//! (Or build.rs automation later — for now, manual.)

use bevy::prelude::*;
use std::sync::{Arc, Mutex};
use wasm_bindgen::JsValue;

use crate::states::AppState;
use crate::GameFont;

fn log(s: &str) {
    web_sys::console::log_1(&JsValue::from_str(s));
}

/// Must match the `?v=NNN` number in crates/gameclient/index.html.
/// Bumped together by hand on every WASM rebuild.
pub const CLIENT_VERSION: u32 = 321;

/// How often to poll the server for version. 60s is frequent enough that
/// players notice a deploy within a minute without hammering the server.
const POLL_INTERVAL_S: f32 = 60.0;

pub struct VersionPlugin;

impl Plugin for VersionPlugin {
    fn build(&self, app: &mut App) {
        app
            .init_resource::<VersionState>()
            .add_systems(OnEnter(AppState::InGame), spawn_banner)
            .add_systems(
                Update,
                (tick_poll, update_banner_visibility, handle_refresh_click)
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Resource)]
struct VersionState {
    server_version: Arc<Mutex<Option<u32>>>,
    poll_timer: f32,
    /// `true` once we've discovered a mismatch. Sticky — we don't flip
    /// it back to false if a subsequent poll returns matching numbers
    /// (race: user hasn't refreshed, so the banner must persist).
    mismatch: bool,
}

impl Default for VersionState {
    fn default() -> Self {
        Self {
            server_version: Arc::new(Mutex::new(None)),
            // Fire the first poll a couple seconds after InGame rather
            // than immediately — avoids competing with the initial world
            // asset fetches.
            poll_timer: POLL_INTERVAL_S - 3.0,
            mismatch: false,
        }
    }
}

#[derive(Component)]
struct UpdateBanner;

#[derive(Component)]
struct RefreshButton;

fn spawn_banner(mut commands: Commands, font: Res<GameFont>) {
    log("[version] spawn_banner ran (entered InGame)");
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            // Below the 28 px top HUD bar — previously at top=4 the banner
            // was hidden behind the semi-opaque HUD background.
            top: Val::Px(34.0),
            left: Val::Percent(50.0),
            margin: UiRect::left(Val::Px(-160.0)),  // centered: half of width
            width: Val::Px(320.0),
            padding: UiRect::axes(Val::Px(10.0), Val::Px(6.0)),
            border: UiRect::all(Val::Px(2.0)),
            flex_direction: FlexDirection::Row,
            justify_content: JustifyContent::SpaceBetween,
            align_items: AlignItems::Center,
            column_gap: Val::Px(10.0),
            ..default()
        },
        // Explicit high z so the banner renders above any other absolute
        // UI node regardless of spawn order.
        ZIndex(100),
        BackgroundColor(Color::srgba(0.15, 0.10, 0.02, 0.95)),
        BorderColor(Color::srgb(0.95, 0.75, 0.25)),
        BorderRadius::all(Val::Px(4.0)),
        Visibility::Hidden,
        UpdateBanner,
    )).with_children(|parent| {
        parent.spawn((
            Text::new("New version available"),
            TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
            TextColor(Color::srgb(1.0, 0.95, 0.7)),
        ));
        parent.spawn((
            Button,
            Node {
                padding: UiRect::axes(Val::Px(8.0), Val::Px(3.0)),
                border: UiRect::all(Val::Px(1.0)),
                ..default()
            },
            BackgroundColor(Color::srgb(0.95, 0.75, 0.25)),
            BorderColor(Color::srgb(0.5, 0.4, 0.1)),
            BorderRadius::all(Val::Px(3.0)),
            RefreshButton,
        )).with_children(|b| {
            b.spawn((
                Text::new("Refresh"),
                TextFont { font: font.0.clone(), font_size: 10.0, ..default() },
                TextColor(Color::srgb(0.15, 0.10, 0.02)),
            ));
        });
    });
}

fn tick_poll(
    time: Res<Time>,
    mut state: ResMut<VersionState>,
) {
    state.poll_timer += time.delta_secs();
    if state.poll_timer < POLL_INTERVAL_S { return; }
    state.poll_timer = 0.0;
    kick_off_fetch(state.server_version.clone());
}

fn kick_off_fetch(slot: Arc<Mutex<Option<u32>>>) {
    wasm_bindgen_futures::spawn_local(async move {
        // Absolute URL via api_url(): reqwest's WASM backend rejects
        // relative paths with `RelativeUrlWithoutBase`, which used to
        // make this poll silently fail forever (banner never fired).
        let url = crate::api_url("/version");
        let Ok(resp) = reqwest::Client::new().get(&url).send().await else { return };
        let Ok(text) = resp.text().await else { return };
        let v = text.find("\"version\":")
            .and_then(|i| {
                let tail = &text[i + "\"version\":".len()..];
                let n: String = tail.chars()
                    .skip_while(|c| c.is_whitespace())
                    .take_while(|c| c.is_ascii_digit())
                    .collect();
                n.parse::<u32>().ok()
            });
        if let (Some(v), Ok(mut g)) = (v, slot.lock()) { *g = Some(v); }
    });
}

fn update_banner_visibility(
    mut state: ResMut<VersionState>,
    mut banner_q: Query<&mut Visibility, With<UpdateBanner>>,
) {
    // Flip mismatch on if we see a server version higher than our own.
    // Lower server version (unlikely) is ignored — could happen during
    // rollbacks and shouldn't trigger a refresh nag.
    if !state.mismatch {
        let srv = state.server_version.lock().ok().and_then(|g| *g);
        if let Some(v) = srv {
            if v > CLIENT_VERSION {
                state.mismatch = true;
                log(&format!("[version] mismatch detected — banner should appear (client={}, server={})", CLIENT_VERSION, v));
            }
        }
    }
    let want_show = if state.mismatch { Visibility::Visible } else { Visibility::Hidden };
    let mut count = 0;
    for mut vis in &mut banner_q {
        count += 1;
        if *vis != want_show { *vis = want_show; }
    }
    if state.mismatch && count == 0 {
        log("[version] mismatch=true but found 0 banner entities — spawn_banner likely never ran");
    }
}

fn handle_refresh_click(
    mouse: Res<ButtonInput<MouseButton>>,
    btn_q: Query<&Interaction, With<RefreshButton>>,
) {
    if !mouse.just_pressed(MouseButton::Left) { return; }
    for interaction in &btn_q {
        if matches!(interaction, Interaction::Hovered | Interaction::Pressed) {
            if let Some(window) = web_sys::window() {
                // Force from server; bypass browser cache to pick up the new JS/WASM.
                let _ = window.location().reload_with_forceget(true);
            }
        }
    }
}
