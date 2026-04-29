//! Day/night cycle (Phase 1): a global time-of-day resource + a
//! full-world darkness overlay for night.
//!
//! Sun/moon position is computed per-frame and is picked up by the
//! water shader via its `sun_pos` uniform (see water_shader.rs's
//! update_material). The F8 debug sun still overrides.
//!
//! Multi-client sync: on boot and every `SYNC_INTERVAL_S`, fetch
//! `/daynight` from the server and snap the local `time_s` /
//! `cycle_seconds` to the server's value. Between polls we keep
//! advancing locally from `Time::delta_secs()` so the sun moves
//! smoothly. Server computes `time_s = unix_now % cycle_seconds` —
//! stateless, so every connected client sees the same time-of-day.

use bevy::prelude::*;
use std::f32::consts::TAU;
use std::sync::{Arc, Mutex};

use crate::states::AppState;

/// Interval between `/daynight` polls in seconds. 60 s is ample —
/// the cycle is 120 s total so worst-case drift between polls is a
/// second or two, which is invisible.
const SYNC_INTERVAL_S: f32 = 60.0;

pub struct DayNightPlugin;

impl Plugin for DayNightPlugin {
    fn build(&self, app: &mut App) {
        app.insert_resource(DayNightCycle::default())
            .insert_resource(DayNightSync::default())
            .add_systems(
                Update,
                (apply_server_sync, advance_cycle, poll_server_time)
                    .chain()
                    .run_if(in_state(AppState::InGame)),
            );
    }
}

/// Shared slot the async `/daynight` fetcher writes into; the main
/// ECS system drains it on the next frame.
#[derive(Resource)]
pub struct DayNightSync {
    pub pending: Arc<Mutex<Option<(f32, f32)>>>,
    pub poll_timer: f32,
    pub boot_fetched: bool,
}

impl Default for DayNightSync {
    fn default() -> Self {
        Self {
            pending: Arc::new(Mutex::new(None)),
            // Force an immediate poll on first frame in-game.
            poll_timer: SYNC_INTERVAL_S,
            boot_fetched: false,
        }
    }
}

/// Cycle timer + derivation helpers.
#[derive(Resource, Debug, Clone, Copy)]
pub struct DayNightCycle {
    /// Total cycle length in seconds. Default 10 minutes.
    pub cycle_seconds: f32,
    /// Current time into cycle. Wraps modulo `cycle_seconds`.
    pub time_s: f32,
}

impl Default for DayNightCycle {
    fn default() -> Self {
        // 5-minute cycle (2.5 min day + 2.5 min night). Server-
        // synced via /daynight, so bumping this in isolation does
        // nothing — keep it in sync with devserver's CYCLE_SECONDS.
        let cycle_seconds: f32 = 300.0;
        // Seed `time_s` from the browser's wall clock so the very
        // first frame is already at the right cycle position. Without
        // this we'd default to 0 (dawn) and players would see a brief
        // dawn flash on every reload before the async /daynight poll
        // (~100 ms later) snapped to the server's wall-clock value.
        // Server sync still corrects any drift from this estimate
        // every 60 s.
        let now_ms = js_sys::Date::now();
        let now_secs = now_ms / 1000.0;
        let time_s = now_secs.rem_euclid(cycle_seconds as f64) as f32;
        Self {
            cycle_seconds,
            time_s,
        }
    }
}

impl DayNightCycle {
    /// Normalized 0..1 cycle position.
    ///   0.00 — dawn
    ///   0.25 — noon (brightest)
    ///   0.50 — dusk
    ///   0.75 — midnight (darkest)
    pub fn t(&self) -> f32 {
        (self.time_s / self.cycle_seconds).fract()
    }

    /// True if we're in the day half (sun up) vs night half (moon up).
    pub fn is_day(&self) -> bool {
        self.t() < 0.5
    }

    /// Current light-source world position.
    ///
    /// One continuous 2π arc across the full cycle — no separate moon
    /// arc, because splitting day and night into two independent arcs
    /// (the previous design) teleported the light source from east to
    /// west at dusk. Every directional shader (shadows, hillshade,
    /// specular) flipped sign at that boundary, producing a visible
    /// "lighting jumps" at the end of each day.
    ///
    /// The arc: at t=0 the sun rises on the east horizon, reaches
    /// zenith at noon (t=0.25), sets on the west horizon at dusk
    /// (t=0.5), passes through the anti-zenith below the ground at
    /// midnight (t=0.75, sun.z negative), and returns to the east
    /// horizon at t=1. `night_alpha` independently drives the darkness
    /// overlay during the below-horizon half of the cycle, so callers
    /// don't have to know that the sun is "the moon" at night.
    pub fn light_pos(&self, world_center: Vec2) -> Vec3 {
        let t = self.t();
        let angle = t * TAU;
        let dist = 10_000.0;
        let height_max = 8_000.0;
        Vec3::new(
            // cos: east (+x) at t=0, center at t=0.25, west (-x) at t=0.5
            world_center.x + angle.cos() * dist,
            // Fixed "sky offset" so the celestial body feels like
            // it's coming from above rather than beside the world.
            world_center.y - dist * 0.20,
            // sin: 100 at horizons (t=0, 0.5), +8100 at noon (t=0.25),
            // goes negative (below ground) between t=0.5 and t=1.
            100.0 + angle.sin() * height_max,
        )
    }

