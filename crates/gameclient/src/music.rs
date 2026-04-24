//! Music system — context-aware music with crossfading.
//!
//! Uses two HTML audio elements for seamless crossfading between tracks.
//! Music context is determined by biome, combat state, and walking speed.

use bevy::prelude::*;
use serde::Deserialize;
use wasm_bindgen::JsCast;

use crate::states::AppState;
use crate::terrain::tilemap::MyPlayerState;
use crate::terrain::world::WorldGrid;

pub struct MusicPlugin;

impl Plugin for MusicPlugin {
    fn build(&self, app: &mut App) {
        let catalog = load_catalog();
        app.insert_resource(MusicState::default())
            .insert_resource(catalog)
            .add_systems(Update, (volume_controls, update_music, update_volume_display, update_music_button));
    }
}

// ── Data ────────────────────────────────────────────

#[derive(Debug, Clone, Deserialize)]
pub struct TrackDef {
    pub id: String,
    pub file: String,
    pub context: String,
    pub recommended_kmh: f32,
    pub volume: f32,
    #[serde(default = "default_true")]
    pub r#loop: bool,
}

fn default_true() -> bool { true }

#[derive(Resource)]
struct MusicCatalog {
    tracks: Vec<TrackDef>,
}

fn load_catalog() -> MusicCatalog {
    let json = include_str!("../assets/music/tracks.json");
    #[derive(Deserialize)]
    struct Data { tracks: Vec<TrackDef> }
    let data: Data = serde_json::from_str(json).unwrap_or(Data { tracks: vec![] });
    MusicCatalog { tracks: data.tracks }
}

// ── Music Context ───────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
enum MusicContext {
    Title,
    Village,
    Grassland,
    Forest,
    Mountain,
    Swamp,
    /// Inside any interior (cave, dungeon, castle). Overrides biome so
    /// caves don't play "Mountain" music just because the cave entrance
    /// happens to sit on a mountain tile in the overworld.
    Interior,
    Combat,
    Boss,
    Victory,
    Silent,
}

impl MusicContext {
    fn as_str(&self) -> &str {
        match self {
            Self::Title => "title",
            Self::Village => "village",
            Self::Grassland => "grassland",
            Self::Forest => "forest",
            Self::Mountain => "mountain",
            Self::Swamp => "swamp",
            Self::Interior => "interior",
            Self::Combat => "combat",
            Self::Boss => "boss",
            Self::Victory => "victory",
            Self::Silent => "",
        }
    }
}

// ── State ───────────────────────────────────────────

#[derive(Resource)]
pub struct MusicState {
    /// Currently playing track ID.
    current_track: Option<String>,
    /// Current music context.
    current_context: MusicContext,
    /// The two audio channels for crossfading.
    channel_a: Option<web_sys::HtmlAudioElement>,
    channel_b: Option<web_sys::HtmlAudioElement>,
    /// Which channel is active (true = A, false = B).
    active_is_a: bool,
    /// Crossfade progress (0.0 = old track full, 1.0 = new track full).
    fade_progress: f32,
    /// Whether we're currently crossfading.
    fading: bool,
    /// Target volume for the new track.
    target_volume: f32,
    /// Cooldown to prevent rapid switching.
    switch_cooldown: f32,
    /// Seconds of continuous play in the current context. Used to
    /// force a track re-roll after a while so tracks with the same
    /// context get variety even on long walks.
    context_elapsed: f32,
    /// Music enabled by user.
    enabled: bool,
    /// User has clicked to activate audio (browser autoplay policy).
    user_activated: bool,
    /// Master volume (0.0 - 1.0), scales all track volumes.
    pub master_volume: f32,
    /// Show volume indicator timer (fades out).
    volume_display_timer: f32,
}

impl Default for MusicState {
    fn default() -> Self {
        Self {
            current_track: None,
            current_context: MusicContext::Silent,
            channel_a: None,
            channel_b: None,
            active_is_a: true,
            fade_progress: 1.0,
            fading: false,
            target_volume: 0.7,
            switch_cooldown: 0.0,
            context_elapsed: 0.0,
            enabled: true,
            user_activated: false,
            master_volume: 0.35,
            volume_display_timer: 0.0,
        }
    }
}

// ── Audio Helpers ────────────────────────────────────

fn create_audio(src: &str, volume: f64, looping: bool) -> Option<web_sys::HtmlAudioElement> {
    let audio = web_sys::HtmlAudioElement::new_with_src(src).ok()?;
    audio.set_volume(0.0); // start silent, fade in
    audio.set_loop(looping);
    // play() returns a Promise — use wasm_bindgen_futures to handle it
    let promise = audio.play().ok()?;
    let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(promise));
    Some(audio)
}

