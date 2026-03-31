use bevy::prelude::*;
use wasm_bindgen::prelude::*;

mod combat;
mod dialogue;
mod hud;
mod states;
pub mod supabase;
pub mod terrain;
mod title;

use states::AppState;

#[wasm_bindgen(start)]
pub fn start() {
    console_error_panic_hook::set_once();

    App::new()
        .add_plugins(
            DefaultPlugins
                .set(WindowPlugin {
                    primary_window: Some(Window {
                        title: "Quest of the Endless Path".to_string(),
                        resolution: (960.0, 540.0).into(),
                        canvas: Some("#game-canvas".to_string()),
                        fit_canvas_to_parent: true,
                        prevent_default_event_handling: true,
                        ..default()
                    }),
                    ..default()
                })
                .set(ImagePlugin::default_nearest())
                .set(AssetPlugin {
                    meta_check: bevy::asset::AssetMetaCheck::Never,
                    ..default()
                }),
        )
        .insert_resource(ClearColor(Color::srgb(
            0x1a as f32 / 255.0,
            0x1a as f32 / 255.0,
            0x2e as f32 / 255.0,
        )))
        .init_state::<AppState>()
        .add_plugins(title::TitlePlugin)
        .add_plugins(terrain::tilemap::TilemapPlugin)
        .add_plugins(supabase::SupabasePlugin)
        .add_plugins(hud::HudPlugin)
        .add_plugins(dialogue::DialoguePlugin)
        .add_plugins(combat::CombatPlugin)
        .run();
}

/// Shared font resource.
#[derive(Resource, Clone)]
pub struct GameFont(pub Handle<Font>);

/// Game session info set during title screen.
#[derive(Resource, Default)]
pub struct GameSession {
    pub game_id: String,
    pub player_id: String,
    pub player_name: String,
    pub join_code: String,
}
