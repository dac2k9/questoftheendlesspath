pub mod poll;
pub mod ui;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};

use crate::states::AppState;

pub struct CombatPlugin;

impl Plugin for CombatPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(CombatUiState::default())
            .init_resource::<BossPortraits>()
            .add_systems(OnEnter(AppState::InGame), load_boss_portraits)
            .add_systems(
                Update,
                (
                    poll::poll_combat_state,
                    ui::manage_combat_overlay,
                    ui::update_combat_ui,
                    ui::handle_combat_input,
                )
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Boss portrait images keyed by theme tag ("frost" / "flame" /
/// "shadow" / "storm"). `ui::match_boss_portrait` resolves an enemy
/// name to one of these tags via substring matching, so authored
/// boss names like "Frost Queen" or "Shadow Lich" land on the right
/// portrait without per-name plumbing.
#[derive(Resource, Default)]
pub struct BossPortraits {
    pub by_theme: HashMap<String, Handle<Image>>,
}

fn load_boss_portraits(
    mut portraits: ResMut<BossPortraits>,
    mut images: ResMut<Assets<Image>>,
) {
    let entries: &[(&str, &[u8])] = &[
        ("frost",  include_bytes!("../../assets/generated/portraits/boss_frost.png")),
        ("flame",  include_bytes!("../../assets/generated/portraits/boss_flame.png")),
        ("shadow", include_bytes!("../../assets/generated/portraits/boss_shadow.png")),
        ("storm",  include_bytes!("../../assets/generated/portraits/boss_storm.png")),
    ];
    for (tag, bytes) in entries {
        let Ok(dyn_img) = image::load_from_memory(bytes) else {
            log::warn!("[boss-portraits] failed to load {}", tag);
            continue;
        };
        let rgba = dyn_img.to_rgba8();
        let (w, h) = rgba.dimensions();
        let img = Image::new(
            Extent3d { width: w, height: h, depth_or_array_layers: 1 },
            TextureDimension::D2,
            rgba.into_raw(),
            TextureFormat::Rgba8UnormSrgb,
            default(),
        );
        portraits.by_theme.insert(tag.to_string(), images.add(img));
    }
    log::info!("[boss-portraits] loaded {} portraits", portraits.by_theme.len());
}

/// Client-side combat state, updated from server polls.
#[derive(Resource)]
pub struct CombatUiState {
    pub active: bool,
    pub state: Option<questlib::combat::CombatState>,
    /// Client-predicted player charge (smooth animation between polls).
    pub local_player_charge: f32,
    /// Client-predicted enemy charge.
    pub local_enemy_charge: f32,
    /// Shared with async poll task — Some means new state arrived.
    pub fetched: Arc<Mutex<Option<questlib::combat::CombatState>>>,
    /// Signal from async task that server returned null (no combat).
    pub server_cleared: Arc<Mutex<bool>>,
    /// Timer for polling interval.
    pub poll_timer: f32,
    /// True while waiting for action response.
    pub action_pending: bool,
}

impl Default for CombatUiState {
    fn default() -> Self {
        Self {
            active: false,
            state: None,
            local_player_charge: 0.0,
            local_enemy_charge: 0.0,
            fetched: Arc::new(Mutex::new(None)),
            server_cleared: Arc::new(Mutex::new(false)),
            poll_timer: 0.0,
            action_pending: false,
        }
    }
}

/// Public helper for run conditions — returns true when combat overlay is active.
pub fn combat_active(state: Res<CombatUiState>) -> bool {
    state.active
}
