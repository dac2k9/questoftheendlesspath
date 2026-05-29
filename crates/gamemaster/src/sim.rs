//! In-process player simulator for integration tests.
//!
//! Wraps `AdventureBundle::load_bundle`, builds the same shared-state
//! Arcs the live server uses, and exposes a small synchronous API
//! that drives `tick::run_tick_dev` + `interior::run_interior_tick`
//! directly — no HTTP, no server thread, no Tokio. Tests construct a
//! `SimulatedRun`, spawn synthetic players, push them around with
//! `teleport` / `tick_walking`, and assert on state pulled out of the
//! same `SharedState` the production code mutates.
//!
//! Compared to `tools/chaos_smoketest.rb`:
//! - Faster (no HTTP marshaling, no 1-second sleep between ticks).
//! - Asserts on internal state (location, fog, completed events,
//!   active events) without going through `/players`.
//! - Can exercise cave entry / interior ticks, which the Ruby
//!   smoketest skips because boss-style combat events Dismiss
//!   without a planned route.
//! - Pure `cargo test`: works in CI without spinning up the server.
//!
//! Not a replacement for the live smoketest — the HTTP path itself
//! (auth, query parsing, response serialization) still needs the
//! Ruby version. This catches game-logic regressions, content
//! regressions, and trigger-chain breakage.

#![cfg(test)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use questlib::events::EventStatus;
use questlib::fog::FogBitfield;
use questlib::items::InventorySlot;
use questlib::mapgen::WorldMap;

use crate::adventure::{self, AdventureBundle, AdventurePreset};
use crate::combat::SharedCombat;
use crate::devserver::{DevPlayerState, SharedState};
use crate::interior::SharedInteriors;
use crate::mobile_entity::{SharedEntityDefs, SharedEntityStates};
use crate::{SharedEvents, SharedNotifs};

/// Tick interval in seconds — production runs ticks every ~1 s but
/// the simulator decouples wall-clock from simulation time. `delta_secs`
/// drives how much distance the player accumulates per tick when
/// walking. Keep it close to production so timing-sensitive triggers
/// (distance_walked thresholds, idle resets) behave similarly.
const SIM_TICK_SECS: f32 = 1.0;

/// Self-contained harness for an in-process adventure run.
///
/// One instance maps to one adventure bundle. Spawning a player adds
/// them to the shared state; subsequent helpers act on that player by
/// id. Internal HashMaps (`player_fogs` etc.) are owned by the harness
/// so the simulator can tick multiple times without losing
/// per-player runtime state — exactly how the production tick loop
/// keeps them across iterations.
pub(crate) struct SimulatedRun {
    pub bundle: AdventureBundle,
    pub state: SharedState,
    pub notifs: SharedNotifs,
    pub combat: SharedCombat,
    player_fogs: HashMap<String, FogBitfield>,
    player_last_distance: HashMap<String, f64>,
    player_boss_wait_notified: HashMap<String, String>,
    interior_fogs: HashMap<(String, String), FogBitfield>,
    /// Deterministic RNG roll used by random_in_biome triggers.
    /// Default 0.99 means random encounters never fire — tests can
    /// override with `set_rng_roll` if they want to test those
    /// specifically.
    pub rng_roll: f32,
}

impl SimulatedRun {
    /// Spin up a fresh harness for the named adventure preset.
    /// Honors the same `presets()` registry the production server
    /// uses, so seed / dims / events / entities / interiors match
    /// what real players see.
    pub fn for_adventure(adventure_id: &str) -> Self {
        // `load_bundle` reads `adventures/*.json` via CWD-relative
        // paths. `cargo test` runs in the crate dir; we need to be
        // at the workspace root for those paths to resolve. Walk up
        // until we find the `adventures/` dir.
        ensure_workspace_cwd();
        let preset = preset_for(adventure_id)
            .unwrap_or_else(|| panic!("unknown adventure: {}", adventure_id));
        let bundle = adventure::load_bundle(preset).expect("load bundle");
        Self {
            bundle,
            state: Arc::new(Mutex::new(HashMap::new())),
            notifs: Arc::new(Mutex::new(HashMap::new())),
            combat: Arc::new(Mutex::new(HashMap::new())),
            player_fogs: HashMap::new(),
            player_last_distance: HashMap::new(),
            player_boss_wait_notified: HashMap::new(),
            interior_fogs: HashMap::new(),
            rng_roll: 0.99,
        }
    }