fn set_volume(audio: &Option<web_sys::HtmlAudioElement>, vol: f64) {
    if let Some(a) = audio {
        a.set_volume(vol.clamp(0.0, 1.0));
    }
}

fn stop_audio(audio: &mut Option<web_sys::HtmlAudioElement>) {
    if let Some(a) = audio.take() {
        a.pause().ok();
        a.set_src("");
    }
}

// ── Base URL for music files ────────────────────────

fn music_base_url() -> String {
    // Derive from current page URL
    if let Some(window) = web_sys::window() {
        if let Ok(href) = window.location().href() {
            // href is like http://localhost:9090/crates/gameclient/index.html
            if let Some(idx) = href.rfind('/') {
                return format!("{}/assets/music/", &href[..idx]);
            }
        }
    }
    "assets/music/".to_string()
}

// ── Update System ───────────────────────────────────

const FADE_DURATION: f32 = 1.5; // seconds
const SWITCH_COOLDOWN: f32 = 3.0; // minimum seconds between track changes

fn update_music(
    time: Res<Time>,
    state: Res<State<AppState>>,
    player: Option<Res<MyPlayerState>>,
    world: Option<Res<WorldGrid>>,
    combat: Option<Res<crate::combat::CombatUiState>>,
    catalog: Res<MusicCatalog>,
    mut music: ResMut<MusicState>,
) {
    if !music.enabled || !music.user_activated || catalog.tracks.is_empty() { return; }

    let dt = time.delta_secs();
    music.switch_cooldown = (music.switch_cooldown - dt).max(0.0);

    // Determine desired context
    let desired_context = determine_context(&state, &player, &world, &combat);
    let speed = player.as_ref().map(|p| p.speed_kmh).unwrap_or(0.0);

    // Track how long we've been in this context; force a re-roll every
    // 3 min so long walks in one biome actually cycle through their
    // available tracks.
    const CONTEXT_REROLL_SECONDS: f32 = 90.0;
    if desired_context == music.current_context {
        music.context_elapsed += dt;
    } else {
        music.context_elapsed = 0.0;
    }
    // Don't reset context_elapsed here — if the switch below is blocked
    // by switch_cooldown, we'd waste the reroll and wait another 180 s.
    // Reset it only inside the branch that actually changes tracks.
    let force_reroll = music.context_elapsed >= CONTEXT_REROLL_SECONDS;

    // Find best track for this context + speed. Always pass current
    // track id — it's used for no-repeat filtering. `stability` toggles
    // whether we also KEEP current if it's still valid for the context.
    let desired_track = pick_track(
        &catalog, &desired_context, speed,
        music.current_track.as_deref(),
        /* stability = */ !force_reroll,
    );

    // Check if we need to switch
    let should_switch = desired_track.as_ref().map(|t| &t.id) != music.current_track.as_ref();

    if should_switch && music.switch_cooldown <= 0.0 {
        if let Some(track) = &desired_track {
            let base = music_base_url();
            let src = format!("{}{}", base, track.file);

            // Start new track on inactive channel
            let new_channel = if music.active_is_a {
                stop_audio(&mut music.channel_b);
                music.channel_b = create_audio(&src, 0.0, track.r#loop);
                &music.channel_b
            } else {
                stop_audio(&mut music.channel_a);
                music.channel_a = create_audio(&src, 0.0, track.r#loop);
                &music.channel_a
            };

            if new_channel.is_some() {
                music.fading = true;
                music.fade_progress = 0.0;
                music.target_volume = track.volume;
                music.current_track = Some(track.id.clone());
                music.current_context = desired_context;
                music.switch_cooldown = SWITCH_COOLDOWN;
                // Reset the variety timer only on an actual switch —
                // otherwise a blocked reroll (cooldown or same-track
                // re-pick) throws away the 3-minute window.
                music.context_elapsed = 0.0;
            }
        } else {
            // Fade to silence
            music.fading = true;
            music.fade_progress = 0.0;
            music.target_volume = 0.0;
            music.current_track = None;
            music.current_context = MusicContext::Silent;
        }
    }

    // Process crossfade
    if music.fading {
        music.fade_progress = (music.fade_progress + dt / FADE_DURATION).min(1.0);

        let (active, inactive) = if music.active_is_a {
            (&music.channel_a, &music.channel_b)
        } else {
            (&music.channel_b, &music.channel_a)
        };

        let master = music.master_volume as f64;

        // Fade out old track
        let old_vol = (1.0 - music.fade_progress) as f64 * music.target_volume as f64 * master;
        set_volume(active, old_vol);

        // Fade in new track
        let new_vol = music.fade_progress as f64 * music.target_volume as f64 * master;
        set_volume(inactive, new_vol);

        if music.fade_progress >= 1.0 {
            // Crossfade complete — swap channels
            music.fading = false;
            music.active_is_a = !music.active_is_a;

            // Stop the old channel
            if music.active_is_a {
                stop_audio(&mut music.channel_b);
            } else {
                stop_audio(&mut music.channel_a);
            }
        }
    }

    // Apply master volume to active channel even when not fading
    if !music.fading {
        let active = if music.active_is_a { &music.channel_a } else { &music.channel_b };
        let vol = music.target_volume as f64 * music.master_volume as f64;
        set_volume(active, vol);
    }
}

fn volume_controls(
    keys: Res<ButtonInput<KeyCode>>,
    mut music: ResMut<MusicState>,
) {
    let mut changed = false;
    if keys.just_pressed(KeyCode::Equal) || keys.just_pressed(KeyCode::NumpadAdd) {
        music.master_volume = (music.master_volume + 0.1).min(1.0);
        changed = true;
    }
    if keys.just_pressed(KeyCode::Minus) || keys.just_pressed(KeyCode::NumpadSubtract) {
        music.master_volume = (music.master_volume - 0.1).max(0.0);
        changed = true;
    }
    if keys.just_pressed(KeyCode::KeyM) {
        if !music.user_activated {
            music.user_activated = true;
            music.enabled = true;
        } else {
            music.enabled = !music.enabled;
        }
        if !music.enabled {
            set_volume(&music.channel_a, 0.0);
            set_volume(&music.channel_b, 0.0);
        }
        changed = true;
    }
    if changed {
        music.volume_display_timer = 2.0;
    }
}

#[derive(Component)]
struct VolumeIndicator;

fn update_volume_display(
    mut commands: Commands,
    font: Res<crate::GameFont>,
    time: Res<Time>,
    mut music: ResMut<MusicState>,
    existing: Query<Entity, With<VolumeIndicator>>,
) {
    music.volume_display_timer -= time.delta_secs();

    if music.volume_display_timer <= 0.0 {
        for entity in &existing {
            commands.entity(entity).despawn_recursive();
        }
        return;
    }

    // Rebuild indicator
    for entity in &existing {
        commands.entity(entity).despawn_recursive();
    }

    let vol_pct = (music.master_volume * 100.0) as u32;
    let label = if !music.enabled {
        "Music: OFF".to_string()
    } else {
        format!("Vol: {}%", vol_pct)
    };

    commands.spawn((
        Node {
            position_type: PositionType::Absolute,
            top: Val::Px(36.0),
            right: Val::Px(12.0),
            padding: UiRect::axes(Val::Px(10.0), Val::Px(5.0)),
            ..default()
        },
        BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.6)),
        BorderRadius::all(Val::Px(4.0)),
        VolumeIndicator,
    )).with_children(|parent| {
        parent.spawn((
            Text::new(label),
            TextFont { font: font.0.clone(), font_size: 9.0, ..default() },
            TextColor(Color::srgb(0.8, 0.8, 0.8)),
        ));
    });
}

