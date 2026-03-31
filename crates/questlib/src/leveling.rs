//! Character leveling system.
//!
//! Walking distance is the XP. Each level requires more meters than the last.
//! Leveling up grants HP, stats, and unlocks abilities.

use serde::{Deserialize, Serialize};

/// Meters required to reach each level (cumulative).
/// Each level gap grows by 10%: Lv 1→2 = 1000m, Lv 2→3 = 1100m, Lv 3→4 = 1210m, etc.
/// Cumulative: sum of 1000 * 1.1^(i-1) for i=1..level-1
/// This gives: Lv 2 = 1km, Lv 5 = 4.6km, Lv 10 = 15.9km, Lv 20 = 57.3km, Lv 30 = 164km

/// Compute the cumulative meters required to reach a given level.
pub fn meters_for_level(level: u32) -> u64 {
    if level <= 1 {
        return 0;
    }
    // Sum of geometric series: 1000 * (1.1^(n-1) - 1) / (1.1 - 1)
    // = 10000 * (1.1^(n-1) - 1)
    let n = (level - 1) as f64;
    (10000.0 * (1.1_f64.powf(n) - 1.0)) as u64
}

/// Compute the current level from total meters walked.
pub fn level_from_meters(total_meters: u64) -> u32 {
    let mut level = 1;
    loop {
        let next = meters_for_level(level + 1);
        if total_meters < next {
            return level;
        }
        level += 1;
        if level > 999 {
            return level; // safety cap
        }
    }
}

/// Meters needed to reach the NEXT level from current total.
pub fn meters_to_next_level(total_meters: u64) -> u64 {
    let current_level = level_from_meters(total_meters);
    let next_threshold = meters_for_level(current_level + 1);
    next_threshold.saturating_sub(total_meters)
}

/// Progress fraction (0.0-1.0) toward next level.
pub fn level_progress(total_meters: u64) -> f32 {
    let current_level = level_from_meters(total_meters);
    let current_threshold = meters_for_level(current_level);
    let next_threshold = meters_for_level(current_level + 1);
    let range = next_threshold - current_threshold;
    if range == 0 {
        return 1.0;
    }
    let progress = total_meters.saturating_sub(current_threshold);
    (progress as f32 / range as f32).clamp(0.0, 1.0)
}

/// Character stats that scale with level.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterStats {
    pub level: u32,
    pub max_hp: i32,
    pub current_hp: i32,
    pub attack: i32,
    pub defense: i32,
    pub total_meters_walked: u64,
}

impl CharacterStats {
    /// Create stats for a fresh character.
    pub fn new() -> Self {
        Self {
            level: 1,
            max_hp: 50,
            current_hp: 50,
            attack: 5,
            defense: 3,
            total_meters_walked: 0,
        }
    }

    /// Create stats for a given level.
    pub fn new_at_level(level: u32) -> Self {
        let mut s = Self::new();
        s.level = level.max(1);
        s.recalculate_stats();
        s
    }

    /// Update stats from total meters walked. Returns true if leveled up.
    pub fn update_from_meters(&mut self, total_meters: u64) -> bool {
        self.total_meters_walked = total_meters;
        let new_level = level_from_meters(total_meters);
        if new_level > self.level {
            let old_level = self.level;
            self.level = new_level;
            self.recalculate_stats();
            true
        } else {
            false
        }
    }

    /// Recalculate derived stats from level.
    fn recalculate_stats(&mut self) {
        let lvl = self.level as i32;
        self.max_hp = 50 + (lvl - 1) * 15;       // +15 HP per level
        self.current_hp = self.max_hp;              // full heal on level up
        self.attack = 5 + (lvl - 1) * 3;           // +3 attack per level
        self.defense = 3 + (lvl - 1) * 2;          // +2 defense per level
    }
}

impl Default for CharacterStats {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_1_at_zero() {
        assert_eq!(level_from_meters(0), 1);
    }

    #[test]
    fn level_2_threshold() {
        let threshold = meters_for_level(2);
        assert_eq!(threshold, 1000, "level 2 should be at 1000m, got {}m", threshold);
        assert_eq!(level_from_meters(threshold), 2);
        assert_eq!(level_from_meters(threshold - 1), 1);
    }

    #[test]
    fn levels_increase() {
        let l2 = meters_for_level(2);
        let l3 = meters_for_level(3);
        let l5 = meters_for_level(5);
        let l10 = meters_for_level(10);
        assert!(l3 > l2);
        assert!(l5 > l3);
        assert!(l10 > l5);
        println!("Level thresholds: 2={}m, 3={}m, 5={}m, 10={}m, 20={}m, 50={}m",
            l2, l3, l5, l10, meters_for_level(20), meters_for_level(50));
    }

    #[test]
    fn progress_fraction() {
        let l2 = meters_for_level(2);
        assert_eq!(level_progress(0), 0.0);
        assert!(level_progress(l2 / 2) > 0.3);
        assert!(level_progress(l2 / 2) < 0.7);
        assert_eq!(level_progress(l2), 0.0); // just hit level 2, 0% to level 3
    }

    #[test]
    fn meters_to_next() {
        let to_2 = meters_to_next_level(0);
        assert_eq!(to_2, meters_for_level(2));
        assert_eq!(meters_to_next_level(meters_for_level(2)), meters_for_level(3) - meters_for_level(2));
    }

    #[test]
    fn character_levels_up() {
        let mut stats = CharacterStats::new();
        assert_eq!(stats.level, 1);
        assert_eq!(stats.max_hp, 50);

        let leveled = stats.update_from_meters(meters_for_level(2));
        assert!(leveled);
        assert_eq!(stats.level, 2);
        assert_eq!(stats.max_hp, 65); // 50 + 15

        let leveled = stats.update_from_meters(meters_for_level(5));
        assert!(leveled);
        assert_eq!(stats.level, 5);
        assert_eq!(stats.attack, 5 + 4 * 3); // 17
    }

    #[test]
    fn no_level_up_without_progress() {
        let mut stats = CharacterStats::new();
        assert!(!stats.update_from_meters(10));
        assert_eq!(stats.level, 1);
    }

    #[test]
    fn print_level_table() {
        println!("\nLevel Table:");
        for lvl in 1..=30 {
            let m = meters_for_level(lvl);
            let stats_hp = 50 + (lvl as i32 - 1) * 15;
            println!("  Lvl {:2}: {:6}m ({:.1}km) | HP: {}", lvl, m, m as f32 / 1000.0, stats_hp);
        }
    }
}