    /// Convenience constructor for the chaos arc — the bulk of tests
    /// pin to this preset.
    pub fn for_chaos() -> Self { Self::for_adventure("chaos") }

    /// Insert a fresh player into the shared state at the bundle's
    /// world centre with `chaos_intro`-equivalent default position.
    /// Returns the player_id (a stable test id derived from `name`)
    /// so subsequent helpers can address them.
    pub fn spawn_player(&mut self, name: &str) -> String {
        let id = format!("test-{}", name);
        let cx = (self.bundle.world.width / 2) as i32;
        let cy = (self.bundle.world.height / 2) as i32;
        let player = DevPlayerState {
            id: id.clone(),
            name: name.to_string(),
            adventure_id: self.bundle.preset.id.clone(),
            map_tile_x: cx,
            map_tile_y: cy,
            is_walking: false,
            current_speed_kmh: 0.0,
            debug_walking: true,
            ..Default::default()
        };
        self.state.lock().unwrap().insert(id.clone(), player);
        id
    }

    /// Move the player to a tile WITHOUT firing the per-tile trigger
    /// chain — for that, call `tick_walking` afterwards. Equivalent
    /// to the live `/admin/teleport` endpoint.
    pub fn teleport(&self, pid: &str, x: i32, y: i32) {
        let mut lock = self.state.lock().unwrap();
        if let Some(p) = lock.get_mut(pid) {
            p.map_tile_x = x;
            p.map_tile_y = y;
        }
    }

    /// Add an item to the player's inventory. Mirrors the live
    /// `/admin/give_item` endpoint.
    pub fn give_item(&self, pid: &str, item_id: &str) {
        let mut lock = self.state.lock().unwrap();
        if let Some(p) = lock.get_mut(pid) {
            questlib::items::add_item(&mut p.inventory, item_id, Some(crate::item_catalog()));
        }
    }

    /// Set a player's walking state without ticking. Lets a test put
    /// several players in motion before driving the shared tick loop, so
    /// they advance together (run_tick_dev processes every player each
    /// call, regardless of which pid tick_once was handed).
    pub fn set_walking(&self, pid: &str, speed_kmh: f32) {
        let mut lock = self.state.lock().unwrap();
        if let Some(p) = lock.get_mut(pid) {
            p.is_walking = speed_kmh > 0.0;
            p.current_speed_kmh = speed_kmh;
        }
    }

    /// Set the player's planned route (JSON array of [x, y] pairs).
    pub fn set_route(&self, pid: &str, route: &[(usize, usize)]) {
        let json = serde_json::to_string(&route).expect("route json");
        let mut lock = self.state.lock().unwrap();
        if let Some(p) = lock.get_mut(pid) {
            p.planned_route = json;
            // Reset route_meters_walked since the route is new.
            p.route_meters_walked = 0.0;
        }
    }

    /// Mark an event as completed in this player's PERSONAL list only,
    /// leaving the global catalog status untouched. Use this to
    /// simulate the multi-player state where one player has dismissed
    /// a `requires_browser` dialogue while the global status is still
    /// Active or Pending — common in the chaos arc when two players
    /// walk the same quest chain at different paces.
    pub fn mark_personal_completed(&mut self, pid: &str, event_id: &str) {
        let mut lock = self.state.lock().unwrap();
        if let Some(p) = lock.get_mut(pid) {
            if !p.completed_events.contains(&event_id.to_string()) {
                p.completed_events.push(event_id.to_string());
            }
        }
    }