fn determine_context(
    app_state: &State<AppState>,
    player: &Option<Res<MyPlayerState>>,
    world: &Option<Res<WorldGrid>>,
    combat: &Option<Res<crate::combat::CombatUiState>>,
) -> MusicContext {
    if **app_state == AppState::Title {
        return MusicContext::Title;
    }

    // Combat overrides everything
    if let Some(combat) = combat {
        if combat.active && combat.state.is_some() {
            let difficulty = combat.state.as_ref().map(|s| s.difficulty).unwrap_or(0);
            return if difficulty >= 6 { MusicContext::Boss } else { MusicContext::Combat };
        }
    }

    let Some(player) = player else { return MusicContext::Silent };

    // Inside an interior? Distinct context — don't inherit the overworld
    // biome's track just because the cave mouth happens to sit in a forest.
    if player.location.is_some() {
        return MusicContext::Interior;
    }

    let Some(world) = world else { return MusicContext::Silent };

    // Check if at a village/town POI
    if let Some(poi) = world.map.poi_at(player.tile_x as usize, player.tile_y as usize) {
        if matches!(poi.poi_type, questlib::mapgen::PoiType::Town | questlib::mapgen::PoiType::Village) {
            return MusicContext::Village;
        }
    }

    // Biome-based
    let biome = world.map.biome_at(player.tile_x as usize, player.tile_y as usize);
    match biome {
        questlib::mapgen::Biome::Grassland | questlib::mapgen::Biome::Desert => MusicContext::Grassland,
        questlib::mapgen::Biome::Forest | questlib::mapgen::Biome::DenseForest => MusicContext::Forest,
        questlib::mapgen::Biome::Mountain | questlib::mapgen::Biome::Snow => MusicContext::Mountain,
        questlib::mapgen::Biome::Swamp => MusicContext::Swamp,
        _ => MusicContext::Grassland,
    }
}

