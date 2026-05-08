//! Owned-boons strip — small icons below the top HUD bar showing
//! which boons the player has earned. Hovering an icon pops a
//! tooltip with the boon's name and description.
//!
//! Two strips stacked vertically:
//!   - Permanent boons (top: 32) — the 9-icon catalog from
//!     questlib::boons. Persistent; survives adventure resets.
//!   - Temporary buffs (top: 60) — from consumed potions etc.
//!     Pull off the player's active_buffs list. Tooltip shows the
//!     buff's source-item name, % effect, and time remaining.
//!
//! Permanent icons are 32×32 PNGs embedded via include_bytes!.
//! Temp buff chips use a placeholder colored letter for now (no
//! buff icons generated yet — same path as permanent originally).

use bevy::prelude::*;
use bevy::render::render_resource::{Extent3d, TextureDimension, TextureFormat};
use bevy::text::LineBreak;
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
                (load_boon_icons, spawn_strips).chain(),
            )
            .add_systems(
                Update,
                (rebuild_boon_chips, rebuild_buff_chips, update_tooltip)
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
struct BuffStrip;

#[derive(Component)]
struct BoonChip(String);

#[derive(Component, Clone)]
struct BuffChip {
    kind: String,
    multiplier: f32,
    expires_unix: u64,
    source_item: String,
}

#[derive(Component)]
struct BoonTooltip;

#[derive(Component)]
struct BoonTooltipName;

#[derive(Component)]
struct BoonTooltipDesc;

fn load_boon_icons(mut icons: ResMut<BoonIcons>, mut images: ResMut<Assets<Image>>) {
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

fn spawn_strips(mut commands: Commands, font: Res<GameFont>) {
    // Permanent boon strip — first row, just below the top HUD bar.
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

    // Temp-buff strip — second row, below the permanent strip.
    // Empty when no active buffs, so it just doesn't render.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(60.0),
            left: Val::Px(8.0),
            flex_direction: FlexDirection::Row,
            column_gap: Val::Px(4.0),
            ..default()
        },
        ZIndex(15),
        BuffStrip,
    ));

    // Tooltip — single panel; works for both chip kinds. Sits below
    // both strips so it never covers a chip you might want to hover.
    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(92.0),
            left: Val::Px(8.0),
            width: Val::Px(280.0),
            padding: UiRect::all(Val::Px(8.0)),
            border: UiRect::all(Val::Px(1.0)),
            flex_direction: FlexDirection::Column,
            row_gap: Val::Px(3.0),
            ..default()
        },
        BackgroundColor(Color::srgba(0.05, 0.04, 0.02, 0.95)),
        BorderColor(Color::srgb(0.85, 0.65, 0.20)),
        BorderRadius::all(Val::Px(4.0)),
        ZIndex(40),
        Visibility::Hidden,
        BoonTooltip,
    ))
    .with_children(|tt| {
        tt.spawn((
            Text::new(""),
            TextFont { font: font.0.clone(), font_size: 11.0, ..default() },
            TextColor(Color::srgb(1.0, 0.92, 0.55)),
            BoonTooltipName,
        ));
        tt.spawn((
            Text::new(""),
            TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
            TextColor(Color::srgb(0.9, 0.88, 0.78)),
            TextLayout::new_with_linebreak(LineBreak::WordBoundary),
            BoonTooltipDesc,
        ));
    });
}

