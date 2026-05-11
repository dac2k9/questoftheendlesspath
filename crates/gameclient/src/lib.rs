use bevy::prelude::*;
use wasm_bindgen::prelude::*;

mod adventure_menu;
mod ambient;
mod boon_picker;
mod combat;
mod daynight;
mod dialogue;
mod entities;
mod hud;
mod music;
mod player_shadow;
mod sfx;
mod version;
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
        // Match the fog of war color (15, 15, 25) so the area outside
        // the world rectangle blends seamlessly with fogged tiles when
        // the camera is zoomed out beyond the map bounds.
        .insert_resource(ClearColor(Color::srgb(
            15.0 / 255.0,
            15.0 / 255.0,
            25.0 / 255.0,
        )))
        .init_state::<AppState>()
        .add_plugins(title::TitlePlugin)
        .add_plugins(terrain::tilemap::TilemapPlugin)
        .add_plugins(terrain::interior::InteriorPlugin)
        .add_plugins(terrain::lighting::LightingPlugin)
        .add_plugins(terrain::water_shader::WaterShaderPlugin)
        // terrain_lighting overlay was the previous standalone Phong
        // pass on top of the ground. The ground material now handles
        // its own lighting in procedural_ground.wgsl, so the overlay
        // would double-shade if also enabled. Plugin is intentionally
        // not registered; the file remains for reference / quick
        // toggle if we need a side-by-side comparison.
        .add_plugins(terrain::night_lights::NightLightsPlugin)
        .add_plugins(terrain::procedural_ground::ProceduralGroundPlugin)
        .add_plugins(terrain::fog_shader::FogShaderPlugin)
        .add_plugins(daynight::DayNightPlugin)
        .add_plugins(supabase::SupabasePlugin)
        .add_plugins(hud::HudPlugin)
        .add_plugins(dialogue::DialoguePlugin)
        .add_plugins(combat::CombatPlugin)
        .add_plugins(music::MusicPlugin)
        .add_plugins(sfx::SfxPlugin)
        .add_plugins(ambient::AmbientPlugin)
        // Player shadow disabled — the sprite-skew approach reads odd
        // when the shadow is long (jagged scaling, visible padding gap
        // hard to compensate cleanly across animation frames). Code
        // kept for now so we can revisit; re-enable by adding the
        // plugin back here.
        // .add_plugins(player_shadow::PlayerShadowPlugin)
        .add_plugins(entities::EntitiesPlugin)
        .add_plugins(boon_picker::BoonPickerPlugin)
        .add_plugins(adventure_menu::AdventureMenuPlugin)
        .add_plugins(version::VersionPlugin)
        .insert_resource(UiHovered(false))
        .add_systems(Update, (detect_ui_hover, update_cursor))
        .run();
}

/// Get the server base URL from the current page origin.
/// Returns e.g. "http://localhost:3001" or "https://questoftheendlesspath.onrender.com"
pub fn api_url(path: &str) -> String {
    let base = web_sys::window()
        .and_then(|w| w.location().origin().ok())
        .unwrap_or_else(|| "http://localhost:3001".to_string());
    format!("{}{}", base, path)
}

/// True when the mouse is over any UI element with Interaction.
/// Map clicks should be suppressed when this is true.
#[derive(Resource)]
pub struct UiHovered(pub bool);

/// Detect if any UI node is hovered — runs before other systems.
fn detect_ui_hover(
    mut ui_hovered: ResMut<UiHovered>,
    interactions: Query<&Interaction>,
) {
    ui_hovered.0 = interactions.iter().any(|i| matches!(i, Interaction::Hovered | Interaction::Pressed));
}

/// Set canvas cursor to pointer when hovering any Button.
fn update_cursor(
    ui_hovered: Res<UiHovered>,
    mut last_cursor: Local<bool>,
) {
    let hovering = ui_hovered.0;
    if hovering == *last_cursor { return; }
    *last_cursor = hovering;
    let cursor = if hovering { "pointer" } else { "default" };
    if let Some(window) = web_sys::window() {
        if let Some(doc) = window.document() {
            if let Some(canvas) = doc.get_element_by_id("game-canvas") {
                let _ = canvas.unchecked_ref::<web_sys::HtmlElement>().style().set_property("cursor", cursor);
            }
        }
    }
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
    /// Character sprite chosen on title screen (e.g. "Katan"). See CHAMPIONS.
    pub champion: String,
}

/// Available player characters — must match asset filenames in assets/sprites/.
pub const CHAMPIONS: &[&str] = &[
    "Katan", "Zhinja", "Arthax", "Börg", "Gangblanc", "Grum", "Kanji", "Okomo",
];
