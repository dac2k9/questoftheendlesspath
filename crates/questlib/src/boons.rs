//! Boons — permanent meta-progression rewards earned by completing
//! climactic quests (e.g. defeating a final boss). A boon's effect
//! survives across adventures; the player keeps growing in subtle
//! ways even though level / gold / inventory reset on each run.
//!
//! Design choices, for context:
//! - Boons skew toward **speed** and **gold** because combat power
//!   (HP / ATK / DEF) resets each adventure anyway. Permanent stat
//!   buffs would compound until trivial; permanent speed/gold
//!   compounds gently and stays fun.
//! - Pick-one-of-three on completion. The 3 are deterministic per
//!   `(player_id, event_id)` so refreshing the page can't re-roll.
//! - Boons stack additively where it makes sense (multiple speed
//!   boons multiply together, multiple biome-cost boons compose).
//! - Authored as a `&'static [Boon]` catalog — no JSON, no I/O.
//!   Adding a boon = editing the catalog and shipping.

use serde::{Deserialize, Serialize};

use crate::mapgen::Biome;

/// One discrete effect attached to a boon. A single boon can carry
/// multiple effects (e.g. Trailblazer reduces 3 biome costs).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum BoonEffect {
    /// Multiplier on walking-derived meters. >1.0 = faster route advance.
    SpeedMultiplier(f32),
    /// Multiplier on tile cost for a specific biome (lower = faster).
    BiomeCostMultiplier { biome: Biome, mult: f32 },
    /// Multiplier on road-tile cost (stacks with built-in road bonus).
    RoadCostMultiplier(f32),
    /// Speed boost limited to the first N meters of each play session.
    /// Resets when the player rejoins (treated as "session start").
    SessionStartBoost { meters: f32, multiplier: f32 },
    /// Multiplier on gold from any source (chests, monster kills, milestones).
    GoldMultiplier(f32),
    /// One-time gold awarded at adventure start.
    AdventureStartGold(i32),
    /// Reveal chests on the minimap within this Chebyshev radius even through fog.
    ChestMinimapRadius(i32),
    /// Multiplier on forge upgrade gold cost.
    ForgeCostMultiplier(f32),
    /// Bonus tiles added to the fog-reveal radius around the player.
    FogRevealRadiusBonus(i32),
}

/// Boon definition — static metadata + effect list. Catalog entries.
#[derive(Debug, Clone)]
pub struct Boon {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub effects: &'static [BoonEffect],
}

/// All boons available to be earned. Order is stable (the picker's
/// deterministic hash relies on this for reproducible 3-of-N
/// selection).
pub fn catalog() -> &'static [Boon] {
    CATALOG
}