fn rebuild_boon_chips(
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
        let Some(handle) = icons.by_id.get(boon_id) else { continue };
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

fn rebuild_buff_chips(
    mut commands: Commands,
    player: Res<MyPlayerState>,
    font: Res<GameFont>,
    strip_q: Query<Entity, With<BuffStrip>>,
    chips_q: Query<Entity, With<BuffChip>>,
    mut last_buffs: Local<Vec<questlib::items::ActiveBuff>>,
) {
    // Compare on the fields that affect what we render — `expires_unix`
    // is part of equality, so a poll that just refreshes the list with
    // the same buffs won't trigger a respawn. The countdown in the
    // tooltip recomputes from current time anyway.
    if player.active_buffs == *last_buffs {
        return;
    }
    *last_buffs = player.active_buffs.clone();

    let Ok(strip) = strip_q.get_single() else { return };
    for chip in &chips_q {
        commands.entity(chip).despawn_recursive();
    }

    for buff in &player.active_buffs {
        // Placeholder icon: first letter of the source item id (e.g.
        // "speed_potion" → "S") on a violet chip, distinct from the
        // permanent boons' dark-blue background. Real buff icons can
        // slot in later.
        let letter = buff.source_item
            .chars()
            .next()
            .unwrap_or('?')
            .to_ascii_uppercase()
            .to_string();
        let chip_entity = commands
            .spawn((
                Button,
                Node {
                    width: Val::Px(24.0),
                    height: Val::Px(24.0),
                    justify_content: JustifyContent::Center,
                    align_items: AlignItems::Center,
                    ..default()
                },
                BackgroundColor(Color::srgba(0.45, 0.18, 0.55, 0.85)),
                BorderRadius::all(Val::Px(4.0)),
                BuffChip {
                    kind: buff.kind.clone(),
                    multiplier: buff.multiplier,
                    expires_unix: buff.expires_unix,
                    source_item: buff.source_item.clone(),
                },
            ))
            .with_children(|c| {
                c.spawn((
                    Text::new(letter),
                    TextFont { font: font.0.clone(), font_size: 11.0, ..default() },
                    TextColor(Color::srgb(1.0, 0.95, 0.85)),
                ));
            })
            .id();
        commands.entity(strip).add_child(chip_entity);
    }
}

fn update_tooltip(
    boon_chips: Query<(&Interaction, &BoonChip), Without<BuffChip>>,
    buff_chips: Query<(&Interaction, &BuffChip), Without<BoonChip>>,
    catalog: Option<Res<crate::hud::ItemCatalogRes>>,
    mut tooltip_q: Query<&mut Visibility, With<BoonTooltip>>,
    mut name_q: Query<&mut Text, (With<BoonTooltipName>, Without<BoonTooltipDesc>)>,
    mut desc_q: Query<&mut Text, (With<BoonTooltipDesc>, Without<BoonTooltipName>)>,
) {
    let Ok(mut vis) = tooltip_q.get_single_mut() else { return };

    // Permanent boon hover wins if both kinds are hovered (impossible
    // in practice — the strips don't overlap — but defensive).
    let boon_hover = boon_chips.iter().find_map(|(i, c)| {
        matches!(i, Interaction::Hovered | Interaction::Pressed).then(|| c.0.clone())
    });
    let buff_hover = buff_chips.iter().find_map(|(i, c)| {
        matches!(i, Interaction::Hovered | Interaction::Pressed).then(|| c.clone())
    });

    if let Some(boon_id) = boon_hover {
        if let Some(boon) = questlib::boons::lookup(&boon_id) {
            if let Ok(mut name) = name_q.get_single_mut() {
                *name = Text::new(boon.name);
            }
            if let Ok(mut desc) = desc_q.get_single_mut() {
                *desc = Text::new(boon.description);
            }
            *vis = Visibility::Visible;
            return;
        }
    }

    if let Some(buff) = buff_hover {
        // Resolve the item's display name if the catalog knows it,
        // else fall back to the raw item id.
        let display = catalog
            .as_ref()
            .and_then(|c| c.0.get(&buff.source_item))
            .map(|d| d.display_name.clone())
            .unwrap_or_else(|| buff.source_item.clone());
        let pct = ((buff.multiplier - 1.0) * 100.0).round() as i32;
        let now_s = (js_sys::Date::now() / 1000.0) as u64;
        let remaining = buff.expires_unix.saturating_sub(now_s);
        let mins = remaining / 60;
        let secs = remaining % 60;
        let time_str = if mins > 0 {
            format!("{}m {}s left", mins, secs)
        } else {
            format!("{}s left", secs)
        };
        let body = format!(
            "{:+}% {} ({})",
            pct, buff.kind, time_str
        );
        if let Ok(mut name) = name_q.get_single_mut() {
            *name = Text::new(display);
        }
        if let Ok(mut desc) = desc_q.get_single_mut() {
            *desc = Text::new(body);
        }
        *vis = Visibility::Visible;
        return;
    }

    *vis = Visibility::Hidden;
}
