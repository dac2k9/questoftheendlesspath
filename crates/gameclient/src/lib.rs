use bevy::prelude::*;
use wasm_bindgen::prelude::*;

mod states;

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
        .add_systems(Startup, setup)
        .run();
}

#[derive(Resource)]
struct GameFont(Handle<Font>);

fn setup(mut commands: Commands, asset_server: Res<AssetServer>) {
    // Spawn 2D camera
    commands.spawn(Camera2d);

    // Load pixel font
    let font: Handle<Font> = asset_server.load("fonts/PressStart2P.ttf");
    commands.insert_resource(GameFont(font.clone()));

    // Title
    commands.spawn((
        Text2d::new("Quest of the\nEndless Path"),
        TextFont {
            font: font.clone(),
            font_size: 32.0,
            ..default()
        },
        TextColor(Color::srgb(0.77, 0.64, 0.35)),
        TextLayout::new_with_justify(JustifyText::Center),
        Transform::from_xyz(0.0, 60.0, 1.0),
    ));

    // Subtitle
    commands.spawn((
        Text2d::new("A cooperative treadmill adventure"),
        TextFont {
            font: font.clone(),
            font_size: 10.0,
            ..default()
        },
        TextColor(Color::srgb(0.5, 0.5, 0.5)),
        Transform::from_xyz(0.0, -10.0, 1.0),
    ));

    // Loading text
    commands.spawn((
        Text2d::new("Bevy + WASM running!"),
        TextFont {
            font,
            font_size: 12.0,
            ..default()
        },
        TextColor(Color::srgb(0.3, 0.8, 0.3)),
        Transform::from_xyz(0.0, -60.0, 1.0),
    ));
}
