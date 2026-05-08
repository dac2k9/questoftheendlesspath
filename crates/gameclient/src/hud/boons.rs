//! Owned-boons strip — small color-coded chips below the top HUD bar
//! showing which boons the player has earned. Hovering a chip pops a
//! tooltip with the boon's name and description.
//!
//! No proper icons yet; chips use the boon's first letter on a
//! category-colored rounded square. Colors hint at theme:
//!   speed   = blue (Swift Boots, Trailblazer, Roadwise, Sprint)
//!   gold    = warm yellow (Goldfinger, Wealthy Start, Forge Discount)
//!   utility = teal/green (Treasure Sense, Cartographer)
//! Replace with art when we have it; the chip layout / tooltip flow
//! stays the same.

use bevy::prelude::*;

use crate::states::AppState;
use crate::terrain::tilemap::MyPlayerState;
use crate::GameFont;

pub struct BoonHudPlugin;

impl Plugin for BoonHudPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(OnEnter(AppState::InGame), spawn_boon_strip)
            .add_systems(
                Update,
                (rebuild_chips, update_tooltip)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

#[derive(Component)]
struct BoonStrip;

#[derive(Component)]
struct BoonChip(String);

#[derive(Component)]
struct BoonTooltip;

#[derive(Component)]
struct BoonTooltipText;

fn spawn_boon_strip(mut commands: Commands, font: Res<GameFont>) {
    // Strip — a row container under the top HUD bar (which is 28 px
    // tall). Empty until rebuild_chips fills it. Stays empty when the
    // player owns no boons; no visual at all.
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

    // Tooltip — single panel that re-uses the same node every frame,
    // toggled visible while a chip is hovered. Sits below the strip
    // so it doesn't cover the chips themselves.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(56.0),
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

fn chip_color(boon_id: &str) -> Color {
    match boon_id {
        "swift_boots" | "trailblazer" | "roadwise" | "sprint" => Color::srgb(0.45, 0.7, 1.0),
        "goldfinger" | "wealthy_start" | "forge_discount" => Color::srgb(1.0, 0.85, 0.3),
        _ => Color::srgb(0.45, 0.85, 0.7),
    }
}

fn rebuild_chips(
    mut commands: Commands,
    player: Res<MyPlayerState>,
    font: Res<GameFont>,
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
        let Some(boon) = questlib::boons::lookup(boon_id) else { continue };
        let letter = boon.name
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
            .to_string();
        let color = chip_color(boon_id);

        let chip_entity = commands
            .spawn((
                Button,
                Node {
                    width: Val::Px(20.0),
                    height: Val::Px(20.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(color),
                BorderRadius::all(Val::Px(4.0)),
                BoonChip(boon_id.clone()),
            ))
            .with_children(|c| {
                c.spawn((
                    Text::new(letter),
                    TextFont {
                        font: font.0.clone(),
                        font_size: 11.0,
                        ..default()
                    },
                    TextColor(Color::srgb(0.05, 0.04, 0.02)),
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

    // Find the first chip currently being hovered. Iterate every
    // frame (not Changed<Interaction>) so the tooltip stays open as
    // long as the cursor sits on a chip, not just on the
    // hover-entered frame.
    let hovered_id = chips.iter()
        .find_map(|(interaction, chip)| {
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