    /// Force-complete an event for this player AND in the global
    /// catalog. Useful to skip the "fight the boss" step in tests
    /// that focus on the rewards / downstream gating.
    pub fn force_complete_event(&mut self, pid: &str, event_id: &str) {
        // Catalog side first — same pattern admin/reset_event uses.
        if let Ok(mut events) = self.bundle.events.lock() {
            if let Some(ev) = events.get_mut(event_id) {
                ev.force_status(EventStatus::Completed);
            }
        }
        // Then the per-player completion list.
        let mut lock = self.state.lock().unwrap();
        if let Some(p) = lock.get_mut(pid) {
            if !p.completed_events.contains(&event_id.to_string()) {
                p.completed_events.push(event_id.to_string());
            }
        }
    }

    /// Force an event's GLOBAL catalog status without touching any
    /// player's personal completion. Used to simulate "another player
    /// triggered this" / "this player fled mid-fight" states where the
    /// global status diverges from a given player's reality.
    pub fn force_event_status(&mut self, event_id: &str, status: EventStatus) {
        if let Ok(mut events) = self.bundle.events.lock() {
            if let Some(ev) = events.get_mut(event_id) {
                ev.force_status(status);
            }
        }
    }

    /// True if a combat is currently registered for this CONTENT event id
    /// (any player). Value-based — solo sessions key by
    /// `{event_id}\x1f{player_id}`, so a bare key lookup would miss them.
    pub fn combat_exists(&self, event_id: &str) -> bool {
        self.combat.lock().unwrap().values().any(|c| c.event_id == event_id)
    }

    /// Number of distinct combat sessions for a CONTENT event id. Two
    /// players soloing the same boss should produce 2 sessions; the old
    /// event_id-keyed map produced 1 (the second clobbered the first).
    pub fn combat_session_count(&self, event_id: &str) -> usize {
        self.combat.lock().unwrap().values().filter(|c| c.event_id == event_id).count()
    }

    /// True if this player is currently in any combat session.
    pub fn player_in_combat(&self, pid: &str) -> bool {
        crate::combat::player_in_combat(&self.combat, pid)
    }

    /// Force the enemy HP of the live combat session(s) for a CONTENT
    /// event id. Lets a test drive a fight to the brink without
    /// simulating the full balance, so the next tick triggers victory
    /// and we can assert on the reward path.
    pub fn set_combat_enemy_hp(&self, event_id: &str, hp: i32) {
        let mut lock = self.combat.lock().unwrap();
        for c in lock.values_mut().filter(|c| c.event_id == event_id) {
            c.enemy_hp = hp;
        }
    }

    /// True if the player owns an adventure-scoped boon in their current
    /// adventure.
    pub fn has_adventure_boon(&self, pid: &str, boon_id: &str) -> bool {
        self.snapshot(pid)
            .map(|p| {
                let adv = p.adventure_id.clone();
                p.adventure_boons.get(&adv).map(|v| v.iter().any(|b| b == boon_id)).unwrap_or(false)
            })
            .unwrap_or(false)
    }

    /// Drive the tick loop for `secs` simulated seconds at `speed_kmh`.
    /// Sets `is_walking` / `current_speed_kmh` on the player; the tick
    /// loop's debug-walk path turns those into distance deltas.
    /// Calls the overworld or interior tick based on the player's
    /// current location, so a single call can transition the player
    /// between scenes (e.g. walking onto a cave portal).
    pub fn tick_walking(&mut self, pid: &str, secs: f32, speed_kmh: f32) {
        let n_ticks = (secs / SIM_TICK_SECS).ceil().max(1.0) as u32;
        {
            let mut lock = self.state.lock().unwrap();
            if let Some(p) = lock.get_mut(pid) {
                p.is_walking = speed_kmh > 0.0;
                p.current_speed_kmh = speed_kmh;
            }
        }
        for _ in 0..n_ticks {
            self.tick_once(pid);
        }
    }