const CATALOG: &[Boon] = &[
    Boon {
        id: "swift_boots",
        name: "Swift Boots",
        description: "Each step counts a little extra. +5% walking speed.",
        effects: &[BoonEffect::SpeedMultiplier(1.05)],
    },
    Boon {
        id: "trailblazer",
        name: "Trailblazer",
        description: "You move easier through wild terrain. Forest, swamp, and snow tiles cost 20% less.",
        effects: &[
            BoonEffect::BiomeCostMultiplier { biome: Biome::Forest, mult: 0.8 },
            BoonEffect::BiomeCostMultiplier { biome: Biome::DenseForest, mult: 0.8 },
            BoonEffect::BiomeCostMultiplier { biome: Biome::Swamp, mult: 0.8 },
            BoonEffect::BiomeCostMultiplier { biome: Biome::Snow, mult: 0.8 },
        ],
    },
    Boon {
        id: "roadwise",
        name: "Roadwise",
        description: "You know the highways. Roads cost an extra 25% less.",
        effects: &[BoonEffect::RoadCostMultiplier(0.75)],
    },
    Boon {
        id: "sprint",
        name: "Sprint",
        description: "Fresh legs at every start. +20% speed for the first 1 km of each session.",
        effects: &[BoonEffect::SessionStartBoost { meters: 1000.0, multiplier: 1.2 }],
    },
    Boon {
        id: "goldfinger",
        name: "Goldfinger",
        description: "Gold sticks to you. +10% gold from every source.",
        effects: &[BoonEffect::GoldMultiplier(1.10)],
    },
    Boon {
        id: "wealthy_start",
        name: "Wealthy Start",
        description: "You begin every adventure with 500 extra gold in your pouch.",
        effects: &[BoonEffect::AdventureStartGold(500)],
    },
    Boon {
        id: "treasure_sense",
        name: "Treasure Sense",
        description: "You feel chests nearby. They appear on the minimap within 10 tiles, even through fog.",
        effects: &[BoonEffect::ChestMinimapRadius(10)],
    },
    Boon {
        id: "forge_discount",
        name: "Forge Discount",
        description: "The forgemaster respects you. Upgrades cost 25% less.",
        effects: &[BoonEffect::ForgeCostMultiplier(0.75)],
    },
    Boon {
        id: "cartographer",
        name: "Cartographer",
        description: "You see further. Fog reveals 1 extra tile around you.",
        effects: &[BoonEffect::FogRevealRadiusBonus(1)],
    },
    // ── Chaos arc boss drops (adventure-scoped: only apply while the
    // player is in the chaos adventure). Each is a stronger version
    // of an existing permanent boon, on the theory that "the boss's
    // gift only echoes through the world you slew them in."
    Boon {
        id: "frostproof",
        name: "Frostproof",
        description: "The Queen's last gift. Snow tiles cost 50% less while chaos endures.",
        effects: &[BoonEffect::BiomeCostMultiplier { biome: Biome::Snow, mult: 0.5 }],
    },
    Boon {
        id: "forge_tempered",
        name: "Forge-Tempered",
        description: "The Lord's anvil sang for you. Forge upgrades cost 50% less while chaos endures.",
        effects: &[BoonEffect::ForgeCostMultiplier(0.5)],
    },
    Boon {
        id: "voidsight",
        name: "Voidsight",
        description: "You learned to see through the shroud. Fog reveals 2 extra tiles while chaos endures.",
        effects: &[BoonEffect::FogRevealRadiusBonus(2)],
    },
    Boon {
        id: "lightning_footed",
        name: "Lightning-Footed",
        description: "The Stormbinder's lesson. Roads cost 50% less while chaos endures.",
        effects: &[BoonEffect::RoadCostMultiplier(0.5)],
    },
    // Climax of the chaos arc — only awarded after all four lords fall
    // AND the Starstone Avatar is severed. Compound effect: a gold
    // bump big enough to feel like a victory lap, plus the strongest
    // fog-radius in the game so any post-victory wandering reveals
    // wide swathes of the chaos lands.
    Boon {
        id: "starstone_awakened",
        name: "Starstone Awakened",
        description: "The second cut is yours. +50% gold and fog reveals 3 extra tiles while chaos endures.",
        effects: &[
            BoonEffect::GoldMultiplier(1.5),
            BoonEffect::FogRevealRadiusBonus(3),
        ],
    },
];

pub fn lookup(id: &str) -> Option<&'static Boon> {
    catalog().iter().find(|b| b.id == id)
}

// ── Effect application helpers ──────────────────────────
//
// Each helper iterates the player's owned boons, finds the relevant
// effect variants, and folds them into a single number. Multipliers
// compose by multiplication; flat bonuses sum.

fn iter_effects<'a>(boons: &'a [String]) -> impl Iterator<Item = &'static BoonEffect> + 'a {
    boons.iter()
        .filter_map(|id| lookup(id))
        .flat_map(|b| b.effects.iter())
}

/// Aggregate global walking-speed multiplier from all owned boons.
/// Includes both `SpeedMultiplier` and the `SessionStartBoost` effect
/// while session distance is below the boost threshold.
pub fn speed_multiplier(boons: &[String], session_meters_walked: f32) -> f32 {
    let mut mult = 1.0_f32;
    for effect in iter_effects(boons) {
        match effect {
            BoonEffect::SpeedMultiplier(m) => mult *= m,
            BoonEffect::SessionStartBoost { meters, multiplier } => {
                if session_meters_walked < *meters {
                    mult *= multiplier;
                }
            }
            _ => {}
        }
    }
    mult
}

