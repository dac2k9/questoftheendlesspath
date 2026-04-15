//! Auto-attack combat system — walking speed drives the fight.
//!
//! Both player and enemy auto-attack when their charge bars fill.
//! The only player action is "run away" to escape a losing fight.
//! Pure logic, no Bevy. Used by both gamemaster and gameclient.

use serde::{Deserialize, Serialize};

use crate::events::kind::EventKind;
use crate::leveling;

// ── Types ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombatStatus {
    /// Both charge bars filling, auto-attacks fire when full.
    Fighting,
    /// Enemy HP reached 0.
    Victory,
    /// Player HP reached 0 — retreated, enemy stays on map.
    Defeat,
    /// Player chose to run away.
    Fled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombatLogEntry {
    pub actor: String,
    pub action: String,
    pub damage: i32,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CombatState {
    pub event_id: String,
    /// Which player is fighting this combat.
    #[serde(default)]
    pub player_id: String,
    pub status: CombatStatus,
    /// Encounter description shown at combat start.
    #[serde(default)]
    pub description: String,

    // Player
    pub player_hp: i32,
    pub player_max_hp: i32,
    pub player_attack: i32,
    pub player_defense: i32,
    pub player_level: u32,
    pub player_charge: f32,

    // Enemy
    pub enemy_name: String,
    pub enemy_hp: i32,
    pub enemy_max_hp: i32,
    pub enemy_attack: i32,
    pub enemy_defense: i32,
    pub enemy_charge: f32,
    pub difficulty: u32,

    // Balancing info
    pub min_level: u32,
    pub recommended_level: u32,

    // Log
    pub turn_log: Vec<CombatLogEntry>,
}

// ── Charge Rates ─────────────────────────────────────

/// Player charge rate from walking speed.
/// At 3 km/h fills in ~6s, at 6 km/h fills in ~3s.
pub fn player_charge_rate(speed_kmh: f32) -> f32 {
    (speed_kmh * 0.055).min(0.5)
}

/// Enemy charge rate from difficulty (0-5).
/// Difficulty 1 ≈ 14s, difficulty 3 ≈ 10s, difficulty 5 ≈ 8s.
pub fn enemy_charge_rate(difficulty: u32) -> f32 {
    0.06 + (difficulty as f32 * 0.015)
}

// ── Damage Formulas ──────────────────────────────────

/// Player attack damage. Incline (0-15%) boosts damage.
pub fn player_damage(player_attack: i32, enemy_defense: i32, incline_pct: f32) -> i32 {
    let incline_mult = 1.0 + (incline_pct.max(0.0) * 0.1);
    let raw = (player_attack as f32 * incline_mult) - (enemy_defense as f32 / 2.0);
    raw.round().max(1.0) as i32
}

/// Enemy attack damage.
pub fn enemy_damage(enemy_attack: i32, player_defense: i32) -> i32 {
    let raw = enemy_attack as f32 - (player_defense as f32 / 2.0);
    raw.round().max(1.0) as i32
}

// ── Enemy Stats ──────────────────────────────────────

pub struct EnemyStats {
    pub max_hp: i32,
    pub attack: i32,
    pub defense: i32,
    pub difficulty: u32,
}

/// Generate enemy stats from event kind + player level.
pub fn enemy_stats_from_event(kind: &EventKind, player_level: u32) -> EnemyStats {
    let lvl = player_level as i32;
    match kind {
        EventKind::Boss { max_hp, .. } => EnemyStats {
            max_hp: *max_hp,
            attack: 4 + lvl * 2,
            defense: 2 + lvl,
            difficulty: 3,
        },
        EventKind::RandomEncounter { difficulty, .. } => {
            let d = *difficulty as i32;
            EnemyStats {
                max_hp: 20 + d * 15 + lvl * 8,
                attack: 3 + d * 2 + lvl,
                defense: 1 + d + lvl / 2,
                difficulty: *difficulty,
            }
        }
        _ => EnemyStats {
            max_hp: 50,
            attack: 5,
            defense: 3,
            difficulty: 1,
        },
    }
}

/// Extract enemy name from event kind.
pub fn enemy_name_from_event(kind: &EventKind) -> String {
    match kind {
        EventKind::Boss { boss_name, .. } => boss_name.clone(),
        EventKind::RandomEncounter { enemy_name, .. } => enemy_name.clone(),
        _ => "Enemy".to_string(),
    }
}

pub fn encounter_description(kind: &EventKind) -> String {
    match kind {
        EventKind::RandomEncounter { description, .. } => description.clone(),
        EventKind::Boss { dialogue_intro, boss_name, .. } => {
            dialogue_intro.first().cloned().unwrap_or_else(|| format!("{} appears!", boss_name))
        }
        _ => String::new(),
    }
}

// ── Encounter Balancing ──────────────────────────────

/// Simulate a fight at a given level and speed. Returns true if player wins.
fn simulate_fight(player_level: u32, enemy: &EnemyStats, speed_kmh: f32) -> bool {
    let stats = leveling::CharacterStats::new_at_level(player_level);
    let p_dmg = player_damage(stats.attack, enemy.defense, 0.0).max(1);
    let e_dmg = enemy_damage(enemy.attack, stats.defense).max(1);

    let p_rate = player_charge_rate(speed_kmh);
    let e_rate = enemy_charge_rate(enemy.difficulty);

    if p_rate < 0.001 { return false; } // not walking = can't win

    // Time to fill charge bars
    let p_fill = 1.0 / p_rate;
    let e_fill = 1.0 / e_rate;

    // Attacks per second
    let p_dps = p_dmg as f64 / p_fill as f64;
    let e_dps = e_dmg as f64 / e_fill as f64;

    // Time to kill
    let time_to_kill_enemy = enemy.max_hp as f64 / p_dps;
    let time_to_kill_player = stats.max_hp as f64 / e_dps;

    time_to_kill_enemy < time_to_kill_player
}

/// Minimum level to beat this enemy at max walking speed (5 km/h).
pub fn min_level(kind: &EventKind) -> u32 {
    for lvl in 1..=100 {
        let enemy = enemy_stats_from_event(kind, lvl);
        if simulate_fight(lvl, &enemy, 5.0) {
            return lvl;
        }
    }
    100
}

/// Recommended level to beat this enemy at comfortable speed (3 km/h).
pub fn recommended_level(kind: &EventKind) -> u32 {
    for lvl in 1..=100 {
        let enemy = enemy_stats_from_event(kind, lvl);
        if simulate_fight(lvl, &enemy, 3.0) {
            return lvl;
        }
    }
    100
}

// ── Combat Initialization ────────────────────────────

/// Create a new combat state from event data and player distance.
pub fn init_combat(event_id: &str, kind: &EventKind, total_distance_m: u64, equipment_bonuses: (i32, i32, i32), player_id: &str) -> CombatState {
    let player_level = leveling::level_from_meters(total_distance_m);
    let stats = leveling::CharacterStats::new_at_level(player_level);
    let enemy = enemy_stats_from_event(kind, player_level);
    let name = enemy_name_from_event(kind);
    let desc = encounter_description(kind);
    let min_lvl = min_level(kind);
    let rec_lvl = recommended_level(kind);

    let (eq_atk, eq_def, eq_hp) = equipment_bonuses;

    CombatState {
        event_id: event_id.to_string(),
        player_id: player_id.to_string(),
        status: CombatStatus::Fighting,
        description: desc,
        player_hp: stats.max_hp + eq_hp,
        player_max_hp: stats.max_hp + eq_hp,
        player_attack: stats.attack + eq_atk,
        player_defense: stats.defense + eq_def,
        player_level,
        player_charge: 0.0,
        enemy_name: name,
        enemy_hp: enemy.max_hp,
        enemy_max_hp: enemy.max_hp,
        enemy_attack: enemy.attack,
        enemy_defense: enemy.defense,
        enemy_charge: 0.0,
        difficulty: enemy.difficulty,
        min_level: min_lvl,
        recommended_level: rec_lvl,
        turn_log: Vec::new(),
    }
}

// ── Combat Tick (server-side) ────────────────────────

/// Advance combat by one tick. Both sides auto-attack when charge fills.
/// Returns true if combat ended (Victory or Defeat).
pub fn tick_combat(state: &mut CombatState, speed_kmh: f32, incline_pct: f32, delta_secs: f32) -> bool {
    match state.status {
        CombatStatus::Victory | CombatStatus::Defeat | CombatStatus::Fled => return true,
        _ => {}
    }

    // Advance charge bars
    state.player_charge += player_charge_rate(speed_kmh) * delta_secs;
    state.enemy_charge += enemy_charge_rate(state.difficulty) * delta_secs;

    // Player auto-attacks when charge full
    if state.player_charge >= 1.0 {
        let dmg = player_damage(state.player_attack, state.enemy_defense, incline_pct);
        state.enemy_hp = (state.enemy_hp - dmg).max(0);
        state.player_charge = 0.0;

        state.turn_log.push(CombatLogEntry {
            actor: "You".to_string(),
            action: "attack".to_string(),
            damage: dmg,
            message: format!("You strike for {} damage!", dmg),
        });

        if state.enemy_hp <= 0 {
            state.status = CombatStatus::Victory;
            return true;
        }
    }

    // Enemy auto-attacks when charge full
    if state.enemy_charge >= 1.0 {
        let dmg = enemy_damage(state.enemy_attack, state.player_defense);
        state.player_hp = (state.player_hp - dmg).max(0);
        state.enemy_charge = 0.0;

        state.turn_log.push(CombatLogEntry {
            actor: state.enemy_name.clone(),
            action: "attack".to_string(),
            damage: dmg,
            message: format!("{} attacks for {} damage!", state.enemy_name, dmg),
        });

        if state.player_hp <= 0 {
            state.status = CombatStatus::Defeat;
            return true;
        }
    }

    false
}

/// Player runs away. Combat ends, enemy stays on map.
pub fn flee_combat(state: &mut CombatState) {
    state.status = CombatStatus::Fled;
    state.turn_log.push(CombatLogEntry {
        actor: "You".to_string(),
        action: "flee".to_string(),
        damage: 0,
        message: "You run away!".to_string(),
    });
}

// ── Tests ────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charge_rates() {
        assert!(player_charge_rate(0.0) < 0.001);
        assert!((player_charge_rate(3.0) - 0.165).abs() < 0.001);
        assert!(player_charge_rate(10.0) <= 0.5);

        assert!(enemy_charge_rate(1) > 0.07);
        assert!(enemy_charge_rate(5) > enemy_charge_rate(1));
    }

    #[test]
    fn damage_formulas() {
        let dmg = player_damage(10, 4, 0.0);
        assert_eq!(dmg, 8); // 10 - 4/2 = 8

        let dmg_incline = player_damage(10, 4, 5.0);
        assert!(dmg_incline > dmg);

        let edmg = enemy_damage(8, 6);
        assert_eq!(edmg, 5); // 8 - 6/2 = 5

        assert_eq!(player_damage(1, 100, 0.0), 1);
        assert_eq!(enemy_damage(1, 100), 1);
    }

    #[test]
    fn auto_attack_combat() {
        let kind = EventKind::RandomEncounter {
            enemy_name: "Slime".into(),
            description: "A weak slime".into(),
            difficulty: 1,
        };
        let mut combat = init_combat("test", &kind, 5000, (0, 0, 0), "player1");
        assert_eq!(combat.status, CombatStatus::Fighting);

        // Simulate at 3 km/h until combat ends
        let mut ticks = 0;
        loop {
            let ended = tick_combat(&mut combat, 3.0, 0.0, 0.1);
            ticks += 1;
            if ended || ticks > 10000 { break; }
        }

        // Should have ended (either victory or defeat)
        assert!(combat.status == CombatStatus::Victory || combat.status == CombatStatus::Defeat);
        assert!(!combat.turn_log.is_empty());
    }

    #[test]
    fn flee_ends_combat() {
        let kind = EventKind::RandomEncounter {
            enemy_name: "Dragon".into(),
            description: "test".into(),
            difficulty: 5,
        };
        let mut combat = init_combat("test", &kind, 100, (0, 0, 0), "player1");

        flee_combat(&mut combat);
        assert_eq!(combat.status, CombatStatus::Fled);
        assert!(tick_combat(&mut combat, 3.0, 0.0, 1.0)); // returns true (ended)
    }

    #[test]
    fn encounter_balancing() {
        let kind = EventKind::RandomEncounter {
            enemy_name: "Wolf".into(),
            description: "test".into(),
            difficulty: 2,
        };
        let min = min_level(&kind);
        let rec = recommended_level(&kind);

        assert!(min >= 1);
        assert!(rec >= min);
        // At recommended level + 3 km/h, player should win
        let enemy = enemy_stats_from_event(&kind, rec);
        assert!(simulate_fight(rec, &enemy, 3.0));
    }

    #[test]
    fn defeat_preserves_state() {
        let kind = EventKind::RandomEncounter {
            enemy_name: "Goblin".into(),
            description: "test".into(),
            difficulty: 1,
        };
        let mut combat = init_combat("test", &kind, 100, (0, 0, 0), "player1");

        // Force defeat
        combat.player_hp = 1;
        combat.enemy_charge = 1.0;
        let ended = tick_combat(&mut combat, 0.0, 0.0, 0.0);
        assert!(ended);
        assert_eq!(combat.status, CombatStatus::Defeat);
        // Enemy HP should NOT be reset — stays damaged
    }
}
