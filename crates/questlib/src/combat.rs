//! JRPG combat system — shared types and formulas.
//!
//! Pure logic, no Bevy. Used by both gamemaster (authoritative) and gameclient (prediction).

use serde::{Deserialize, Serialize};

use crate::events::kind::EventKind;
use crate::leveling;

// ── Types ────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CombatStatus {
    /// Both charge bars filling.
    Charging,
    /// Player charge full — waiting for action input.
    PlayerTurn,
    /// Enemy charge full — enemy attacks this tick.
    EnemyAttacking,
    /// Enemy HP reached 0.
    Victory,
    /// Player HP reached 0 (auto-retries with full heal).
    Defeat,
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
    pub status: CombatStatus,

    // Player
    pub player_hp: i32,
    pub player_max_hp: i32,
    pub player_attack: i32,
    pub player_defense: i32,
    pub player_level: u32,
    pub player_charge: f32,
    pub player_defending: bool,

    // Enemy
    pub enemy_name: String,
    pub enemy_hp: i32,
    pub enemy_max_hp: i32,
    pub enemy_attack: i32,
    pub enemy_defense: i32,
    pub enemy_charge: f32,
    pub difficulty: u32,

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

/// Enemy attack damage. Halved if player is defending.
pub fn enemy_damage(enemy_attack: i32, player_defense: i32, player_defending: bool) -> i32 {
    let def_mult = if player_defending { 2.0 } else { 1.0 };
    let raw = enemy_attack as f32 - (player_defense as f32 * def_mult / 2.0);
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

// ── Combat Initialization ────────────────────────────

/// Create a new combat state from event data and player distance.
pub fn init_combat(event_id: &str, kind: &EventKind, total_distance_m: u64) -> CombatState {
    let player_level = leveling::level_from_meters(total_distance_m);
    let stats = leveling::CharacterStats::new_at_level(player_level);
    let enemy = enemy_stats_from_event(kind, player_level);
    let name = enemy_name_from_event(kind);

    CombatState {
        event_id: event_id.to_string(),
        status: CombatStatus::Charging,
        player_hp: stats.max_hp,
        player_max_hp: stats.max_hp,
        player_attack: stats.attack,
        player_defense: stats.defense,
        player_level,
        player_charge: 0.0,
        player_defending: false,
        enemy_name: name,
        enemy_hp: enemy.max_hp,
        enemy_max_hp: enemy.max_hp,
        enemy_attack: enemy.attack,
        enemy_defense: enemy.defense,
        enemy_charge: 0.0,
        difficulty: enemy.difficulty,
        turn_log: Vec::new(),
    }
}

// ── Combat Tick (server-side) ────────────────────────

/// Advance combat by one tick. Returns true if combat ended (Victory or Defeat).
pub fn tick_combat(state: &mut CombatState, speed_kmh: f32, delta_secs: f32) -> bool {
    match state.status {
        CombatStatus::Victory | CombatStatus::Defeat => return true,
        CombatStatus::PlayerTurn => return false, // waiting for player action
        _ => {}
    }

    // Advance charge bars
    state.player_charge += player_charge_rate(speed_kmh) * delta_secs;
    state.enemy_charge += enemy_charge_rate(state.difficulty) * delta_secs;

    // Enemy acts when charge full
    if state.enemy_charge >= 1.0 {
        let dmg = enemy_damage(state.enemy_attack, state.player_defense, state.player_defending);
        state.player_hp = (state.player_hp - dmg).max(0);
        state.player_defending = false; // defend consumed
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

    // Player can act when charge full
    if state.player_charge >= 1.0 {
        state.player_charge = 1.0;
        state.status = CombatStatus::PlayerTurn;
    }

    false
}

/// Apply a player action. Returns true if combat ended.
pub fn apply_player_action(state: &mut CombatState, action: &str, incline_pct: f32) -> bool {
    if state.status != CombatStatus::PlayerTurn {
        return false;
    }

    match action {
        "attack" => {
            let dmg = player_damage(state.player_attack, state.enemy_defense, incline_pct);
            state.enemy_hp = (state.enemy_hp - dmg).max(0);
            state.turn_log.push(CombatLogEntry {
                actor: "You".to_string(),
                action: "attack".to_string(),
                damage: dmg,
                message: format!("You attack for {} damage!", dmg),
            });

            if state.enemy_hp <= 0 {
                state.status = CombatStatus::Victory;
                return true;
            }
        }
        "defend" => {
            state.player_defending = true;
            state.turn_log.push(CombatLogEntry {
                actor: "You".to_string(),
                action: "defend".to_string(),
                damage: 0,
                message: "You brace for the next attack!".to_string(),
            });
        }
        _ => {}
    }

    state.player_charge = 0.0;
    state.status = CombatStatus::Charging;
    false
}

/// Reset combat after defeat (full heal, restart).
pub fn retry_combat(state: &mut CombatState) {
    state.player_hp = state.player_max_hp;
    state.player_charge = 0.0;
    state.enemy_charge = 0.0;
    state.player_defending = false;
    state.enemy_hp = state.enemy_max_hp;
    state.status = CombatStatus::Charging;
    state.turn_log.clear();
    state.turn_log.push(CombatLogEntry {
        actor: "System".to_string(),
        action: "retry".to_string(),
        damage: 0,
        message: "You steel yourself and try again!".to_string(),
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
        assert!(player_charge_rate(10.0) <= 0.5); // capped

        assert!(enemy_charge_rate(1) > 0.07);
        assert!(enemy_charge_rate(5) > enemy_charge_rate(1));
    }

    #[test]
    fn damage_formulas() {
        // Basic attack
        let dmg = player_damage(10, 4, 0.0);
        assert!(dmg >= 1);
        assert_eq!(dmg, 8); // 10 - 4/2 = 8

        // Incline bonus (5% = 1.5x)
        let dmg_incline = player_damage(10, 4, 5.0);
        assert!(dmg_incline > dmg);

        // Enemy damage
        let edmg = enemy_damage(8, 6, false);
        assert_eq!(edmg, 5); // 8 - 6/2 = 5

        // Defending halves
        let edmg_def = enemy_damage(8, 6, true);
        assert!(edmg_def < edmg);

        // Minimum 1
        assert_eq!(player_damage(1, 100, 0.0), 1);
        assert_eq!(enemy_damage(1, 100, false), 1);
    }

    #[test]
    fn enemy_stats_scale() {
        let boss = enemy_stats_from_event(
            &EventKind::Boss {
                boss_name: "Troll".into(),
                max_hp: 500,
                portrait: None,
                dialogue_intro: vec![],
                dialogue_defeat: vec![],
            },
            5,
        );
        assert_eq!(boss.max_hp, 500);
        assert_eq!(boss.attack, 14); // 4 + 5*2
        assert_eq!(boss.defense, 7); // 2 + 5

        let enc = enemy_stats_from_event(
            &EventKind::RandomEncounter {
                enemy_name: "Wolf".into(),
                description: "A wolf".into(),
                difficulty: 2,
            },
            5,
        );
        assert_eq!(enc.max_hp, 20 + 30 + 40); // 20 + 2*15 + 5*8
        assert_eq!(enc.attack, 12); // 3 + 2*2 + 5
    }

    #[test]
    fn combat_lifecycle() {
        let kind = EventKind::RandomEncounter {
            enemy_name: "Slime".into(),
            description: "A weak slime".into(),
            difficulty: 1,
        };
        let mut combat = init_combat("test_event", &kind, 500);
        assert_eq!(combat.status, CombatStatus::Charging);
        assert!(combat.player_hp > 0);
        assert!(combat.enemy_hp > 0);

        // Tick until player can act (simulate 3 km/h walking)
        for _ in 0..100 {
            if combat.status == CombatStatus::PlayerTurn {
                break;
            }
            tick_combat(&mut combat, 3.0, 0.1);
        }
        assert_eq!(combat.status, CombatStatus::PlayerTurn);

        // Attack
        let old_hp = combat.enemy_hp;
        apply_player_action(&mut combat, "attack", 0.0);
        assert!(combat.enemy_hp < old_hp);
        assert_eq!(combat.status, CombatStatus::Charging);
    }

    #[test]
    fn defend_reduces_damage() {
        let kind = EventKind::RandomEncounter {
            enemy_name: "Goblin".into(),
            description: "test".into(),
            difficulty: 2,
        };
        let mut combat = init_combat("test", &kind, 1000);

        // Charge player and defend
        combat.player_charge = 1.0;
        combat.status = CombatStatus::PlayerTurn;
        apply_player_action(&mut combat, "defend", 0.0);
        assert!(combat.player_defending);

        // Enemy attacks — damage should be reduced
        let hp_before = combat.player_hp;
        combat.enemy_charge = 1.0;
        tick_combat(&mut combat, 0.0, 0.0);
        let hp_after = combat.player_hp;
        let dmg_defended = hp_before - hp_after;

        // Reset and take undefended hit
        combat.player_hp = hp_before;
        combat.player_defending = false;
        combat.enemy_charge = 1.0;
        combat.status = CombatStatus::Charging;
        tick_combat(&mut combat, 0.0, 0.0);
        let dmg_undefended = hp_before - combat.player_hp;

        assert!(dmg_defended <= dmg_undefended);
    }

    #[test]
    fn defeat_and_retry() {
        let kind = EventKind::RandomEncounter {
            enemy_name: "Dragon".into(),
            description: "test".into(),
            difficulty: 5,
        };
        let mut combat = init_combat("test", &kind, 100);

        // Force defeat
        combat.player_hp = 1;
        combat.enemy_charge = 1.0;
        let ended = tick_combat(&mut combat, 0.0, 0.0);
        assert!(ended);
        assert_eq!(combat.status, CombatStatus::Defeat);

        // Retry
        retry_combat(&mut combat);
        assert_eq!(combat.status, CombatStatus::Charging);
        assert_eq!(combat.player_hp, combat.player_max_hp);
        assert_eq!(combat.enemy_hp, combat.enemy_max_hp);
    }
}