/// Aggregate biome-specific cost multiplier (per Biome). Multipliers
/// from multiple boons targeting the same biome compound.
pub fn biome_cost_multiplier(boons: &[String], biome: Biome) -> f32 {
    let mut mult = 1.0_f32;
    for effect in iter_effects(boons) {
        if let BoonEffect::BiomeCostMultiplier { biome: b, mult: m } = effect {
            if *b == biome {
                mult *= m;
            }
        }
    }
    mult
}

/// Aggregate extra multiplier on road tiles (stacks atop the built-in
/// road cost reduction in `route::tile_cost`).
pub fn road_cost_multiplier(boons: &[String]) -> f32 {
    let mut mult = 1.0_f32;
    for effect in iter_effects(boons) {
        if let BoonEffect::RoadCostMultiplier(m) = effect {
            mult *= m;
        }
    }
    mult
}

/// Aggregate gold-gain multiplier across all owned boons.
pub fn gold_multiplier(boons: &[String]) -> f32 {
    let mut mult = 1.0_f32;
    for effect in iter_effects(boons) {
        if let BoonEffect::GoldMultiplier(m) = effect {
            mult *= m;
        }
    }
    mult
}

/// Sum of one-time gold grants applied at adventure start.
pub fn adventure_start_gold(boons: &[String]) -> i32 {
    iter_effects(boons)
        .filter_map(|e| match e {
            BoonEffect::AdventureStartGold(n) => Some(*n),
            _ => None,
        })
        .sum()
}