fn pick_track<'a>(
    catalog: &'a MusicCatalog,
    context: &MusicContext,
    speed: f32,
    current_track: Option<&str>,
    stability: bool,
) -> Option<&'a TrackDef> {
    let context_str = context.as_str();
    if context_str.is_empty() { return None; }

    let mut candidates: Vec<&TrackDef> = catalog.tracks.iter()
        .filter(|t| t.context == context_str)
        .collect();
    // Fallback: contexts without their own tracks (currently Mountain)
    // borrow from grassland so music doesn't go silent when the player
    // walks into those biomes. Drop these lines as soon as mountain-
    // specific tracks get added to tracks.json.
    if candidates.is_empty() && context_str != "grassland" {
        candidates = catalog.tracks.iter()
            .filter(|t| t.context == "grassland")
            .collect();
    }
    if candidates.is_empty() { return None; }

    // Stability: if we're already playing a track valid for this context,
    // keep it. Otherwise we'd snap from track-N to track-M every time the
    // player's speed nudged past a recommended_kmh midpoint — forcing
    // constant music changes on a normal walk. Caller sets stability=false
    // on a forced reroll.
    if stability {
        if let Some(cur) = current_track {
            if let Some(&t) = candidates.iter().find(|t| t.id == cur) {
                return Some(t);
            }
        }
    }

    // First-time pick (or a context re-roll): we treat speed as a
    // SOFT preference. Start with candidates within a wide band
    // (±2.5 km/h), and if the band would leave us with fewer than 3
    // options we widen to everything in the context. That way a biome
    // with 5 tracks always has at least 3 real choices in the pool, so
    // rerolls produce genuine variety even at uncommon walking speeds.
    let in_band: Vec<&TrackDef> = candidates.iter()
        .copied()
        .filter(|t| (t.recommended_kmh - speed).abs() <= 2.5)
        .collect();
    let pool: Vec<&TrackDef> = if in_band.len() >= 3 { in_band } else { candidates.clone() };

    // Avoid repeating the just-played track if the pool has any other
    // option. If the track catalog has only one entry, we'll still pick
    // that same one — better than silence.
    let filtered: Vec<&TrackDef> = if let Some(cur) = current_track {
        let without_current: Vec<&TrackDef> = pool.iter()
            .copied()
            .filter(|t| t.id != cur)
            .collect();
        if without_current.is_empty() { pool } else { without_current }
    } else {
        pool
    };

    let idx = (js_sys::Math::random() * filtered.len() as f64) as usize;
    filtered.get(idx).copied()
}

// ── Music Button (top-right HUD) ────────────────────

#[derive(Component)]
struct MusicButton;

#[derive(Component)]
struct MusicButtonText;

fn update_music_button(
    mut commands: Commands,
    font: Res<crate::GameFont>,
    mouse: Res<ButtonInput<MouseButton>>,
    mut music: ResMut<MusicState>,
    btn_q: Query<&Interaction, With<MusicButton>>,
    text_q: Query<Entity, With<MusicButtonText>>,
    existing: Query<Entity, With<MusicButton>>,
) {
    // Handle click
    if mouse.just_pressed(MouseButton::Left) {
        for interaction in &btn_q {
            if matches!(interaction, Interaction::Hovered | Interaction::Pressed) {
                if !music.user_activated {
                    music.user_activated = true;
                    music.enabled = true;
                } else {
                    music.enabled = !music.enabled;
                    if !music.enabled {
                        set_volume(&music.channel_a, 0.0);
                        set_volume(&music.channel_b, 0.0);
                    }
                }
                music.volume_display_timer = 2.0;
            }
        }
    }

    // Spawn button if it doesn't exist
    if existing.is_empty() {
        commands.spawn((
            Button,
            Node {
                position_type: PositionType::Absolute,
                top: Val::Px(4.0),
                right: Val::Px(130.0),
                padding: UiRect::axes(Val::Px(8.0), Val::Px(4.0)),
                ..default()
            },
            BackgroundColor(Color::srgba(0.0, 0.0, 0.0, 0.4)),
            BorderRadius::all(Val::Px(3.0)),
            MusicButton,
        )).with_children(|btn| {
            let label = if !music.user_activated {
                "[M]usic"
            } else if music.enabled {
                "[M] On"
            } else {
                "[M] Off"
            };
            btn.spawn((
                Text::new(label),
                TextFont { font: font.0.clone(), font_size: 8.0, ..default() },
                TextColor(Color::srgb(0.7, 0.7, 0.7)),
                MusicButtonText,
            ));
        });
        return;
    }

    // Update button text
    for entity in &text_q {
        let label = if !music.user_activated {
            "[M]usic"
        } else if music.enabled {
            "[M] On"
        } else {
            "[M] Off"
        };
        commands.entity(entity).insert(Text::new(label));
    }
}
