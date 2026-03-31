pub mod poll;
pub mod ui;

use std::sync::{Arc, Mutex};

use bevy::prelude::*;

use crate::states::AppState;

pub struct CombatPlugin;

impl Plugin for CombatPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(CombatUiState::default())
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