/// Largest minimap radius for chest reveal (only the strongest boon
/// applies; doesn't compound — radii would get silly with stacking).
pub fn chest_minimap_radius(boons: &[String]) -> i32 {
    iter_effects(boons)
        .filter_map(|e| match e {
            BoonEffect::ChestMinimapRadius(r) => Some(*r),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// Aggregate forge-cost multiplier across all owned boons.
pub fn forge_cost_multiplier(boons: &[String]) -> f32 {
    let mut mult = 1.0_f32;
    for effect in iter_effects(boons) {
        if let BoonEffect::ForgeCostMultiplier(m) = effect {
            mult *= m;
        }
    }
    mult
}

/// Sum of fog-reveal radius bonuses (e.g. base 5 → 6 with Cartographer).
pub fn fog_radius_bonus(boons: &[String]) -> i32 {
    iter_effects(boons)
        .filter_map(|e| match e {
            BoonEffect::FogRevealRadiusBonus(n) => Some(*n),
            _ => None,
        })
        .sum()
}

// ── Deterministic 3-of-N selection ──────────────────────
//
// On boss completion the server picks 3 boons for the player to
// choose from. The choice is deterministic per (player_id, event_id)
// — refreshing the page returns the same 3, so reload-rolling for a
// favorable set is impossible. Already-owned boons are skipped.

/// Pick `n` boon IDs from the catalog, excluding those already owned,
/// deterministic on `seed`. Returns fewer than `n` if the catalog is
/// short on un-owned boons.
pub fn pick_choices(
    seed: u64,
    n: usize,
    owned: &[String],
) -> Vec<&'static str> {
    let mut pool: Vec<&'static str> = catalog().iter()
        .map(|b| b.id)
        .filter(|id| !owned.iter().any(|o| o == id))
        .collect();
    if pool.is_empty() {
        return Vec::new();
    }
    // Fisher-Yates shuffle driven by the seed-keyed RNG, take first n.
    let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    let mut next = || {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (state >> 33) as u32
    };
    for i in (1..pool.len()).rev() {
        let j = (next() as usize) % (i + 1);
        pool.swap(i, j);
    }
    pool.into_iter().take(n).collect()
}

/// Hash `(player_id, event_id)` into a stable seed for `pick_choices`.
/// Same inputs → same seed → same 3 boons.
pub fn choice_seed(player_id: &str, event_id: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    player_id.hash(&mut h);
    "::".hash(&mut h);
    event_id.hash(&mut h);
    h.finish()
}

// ── Tests ───────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_ids_are_unique() {
        let ids: Vec<&str> = catalog().iter().map(|b| b.id).collect();
        let mut seen = std::collections::HashSet::new();
        for id in &ids {
            assert!(seen.insert(*id), "duplicate boon id: {}", id);
        }
        // Sanity: 9 permanent boons + 4 chaos boss-drop boons +
        // 1 chaos climax boon. Bump when adding more.
        assert_eq!(ids.len(), 14, "catalog size changed; update this test");
    }

    #[test]
    fn lookup_finds_known_ids() {
        assert!(lookup("swift_boots").is_some());
        assert!(lookup("nonexistent").is_none());
    }

    #[test]
    fn speed_multiplier_compounds() {
        let owned = vec!["swift_boots".to_string()];
        let m = speed_multiplier(&owned, 0.0);
        assert!((m - 1.05).abs() < 1e-6, "single boon: {}", m);
    }

    #[test]
    fn sprint_only_applies_in_first_km() {
        let owned = vec!["sprint".to_string()];
        // First 100m of session — boost active.
        let m_early = speed_multiplier(&owned, 100.0);
        assert!((m_early - 1.2).abs() < 1e-6, "early: {}", m_early);
        // After 1.5 km — boost expired.
        let m_late = speed_multiplier(&owned, 1500.0);
        assert!((m_late - 1.0).abs() < 1e-6, "late: {}", m_late);
    }

    #[test]
    fn trailblazer_reduces_three_biomes() {
        let owned = vec!["trailblazer".to_string()];
        assert!((biome_cost_multiplier(&owned, Biome::Forest) - 0.8).abs() < 1e-6);
        assert!((biome_cost_multiplier(&owned, Biome::Swamp) - 0.8).abs() < 1e-6);
        assert!((biome_cost_multiplier(&owned, Biome::Snow) - 0.8).abs() < 1e-6);
        // Mountain not in trailblazer's set.
        assert!((biome_cost_multiplier(&owned, Biome::Mountain) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn gold_and_forge_multipliers() {
        let owned = vec!["goldfinger".to_string(), "forge_discount".to_string()];
        assert!((gold_multiplier(&owned) - 1.10).abs() < 1e-6);
        assert!((forge_cost_multiplier(&owned) - 0.75).abs() < 1e-6);
    }

    #[test]
    fn adventure_start_gold_sums() {
        let owned = vec!["wealthy_start".to_string()];
        assert_eq!(adventure_start_gold(&owned), 500);
    }

    #[test]
    fn fog_radius_bonus_sums() {
        let owned = vec!["cartographer".to_string()];
        assert_eq!(fog_radius_bonus(&owned), 1);
    }

    #[test]
    fn pick_choices_deterministic() {
        let seed = choice_seed("player-abc", "frost_lord");
        let a = pick_choices(seed, 3, &[]);
        let b = pick_choices(seed, 3, &[]);
        assert_eq!(a, b, "same seed should give same picks");
    }

    #[test]
    fn pick_choices_excludes_owned() {
        let seed = choice_seed("player-xyz", "boss_two");
        // Own all but the last 2 — picker should return only those.
        let owned: Vec<String> = catalog().iter()
            .take(catalog().len() - 2)
            .map(|b| b.id.to_string())
            .collect();
        let picks = pick_choices(seed, 3, &owned);
        // Only 2 boons un-owned, so we get 2 (not 3).
        assert_eq!(picks.len(), 2);
        for p in &picks {
            assert!(!owned.iter().any(|o| o == p), "owned id slipped in: {}", p);
        }
    }

    #[test]
    fn pick_choices_size_three_for_fresh_player() {
        let seed = choice_seed("fresh", "boss_one");
        let picks = pick_choices(seed, 3, &[]);
        assert_eq!(picks.len(), 3);
        // All distinct.
        assert!(picks[0] != picks[1] && picks[1] != picks[2] && picks[0] != picks[2]);
    }

    #[test]
    fn empty_owned_yields_no_effect() {
        let owned: Vec<String> = vec![];
        assert!((speed_multiplier(&owned, 0.0) - 1.0).abs() < 1e-6);
        assert!((gold_multiplier(&owned) - 1.0).abs() < 1e-6);
        assert!((forge_cost_multiplier(&owned) - 1.0).abs() < 1e-6);
        assert_eq!(fog_radius_bonus(&owned), 0);
        assert_eq!(adventure_start_gold(&owned), 0);
    }
}
