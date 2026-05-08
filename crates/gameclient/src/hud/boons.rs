//! Owned-boons strip — small icons below the top HUD bar showing
//! which boons the player has earned. Hovering an icon pops a
//! tooltip with the boon's name and description.
//!
//! Icons are 32×32 PNGs generated via the pixellab tool, embedded
//! at compile time via `include_bytes!` so the WASM bundle is
//! self-contained (no asset-loading roundtrip on enter-game).

use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use std::collections::HashMap;

use crate::states::AppState;
use crate::terrain::tilemap::MyPlayerState;
use crate::GameFont;

pub struct BoonHudPlugin;

impl Plugin for BoonHudPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<BoonIcons>()
            .add_systems(
                OnEnter(AppState::InGame),
                (load_boon_icons, spawn_boon_strip).chain(),
            )
            .add_systems(
                Update,
                (rebuild_chips, update_tooltip)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Resource, Default)]
struct BoonIcons {
    by_id: HashMap<String, Handle<Image>>,
}

#[derive(Component)]
struct BoonStrip;

#[derive(Component)]
struct BoonChip(String);

#[derive(Component)]
struct BoonTooltip;

#[derive(Component)]
struct BoonTooltipText;

fn load_boon_icons(mut icons: ResMut<BoonIcons>, mut images: ResMut<Assets<Image>>) {
    // Embed every boon's icon at compile time. Order matches the
    // questlib::boons catalog. Adding a new boon = generate the
    // icon, drop the file at the path below, and add a row here.
    let entries: &[(&str, &[u8])] = &[
        ("swift_boots",    include_bytes!("../../assets/generated/boons/swift_boots.png")),
        ("trailblazer",    include_bytes!("../../assets/generated/boons/trailblazer.png")),
        ("roadwise",       include_bytes!("../../assets/generated/boons/roadwise.png")),
        ("sprint",         include_bytes!("../../assets/generated/boons/sprint.png")),
        ("goldfinger",     include_bytes!("../../assets/generated/boons/goldfinger.png")),
        ("wealthy_start",  include_bytes!("../../assets/generated/boons/wealthy_start.png")),
        ("treasure_sense", include_bytes!("../../assets/generated/boons/treasure_sense.png")),
        ("forge_discount", include_bytes!("../../assets/generated/boons/forge_discount.png")),
        ("cartographer",   include_bytes!("../../assets/generated/boons/cartographer.png")),
    ];

    for (id, bytes) in entries {
        let dyn_img = match image::load_from_memory(bytes) {
            Ok(i) => i,
            Err(e) => {
                log::warn!("[boons] failed to load {} icon: {}", id, e);
                continue;
            }
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
        icons.by_id.insert(id.to_string(), images.add(img));
    }
    log::info!("[boons] loaded {} icons", icons.by_id.len());
}

fn spawn_boon_strip(mut commands: Commands, font: Res<GameFont>) {
    // Strip — empty container until rebuild_chips fills it. Sits
    // under the top HUD bar (28 px tall).
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(32.0),
            left: Val::Px(8.0),
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(4.0),
            ..default()
        },
        ZIndex(15),
        BoonStrip,
    ));

    // Tooltip — single panel, toggled visible while a chip is hovered.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(58.0),
            left: Val::Px(8.0),
            padding: UiRect::all(Val::Px(6.0)),
            border: UiRect::all(Val::Px(1.0)),
            max_width: Val::Px(220.0),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(2.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.05, 0.04, 0.02, 0.95)),
        BorderColor(Color::srgb(0.85, 0.65, 0.20)),
        BorderRadius::all(Val::Px(4.0)),
        ZIndex(40),
        Visibility::Hidden,
        BoonTooltip,
    )).with_children(|tt| {
        tt.spawn((
            Text::new(""),
            TextFont {
                font: font.0.clone(),
                font_size: 9.0,
                ..default()
            },
            TextColor(Color::srgb(1.0, 0.92, 0.7)),
            BoonTooltipText,
        ));
    });
}

fn rebuild_chips(
    mut commands: Commands,
    player: Res<MyPlayerState>,
    icons: Res<BoonIcons>,
    strip_q: Query<Entity, With<BoonStrip>>,
    chips_q: Query<Entity, With<BoonChip>>,
    mut last_owned: Local<Vec<String>>,
) {
    if player.boons == *last_owned {
        return;
    }
    *last_owned = player.boons.clone();

    let Ok(strip) = strip_q.get_single() else { return };
    for chip in &chips_q {
        commands.entity(chip).despawn_recursive();
    }

    for boon_id in &player.boons {
        let Some(handle) = icons.by_id.get(boon_id) else {
            // Unknown boon id (catalog drift between server / client).
            // Skip silently — server validation prevents bad ids
            // anyway, this only fires if a generated icon goes missing.
            continue;
        };

        let chip_entity = commands
            .spawn((
                Button,
                Node {
                    width: Val::Px(24.0),
                    height: Val::Px(24.0),
                    padding: UiRect::all(Val::Px(1.0)),
                    ..default()
                },
                BackgroundColor(Color::srgba(0.05, 0.05, 0.10, 0.55)),
                BorderRadius::all(Val::Px(4.0)),
                BoonChip(boon_id.clone()),
            ))
            .with_children(|c| {
                c.spawn((
                    Node {
                        width: Val::Percent(100.0),
                        height: Val::Percent(100.0),
                        ..default()
                    },
                    ImageNode::new(handle.clone()),
                ));
            })
            .id();
        commands.entity(strip).add_child(chip_entity);
    }
}

fn update_tooltip(
    chips: Query<(&Interaction, &BoonChip)>,
    mut tooltip_q: Query<&mut Visibility, With<BoonTooltip>>,
    mut text_q: Query<&mut Text, With<BoonTooltipText>>,
) {
    let Ok(mut vis) = tooltip_q.get_single_mut() else { return };

    // Iterate every frame (not Changed<Interaction>) so the tooltip
    // stays open as long as the cursor sits on a chip.
    let hovered_id = chips.iter().find_map(|(interaction, chip)| {
        matches!(interaction, Interaction::Hovered | Interaction::Pressed)
            .then(|| chip.0.clone())
    });

    if let Some(boon_id) = hovered_id {
        if let Some(boon) = questlib::boons::lookup(&boon_id) {
            if let Ok(mut text) = text_q.get_single_mut() {
                *text = Text::new(format!("{}\n{}", boon.name, boon.description));
            }
            *vis = Visibility::Visible;
            return;
        }
    }
    *vis = Visibility::Hidden;
}
