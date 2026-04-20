//! Sound effects — synthesized 8-bit blips via the Web Audio API.
//!
//! No audio assets shipped. Each sound is a square-wave oscillator run
//! through a gain envelope, scheduled as a short melodic phrase. Cheap,
//! deterministic, no sample files to manage. If you later want proper
//! samples, replace `play_sfx` below with `HtmlAudioElement::new_with_src`.
//!
//! Fired from Bevy event `SfxEvent`. Four kinds:
//!   - GoldGained     (chest open, monster loot, quest reward)
//!   - RouteArrived   (player reached the end of their planned route)
//!   - LevelUp        (+1 character level)
//!   - CombatVictory  (combat state went from active → none)

use bevy::prelude::*;
use wasm_bindgen::JsCast;

use crate::states::AppState;
use crate::terrain::tilemap::MyPlayerState;

pub struct SfxPlugin;

impl Plugin for SfxPlugin {
    fn build(&self, app: &mut App) {
        app
            .add_event::<SfxEvent>()
            .init_resource::<LastRouteLen>()
            .init_resource::<LastCombatActive>()
            .init_resource::<LastGold>()
            .init_resource::<LastLevel>()
            .add_systems(
                Update,
                (
                    detect_gold_gain,
                    detect_route_arrival,
                    detect_level_up,
                    detect_combat_victory,
                    play_events,
                )
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

// ── Events ─────────────────────────────────────────

#[derive(Event, Debug, Clone, Copy, PartialEq, Eq)]
pub struct SfxEvent(pub SfxKind);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SfxKind {
    GoldGained,
    RouteArrived,
    LevelUp,
    CombatVictory,
}

// ── Tracker resources (client-side deltas) ─────────

#[derive(Resource, Default)]
struct LastGold(i32);

#[derive(Resource, Default)]
struct LastLevel(u32);

#[derive(Resource, Default)]
struct LastRouteLen(usize);

#[derive(Resource, Default)]
struct LastCombatActive(bool);

// ── Detection systems ──────────────────────────────

fn detect_gold_gain(
    state: Res<MyPlayerState>,
    mut last: ResMut<LastGold>,
    mut writer: EventWriter<SfxEvent>,
) {
    // First-tick initialization (last.0 starts at 0 but the server hands us a
    // real gold number; we don't want to fire a sound on initial sync).
    if last.0 == 0 && state.gold > 0 {
        last.0 = state.gold;
        return;
    }
    if state.gold > last.0 {
        writer.send(SfxEvent(SfxKind::GoldGained));
    }
    last.0 = state.gold;
}

fn detect_route_arrival(
    state: Res<MyPlayerState>,
    mut last_len: ResMut<LastRouteLen>,
    mut writer: EventWriter<SfxEvent>,
) {
    let len = state.route.len();
    // Non-empty → empty means the server completed our route. (Empty → empty
    // is idle; empty → non-empty is setting a new route.)
    if last_len.0 > 0 && len == 0 {
        writer.send(SfxEvent(SfxKind::RouteArrived));
    }
    last_len.0 = len;
}

fn detect_level_up(
    state: Res<MyPlayerState>,
    mut last: ResMut<LastLevel>,
    mut writer: EventWriter<SfxEvent>,
) {
    let lvl = questlib::leveling::level_from_meters(state.total_distance_m as u64);
    if last.0 > 0 && lvl > last.0 {
        writer.send(SfxEvent(SfxKind::LevelUp));
    }
    last.0 = lvl;
}

fn detect_combat_victory(
    combat: Res<crate::combat::CombatUiState>,
    mut last: ResMut<LastCombatActive>,
    mut writer: EventWriter<SfxEvent>,
) {
    let active_now = combat.active;
    // Active → inactive: either victory or flee. Flee is rare and a fanfare
    // isn't wildly wrong for either; tighten later if we add a retreat sound.
    if last.0 && !active_now {
        writer.send(SfxEvent(SfxKind::CombatVictory));
    }
    last.0 = active_now;
}

// ── Event handler: synthesize + play ───────────────

fn play_events(
    mut reader: EventReader<SfxEvent>,
    music: Res<crate::music::MusicState>,
) {
    for SfxEvent(kind) in reader.read() {
        play_sfx(*kind, sfx_volume(&music));
    }
}

/// SFX volume scales with the music master slider and respects the mute
/// toggle, so the game's existing audio controls work for SFX too.
fn sfx_volume(music: &crate::music::MusicState) -> f32 {
    // Field visibility: only master_volume is pub. That's enough — the
    // mute toggle zeroes master_volume so we inherit that behavior too.
    music.master_volume.clamp(0.0, 1.0)
}

/// Build a short WebAudio graph and play it. Returns silently on any
/// browser-side failure so SFX never break the game.
fn play_sfx(kind: SfxKind, volume: f32) {
    if volume <= 0.0 { return; }
    let Some(window) = web_sys::window() else { return };

    // Each call makes its OWN AudioContext so overlapping sounds don't
    // stomp each other. ACs are cheap; the browser GCs them after `stop`.
    let Ok(ctx) = web_sys::AudioContext::new() else { return };
    let t0 = ctx.current_time();

    let notes = notes_for(kind);
    for (i, note) in notes.iter().enumerate() {
        let start = t0 + note.offset_s as f64;
        let end = start + note.duration_s as f64;
        let _ = schedule_note(&ctx, note.freq_hz, start, end, note.gain * volume);
    }
    // Close the context shortly after the last note finishes so resources free.
    let tail = notes.iter().map(|n| n.offset_s + n.duration_s).fold(0.0_f32, f32::max) + 0.05;
    let closure = wasm_bindgen::closure::Closure::once_into_js(move || {
        let _ = ctx.close();
    });
    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
        closure.as_ref().unchecked_ref(),
        (tail * 1000.0) as i32,
    );
}

/// One note in a phrase.
struct Note {
    offset_s: f32,
    duration_s: f32,
    freq_hz: f32,
    /// 0.0..=1.0; the output is multiplied by the master volume on top.
    gain: f32,
}

/// Deterministic per-kind note sequences. Square-wave 8-bit style.
/// Frequencies roughly C5=523 Hz, E5=659, G5=784, G4=392.
fn notes_for(kind: SfxKind) -> &'static [Note] {
    match kind {
        // GoldGained — quick rising chirp (C5 → E5)
        SfxKind::GoldGained => &[
            Note { offset_s: 0.00, duration_s: 0.08, freq_hz: 523.0, gain: 0.25 },
            Note { offset_s: 0.06, duration_s: 0.10, freq_hz: 659.0, gain: 0.22 },
        ],
        // RouteArrived — soft descending 2-note (E5 → C5), quieter
        SfxKind::RouteArrived => &[
            Note { offset_s: 0.00, duration_s: 0.08, freq_hz: 659.0, gain: 0.15 },
            Note { offset_s: 0.08, duration_s: 0.12, freq_hz: 523.0, gain: 0.13 },
        ],
        // LevelUp — ascending triad (C5 → E5 → G5)
        SfxKind::LevelUp => &[
            Note { offset_s: 0.00, duration_s: 0.08, freq_hz: 523.0, gain: 0.25 },
            Note { offset_s: 0.08, duration_s: 0.08, freq_hz: 659.0, gain: 0.25 },
            Note { offset_s: 0.16, duration_s: 0.16, freq_hz: 784.0, gain: 0.28 },
        ],
        // CombatVictory — 4-note fanfare G4 → C5 → E5 → G5
        SfxKind::CombatVictory => &[
            Note { offset_s: 0.00, duration_s: 0.08, freq_hz: 392.0, gain: 0.22 },
            Note { offset_s: 0.08, duration_s: 0.08, freq_hz: 523.0, gain: 0.25 },
            Note { offset_s: 0.16, duration_s: 0.08, freq_hz: 659.0, gain: 0.25 },
            Note { offset_s: 0.24, duration_s: 0.20, freq_hz: 784.0, gain: 0.28 },
        ],
    }
}

/// Schedule a single square-wave note with a short attack/release envelope.
fn schedule_note(
    ctx: &web_sys::AudioContext,
    freq_hz: f32,
    start_s: f64,
    end_s: f64,
    peak_gain: f32,
) -> Result<(), wasm_bindgen::JsValue> {
    let osc = ctx.create_oscillator()?;
    osc.set_type(web_sys::OscillatorType::Square);
    osc.frequency().set_value(freq_hz);

    let gain = ctx.create_gain()?;
    // Envelope: instant attack to peak_gain, short release to ~0 by end_s.
    // set_value_at_time / linearRampToValueAtTime are the standard mechanism.
    gain.gain().set_value_at_time(0.0, start_s)?;
    gain.gain().linear_ramp_to_value_at_time(peak_gain, start_s + 0.005)?;
    gain.gain().linear_ramp_to_value_at_time(0.0, end_s)?;

    osc.connect_with_audio_node(&gain)?;
    gain.connect_with_audio_node(&ctx.destination())?;

    osc.start_with_when(start_s)?;
    osc.stop_with_when(end_s)?;
    Ok(())
}