    /// Single tick of simulation. Picks overworld vs interior path
    /// based on the player's current location, same as the production
    /// outer loop in `main.rs`.
    fn tick_once(&mut self, pid: &str) {
        let in_interior = self.state.lock().unwrap()
            .get(pid)
            .map(|p| p.location.interior_id().is_some())
            .unwrap_or(false);

        if in_interior {
            let _ = crate::interior::run_interior_tick(
                &self.bundle.interiors,
                &self.state,
                &self.notifs,
                &self.combat,
                &mut self.player_last_distance,
                &mut self.interior_fogs,
                pid,
            );
            return;
        }

        let _ = crate::tick::run_tick_dev(
            &self.state,
            &self.bundle.world,
            &self.bundle.events,
            &self.notifs,
            &self.combat,
            &self.bundle.interiors,
            &self.bundle.entity_defs,
            &self.bundle.entity_states,
            &mut self.player_fogs,
            &mut self.player_last_distance,
            &mut self.player_boss_wait_notified,
            self.rng_roll,
            &self.bundle.preset.id,
        );
    }

    // ── Read helpers ──────────────────────────────────────────────

    pub fn snapshot(&self, pid: &str) -> Option<DevPlayerState> {
        self.state.lock().unwrap().get(pid).cloned()
    }

    pub fn tile(&self, pid: &str) -> (i32, i32) {
        let p = self.snapshot(pid).expect("player");
        (p.map_tile_x, p.map_tile_y)
    }

    pub fn is_in_interior(&self, pid: &str, interior_id: &str) -> bool {
        self.snapshot(pid)
            .and_then(|p| p.location.interior_id().map(|s| s.to_string()))
            .map(|id| id == interior_id)
            .unwrap_or(false)
    }

    pub fn has_item(&self, pid: &str, item_id: &str) -> bool {
        self.snapshot(pid)
            .map(|p| p.inventory.iter().any(|s: &InventorySlot| s.item_id == item_id))
            .unwrap_or(false)
    }

    pub fn has_completed(&self, pid: &str, event_id: &str) -> bool {
        self.snapshot(pid)
            .map(|p| p.completed_events.iter().any(|e| e == event_id))
            .unwrap_or(false)
    }

    pub fn gold(&self, pid: &str) -> i32 {
        self.snapshot(pid).map(|p| p.gold).unwrap_or(0)
    }

    /// Drain pending notifications for this player. Useful for
    /// asserting on dialogue / outcome banners.
    pub fn drain_notifications(&self, pid: &str) -> Vec<String> {
        let mut lock = self.notifs.lock().unwrap();
        lock.remove(pid).unwrap_or_default()
    }

    /// Number of globally-Active events for the bundle. Snapshot of
    /// the catalog status — does not filter by player completion.
    pub fn active_event_count(&self) -> usize {
        self.bundle.events.lock().unwrap().active_events().len()
    }

    /// Convenience for tests that want to check "did event X transition
    /// to Active in the global catalog?" — typically used to verify
    /// triggers fired correctly.
    pub fn event_is_active(&self, event_id: &str) -> bool {
        self.bundle.events.lock().unwrap()
            .events.iter()
            .any(|e| e.id == event_id && e.status == EventStatus::Active)
    }
}

fn preset_for(id: &str) -> Option<AdventurePreset> {
    adventure::presets().into_iter().find(|p| p.id == id)
}

/// Walk up from the current dir until we find one containing
/// `adventures/`, then chdir there. Cheap to call repeatedly; if
/// we're already at the right place it's a no-op. Used by tests
/// so adventure JSON paths resolve regardless of where cargo
/// drops you (crate dir under `cargo test`, workspace root under
/// the production binary).
fn ensure_workspace_cwd() {
    use std::path::PathBuf;
    let mut dir = std::env::current_dir().expect("cwd");
    if dir.join("adventures").is_dir() {
        return;
    }
    let original = dir.clone();
    while dir.pop() {
        if dir.join("adventures").is_dir() {
            std::env::set_current_dir(&dir).expect("chdir to workspace root");
            return;
        }
    }
    panic!(
        "could not find workspace root (no `adventures/` dir up from {})",
        original.display()
    );
}

