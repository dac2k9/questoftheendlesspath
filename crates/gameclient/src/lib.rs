use bevy::prelude::*;
use wasm_bindgen::prelude::*;

mod states;

use states::AppState;

#[wasm_bindgen(start)]
pub fn start() {
    // Initialize browser console logging
    #[cfg(target_arch = "wasm32")]
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
                .set(ImagePlugin::default_nearest()),
        )
        .insert_resource(ClearColor(Color::srgb(
            0x1a as f32 / 255.0,
            0x1a as f32 / 255.0,
            0x2e as f32 / 255.0,
        )))
        .init_state::<AppState>()
        .add_systems(Startup, setup)
        .add_systems(Update, title_system.run_if(in_state(AppState::Title)))
        .run();
}

fn setup(mut commands: Commands) {
    // Spawn 2D camera
    commands.spawn(Camera2d);

    // Placeholder title text
    commands.spawn((
        Text2d::new("Quest of the\nEndless Path"),
        TextFont {
            font_size: 48.0,
            ..default()
        },
        TextColor(Color::srgb(0.77, 0.64, 0.35)),
        Transform::from_xyz(0.0, 80.0, 0.0),
        TitleText,
    ));

    commands.spawn((
        Text2d::new("Loading..."),
        TextFont {
            font_size: 16.0,
            ..default()
        },
        TextColor(Color::srgb(0.5, 0.5, 0.5)),
        Transform::from_xyz(0.0, -40.0, 0.0),
        TitleText,
    ));
}

#[derive(Component)]
struct TitleText;

fn title_system() {
    // Placeholder — will be replaced with actual title screen logic
}