    /// Darkness overlay alpha: 0 during day, sinusoidal rise to the
    /// NIGHT_PEAK at midnight. Smooth either side of dusk/dawn.
    ///
    /// `NIGHT_PEAK` is intentionally high (0.85) so night is actually
    /// dark rather than dusk-tinted. Phase 3 will punch this back out
    /// via radial point lights at player/town positions.
    pub fn night_alpha(&self) -> f32 {
        const NIGHT_PEAK: f32 = 0.98;
        let s = (self.t() * TAU).sin();
        if s >= 0.0 { 0.0 } else { -s * NIGHT_PEAK }
    }

    /// Normalized sun altitude: +1 at noon zenith, 0 on the horizon,
    /// −1 at the midnight anti-zenith (below the ground). Same source
    /// used internally by `light_pos` for the z coordinate, exposed
    /// here for time-of-day color grading.
    pub fn sun_elevation(&self) -> f32 {
        (self.t() * TAU).sin()
    }

    /// Warm/cool tint curve that follows the sun's arc. Warm white at
    /// noon, amber during the golden hour (low sun), cool moonlight
    /// blue at midnight. Drives the terrain hillshade highlight color
    /// and the cloud tint so they all shift together — without this,
    /// individual shaders would pick slightly different hues and the
    /// sunset wouldn't read as a unified event.
    pub fn sky_tint(&self) -> Vec3 {
        let elevation = self.sun_elevation();
        // Orange ramp: 0 above 30 % of arc height, 1 at horizon, saturates
        // below horizon so the brief dusk/dawn window hits full amber.
        let orange_t = ((0.3 - elevation) / 0.3).clamp(0.0, 1.0);
        let night_t = self.night_alpha().clamp(0.0, 1.0);
        let day = Vec3::new(1.00, 0.95, 0.80);
        // Warmer golden-hour: R stays at 1.0, drop G and B a bit
        // further so the horizon band reads as deeper amber / rust
        // rather than pale-orange. Cranks the "real sunset" feel.
        let orange = Vec3::new(1.00, 0.42, 0.08);
        let night = Vec3::new(0.55, 0.70, 1.00);
        day.lerp(orange, orange_t).lerp(night, night_t)
    }
}

fn advance_cycle(time: Res<Time>, mut cycle: ResMut<DayNightCycle>) {
    cycle.time_s = (cycle.time_s + time.delta_secs()).rem_euclid(cycle.cycle_seconds);
}

/// Drain any pending server sync into the local cycle resource. Runs
/// before `advance_cycle` so a fresh sync takes effect on the same
/// frame without being immediately overwritten by the local tick.
fn apply_server_sync(
    mut cycle: ResMut<DayNightCycle>,
    sync: Res<DayNightSync>,
) {
    if let Ok(mut slot) = sync.pending.lock() {
        if let Some((time_s, cycle_seconds)) = slot.take() {
            if cycle_seconds > 0.0 {
                cycle.cycle_seconds = cycle_seconds;
            }
            cycle.time_s = time_s.rem_euclid(cycle.cycle_seconds);
        }
    }
}

/// Fire a `/daynight` fetch on boot and every SYNC_INTERVAL_S
/// thereafter. The fetch writes into `sync.pending`; `apply_server_sync`
/// picks it up on the following frame.
fn poll_server_time(
    time: Res<Time>,
    mut sync: ResMut<DayNightSync>,
) {
    sync.poll_timer += time.delta_secs();
    if sync.poll_timer < SYNC_INTERVAL_S && sync.boot_fetched {
        return;
    }
    sync.poll_timer = 0.0;
    sync.boot_fetched = true;
    kick_off_fetch(sync.pending.clone());
}

fn kick_off_fetch(slot: Arc<Mutex<Option<(f32, f32)>>>) {
    wasm_bindgen_futures::spawn_local(async move {
        let Ok(resp) = reqwest::Client::new().get("/daynight").send().await else { return };
        let Ok(text) = resp.text().await else { return };
        let time_s = parse_field(&text, "\"time_s\":");
        let cycle_s = parse_field(&text, "\"cycle_seconds\":");
        if let (Some(t), Some(c)) = (time_s, cycle_s) {
            if let Ok(mut g) = slot.lock() { *g = Some((t, c)); }
        }
    });
}

fn parse_field(text: &str, key: &str) -> Option<f32> {
    let i = text.find(key)?;
    let tail = &text[i + key.len()..];
    let num: String = tail.chars()
        .skip_while(|c| c.is_whitespace())
        .take_while(|c| c.is_ascii_digit() || *c == '.' || *c == '-')
        .collect();
    num.parse::<f32>().ok()
}

// The world darkness overlay itself lives in `terrain::night_lights`
// now — a Material2d shader that reads night_alpha from this cycle
// resource AND subtracts per-pixel contributions from active point
// lights (players, POIs) so torches/lanterns punch through the night.