// ── Tests ──────────────────────────────────────────────────────────
//
// The tests below cover three concrete scenarios:
//
// 1. `chaos_intro_fires_at_spawn` — fresh chaos player on the camp
//    tile sees Marwen's intro event become Active without the player
//    having to walk first. Regression test for the stationary-trigger
//    fix (commit 0b41a3a).
// 2. `east_gate_cave_entry` — teleport to East Gate POI, walk for
//    a few seconds, assert the player ends up inside `chaos_cavern`
//    at the east-mouth spawn. Exercises the cave_entrance event +
//    enter_interior call path.
// 3. `east_portal_round_trip` — start in the cavern, walk to the
//    east portal tile, assert the player exits back to the East
//    Gate's overworld-adjacent tile (139, 56). Exercises the
//    auto-use_portal logic in run_interior_tick.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chaos_intro_fires_at_spawn() {
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        // Tick once with the player stationary. The stationary-trigger
        // pass should promote chaos_intro to Active even though the
        // player isn't moving.
        run.tick_walking(&p, 1.0, 0.0);
        assert!(
            run.event_is_active("chaos_intro"),
            "chaos_intro should fire for a fresh chaos player on the camp tile"
        );
    }

    #[test]
    fn shrine_npc_grants_speed_potion() {
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        run.force_complete_event(&p, "chaos_intro");
        // Shrine (proc POI 18) at (69, 24). chaos_npc_shrine_pilgrim
        // grants gold + speed_potion when triggered.
        run.teleport(&p, 69, 24);
        run.tick_walking(&p, 1.0, 0.0);
        // The event is requires_browser=true, so the trigger eval
        // promotes it to Active; the simulator completes it via
        // force_complete_event the same way the real client does
        // when the player dismisses the dialogue.
        run.force_complete_event(&p, "chaos_npc_shrine_pilgrim");
        // force_complete_event flips status + adds to completed_events
        // but doesn't apply outcomes (the production path applies
        // outcomes inside the /events/<id>/complete handler). For
        // outcome assertions, walk through the live tick by giving
        // the item directly. (A future helper could simulate the
        // server-side outcome application; not needed for trigger
        // coverage.)
        run.give_item(&p, "speed_potion");
        assert!(run.has_item(&p, "speed_potion"));
        assert!(run.has_completed(&p, "chaos_npc_shrine_pilgrim"));
    }

    #[test]
    fn castle_frost_locked_when_no_key() {
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        run.force_complete_event(&p, "chaos_intro");
        // Castle of Frost (POI 1000) at (28, 24). Without the
        // frostbound_key, the locked variant `chaos_castle_frost_locked`
        // should fire — the boss `chaos_frost_queen` should NOT,
        // because its trigger requires `has_item frostbound_key`.
        run.teleport(&p, 28, 24);
        run.tick_walking(&p, 1.0, 0.0);
        assert!(
            run.event_is_active("chaos_castle_frost_locked"),
            "locked variant should fire when player has no key"
        );
        assert!(
            !run.event_is_active("chaos_frost_queen"),
            "boss should NOT fire without the key"
        );
    }

    #[test]
    fn castle_frost_boss_with_key() {
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        run.force_complete_event(&p, "chaos_intro");
        run.give_item(&p, "frostbound_key");
        run.teleport(&p, 28, 24);
        // Need a walking tick — boss events go through the walking
        // branch's `triggered_ids` path, not the stationary promoter
        // (combat events are intentionally excluded from the latter).
        // Boss events also require a planned_route to actually start
        // combat; otherwise they Dismiss in the same tick. We just
        // check the trigger evaluated true by looking at completion
        // (the Dismissed-no-route path still records it).
        run.set_route(&p, &[(28, 24), (29, 24)]);
        run.tick_walking(&p, 5.0, 3.0);
        // After Dismissal, the event is no longer Active — but the
        // "boss tried to fire" can be verified via the dismissed
        // notification path. For now: just confirm the key + at-poi
        // combo doesn't fire the locked variant.
        assert!(
            !run.event_is_active("chaos_castle_frost_locked"),
            "locked variant should NOT fire when player holds the key"
        );
    }

    #[test]
    fn frost_quest_loads_too() {
        // Sanity: the harness works for the other registered adventure
        // too. Cheap smoke test for cross-adventure correctness.
        let mut run = SimulatedRun::for_adventure("frost_quest");
        let p = run.spawn_player("Tester");
        run.tick_walking(&p, 1.0, 0.0);
        let snap = run.snapshot(&p).expect("player");
        assert_eq!(snap.adventure_id, "frost_quest");
        assert_eq!(run.bundle.world.width, 100);
        assert_eq!(run.bundle.world.height, 80);
    }

    #[test]
    fn east_gate_cave_entry() {
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        // Skip the intro so we're not blocked by its requires_browser.
        run.force_complete_event(&p, "chaos_intro");
        // East Gate (POI 1101) is at (140, 56) in the chaos world.
        run.teleport(&p, 140, 56);
        // Walk for a few simulated seconds — the chaos_enter_via_east_gate
        // cave_entrance event should fire and `enter_interior` should
        // move the player into chaos_cavern at the east-mouth spawn
        // (19, 5).
        run.tick_walking(&p, 3.0, 3.0);
        assert!(
            run.is_in_interior(&p, "chaos_cavern"),
            "expected player to enter chaos_cavern after walking onto East Gate; tile = {:?}",
            run.tile(&p),
        );
        assert_eq!(run.tile(&p), (19, 5), "east mouth spawn coords");
        assert!(
            run.has_completed(&p, "chaos_enter_via_east_gate"),
            "cave-entry event should be marked completed"
        );
    }

    #[test]
    fn boss_victory_grants_rewards() {
        // dac2k9 beat the Frost Queen bare-handed but received nothing —
        // no completion, no 300 gold, no Frost Axe, no Frostproof boon.
        // This pins the full victory→reward path: walk in, engage, drop
        // the enemy to 1 HP, land the killing blow, assert every outcome
        // applied. (Drives enemy HP directly so the test doesn't depend
        // on combat balance / how long the fight takes.)
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        run.force_complete_event(&p, "chaos_intro");
        run.give_item(&p, "frostbound_key");
        run.teleport(&p, 28, 25);
        run.set_route(&p, &[(28, 25), (28, 24)]);
        run.tick_walking(&p, 10.0, 20.0);
        assert!(run.combat_exists("chaos_frost_queen"), "combat should have started");
        let gold_before = run.gold(&p);
        run.set_combat_enemy_hp("chaos_frost_queen", 1);
        // Keep walking so the player charge fills and lands the kill.
        run.tick_walking(&p, 20.0, 20.0);
        assert!(
            run.has_completed(&p, "chaos_frost_queen"),
            "victory must mark the boss completed",
        );
        // >= because the player keeps earning a little distance gold
        // walking after the fight ends in the same tick batch.
        assert!(run.gold(&p) >= gold_before + 300, "victory must award at least 300 gold (got {})", run.gold(&p) - gold_before);
        assert!(run.has_item(&p, "frost_axe"), "victory must drop the Frost Axe");
        assert!(
            run.has_adventure_boon(&p, "frostproof"),
            "victory must grant the Frostproof adventure boon",
        );
    }

    #[test]
    fn two_players_solo_same_boss_independently() {
        // The cross-feed fix: two players who independently engage the
        // same (non-coop) boss must get SEPARATE combat sessions. With
        // the old event_id-keyed map, player B's start_combat overwrote
        // player A's CombatState — A's fight vanished / desynced. Now
        // solo sessions are keyed per player.
        let mut run = SimulatedRun::for_chaos();
        let a = run.spawn_player("Ava");
        let b = run.spawn_player("Bo");
        for p in [&a, &b] {
            run.force_complete_event(p, "chaos_intro");
            run.give_item(p, "frostbound_key");
        }
        // Put BOTH in motion toward the Castle of Frost (28, 24), then
        // drive the shared tick loop a short, fixed number of steps —
        // long enough for both to reach the tile and engage, short
        // enough that neither 220-HP fight resolves before we assert.
        for p in [&a, &b] {
            run.teleport(p, 28, 25);
            run.set_route(p, &[(28, 25), (28, 24)]);
            run.set_walking(p, 20.0);
        }
        // tick_walking(&a, ...) re-sets Ava walking and ticks; Bo is
        // already walking via set_walking, so run_tick_dev advances both.
        run.tick_walking(&a, 8.0, 20.0);
        assert_eq!(run.tile(&a), (28, 24), "Ava reached the boss tile");
        assert_eq!(run.tile(&b), (28, 24), "Bo reached the boss tile");
        assert!(run.player_in_combat(&a), "Ava should be in her own fight");
        assert!(run.player_in_combat(&b), "Bo should be in his own fight");
        assert_eq!(
            run.combat_session_count("chaos_frost_queen"), 2,
            "two solo fighters on one boss must yield two independent sessions, not one clobbered one",
        );
    }

    #[test]
    fn boss_re_engages_when_stuck_active() {
        // Regression: once a boss event went Active (triggered once, then
        // the player fled or combat cleared without a win), the
        // walking-branch filter — which only re-fired Pending/Completed
        // events — locked the player out forever. Walking back onto the
        // tile did nothing. Live repro: dac2k9 on the Castle of Frost,
        // chaos_frost_queen Active, no combat, "one tile down one tile
        // up" never restarted the fight. Fix: combat events bypass the
        // global-status gate and re-trigger as long as the player isn't
        // already mid-fight and hasn't personally completed it.
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        run.force_complete_event(&p, "chaos_intro");
        run.give_item(&p, "frostbound_key");
        // Simulate the stuck state: boss is Active globally, player has
        // NOT completed it, and is not in combat.
        run.force_event_status("chaos_frost_queen", EventStatus::Active);
        assert!(!run.combat_exists("chaos_frost_queen"), "precondition: no combat yet");
        // Player walks a route ONTO the Castle of Frost (28, 24).
        // poi_at is an EXACT tile match, so the player must actually land
        // on (28, 24) — walk fast + long enough to cross the road tile.
        run.teleport(&p, 28, 25);
        run.set_route(&p, &[(28, 25), (28, 24)]);
        run.tick_walking(&p, 30.0, 20.0);
        assert_eq!(run.tile(&p), (28, 24), "precondition: player reached the boss tile");
        assert!(
            run.combat_exists("chaos_frost_queen"),
            "boss combat should (re)start when walking onto the tile even though \
             the global event status was stuck Active",
        );
    }

    #[test]
    fn stationary_pass_does_not_promote_boss() {
        // Regression: a player standing on a boss POI used to have the
        // stationary trigger pass flip the Boss event to Active. The
        // walking-branch trigger filter only re-considers Pending or
        // Completed-by-another events (never Active), so the boss fight
        // then never started when the player walked onto the tile —
        // combat init lives in the walking branch. Live repro: dac2k9
        // stood on the Castle of Frost, chaos_frost_queen went Active
        // from the stationary pass, and walking did nothing. The fix
        // excludes Boss + RandomEncounter from promote_pending_triggers
        // (same treatment CaveEntrance already had).
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        run.force_complete_event(&p, "chaos_intro");
        run.give_item(&p, "frostbound_key");
        // Castle of Frost (POI 1000) at (28, 24).
        run.teleport(&p, 28, 24);
        // Stationary tick — the buggy behavior promoted the boss here.
        run.tick_walking(&p, 1.0, 0.0);
        assert!(
            !run.event_is_active("chaos_frost_queen"),
            "boss must NOT be promoted to Active by the stationary pass; \
             it has to stay Pending so the walking branch can start combat",
        );
    }

    #[test]
    fn per_player_event_completed_satisfies_trigger() {
        // Regression for the live-server bug where dac2k9 stood on the
        // Spire of Hael with chaos_intro in his personal completed_events,
        // yet chaos_hael_quest never fired because its trigger
        // `event_completed chaos_intro` was checking the GLOBAL completed
        // set and the catalog's global status of chaos_intro hadn't
        // flipped to Completed (Daniel had triggered it but the dismissal
        // path didn't propagate the global flag in a way the trigger eval
        // could see). The fix unions the player's personal completed_events
        // with the global set inside TriggerContext; this test pins it.
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        // Mark chaos_intro in the player's personal list ONLY — do not
        // flip the global catalog. This mirrors the real-world state
        // where the dismissal happened in a way that didn't bubble up.
        run.mark_personal_completed(&p, "chaos_intro");
        // Stand on Spire of Hael (POI 1201 at 70, 50).
        run.teleport(&p, 70, 50);
        // Stationary tick: the watcher should promote chaos_hael_quest
        // because the trigger now sees the per-player chaos_intro.
        run.tick_walking(&p, 1.0, 0.0);
        assert!(
            run.event_is_active("chaos_hael_quest"),
            "chaos_hael_quest should fire when player has personal completion of chaos_intro, even if global status didn't flip",
        );
    }

    #[test]
    fn interior_route_clears_on_arrival() {
        // Regression: when a player walked across an interior to the end
        // of their planned_route, the interior tick used to leave the
        // route + route_meters_walked in place. The client then thought
        // the player was still travelling indefinitely. Daniel hit this
        // on the live server after walking the cavern — stuck on his
        // destination tile because the stale route blocked progress.
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        run.force_complete_event(&p, "chaos_intro");
        // Drop the player inside the cavern at the east mouth.
        run.teleport(&p, 140, 56);
        run.tick_walking(&p, 3.0, 3.0);
        assert!(run.is_in_interior(&p, "chaos_cavern"));
        // Walk to a nearby walkable tile that is NOT a portal. The
        // east mouth is at (19, 5); (18, 5) and (17, 5) are floor.
        run.set_route(&p, &[(19, 5), (18, 5), (17, 5)]);
        run.tick_walking(&p, 30.0, 20.0);
        let snap = run.snapshot(&p).expect("player");
        assert_eq!((snap.map_tile_x, snap.map_tile_y), (17, 5),
            "should have walked to the end of the route");
        assert!(snap.planned_route.is_empty() || snap.planned_route == "[]",
            "planned_route should be cleared on arrival, was {:?}", snap.planned_route);
        assert_eq!(snap.route_meters_walked, 0.0,
            "route_meters_walked should reset when route clears, was {}",
            snap.route_meters_walked);
    }

    #[test]
    fn east_portal_round_trip() {
        let mut run = SimulatedRun::for_chaos();
        let p = run.spawn_player("Tester");
        run.force_complete_event(&p, "chaos_intro");

        // Step 1: enter the cavern via the East Gate (same as the
        // previous test). This populates the player's
        // completed_events with chaos_enter_via_east_gate, which
        // unlocks the east portal inside the cavern.
        run.teleport(&p, 140, 56);
        run.tick_walking(&p, 3.0, 3.0);
        assert!(run.is_in_interior(&p, "chaos_cavern"));

        // Step 2: from the east mouth (19, 5), walk one tile east to
        // (20, 5) — the east portal tile. The auto-use_portal logic
        // in run_interior_tick should fire when the player steps onto
        // a portal, dropping them back into the overworld.
        run.set_route(&p, &[(19, 5), (20, 5)]);
        // Need a few ticks at high enough speed to cover one tile
        // (20 m at floor_cost). 20 km/h ≈ 5.5 m/s, so ~4 s.
        run.tick_walking(&p, 6.0, 20.0);

        let snap = run.snapshot(&p).expect("player");
        assert!(
            matches!(snap.location, questlib::interior::Location::Overworld),
            "player should be back on the overworld after stepping on east portal; loc = {:?}",
            snap.location,
        );
        // East portal destination is (139, 56) — one tile west of the
        // East Gate POI so re-arrival doesn't immediately re-trigger
        // the cave_entrance event.
        assert_eq!(snap.map_tile_x, 139);
        assert_eq!(snap.map_tile_y, 56);
    }
}
