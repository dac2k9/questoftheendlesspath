use serde::{Deserialize, Serialize};

// ── Item Categories & Effects ───────────────────────

/// Category of an item — determines behavior and UI grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemCategory {
    Consumable,
    Equipment,
    KeyItem,
}

/// Which slot equipment occupies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EquipmentSlot {
    Weapon,
    Armor,
    Accessory,
    /// Boots — speed multipliers.
    Feet,
}

/// What stat an effect modifies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatType {
    Attack,
    Defense,
    MaxHp,
}

/// An effect that an item provides — either passively (equipment) or on use (consumable).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ItemEffect {
    /// Flat stat bonus (equipment).
    StatBonus { stat: StatType, amount: i32 },
    /// Heal HP (consumable, combat only).
    Heal { amount: i32 },
    /// Reveal fog in a radius around the player.
    RevealFog { radius: usize },
    /// Passive speed multiplier while equipped. 1.0 = no change, 1.10 = +10%.
    SpeedMultiplier { multiplier: f32 },
    /// Temporary speed buff when consumed. Stacks multiplicatively with other
    /// speed sources. Expires after `duration_secs` of wall-clock time.
    BuffSpeed { multiplier: f32, duration_secs: u32 },
}

// ── Item Definition ─────────────────────────────────

/// An item definition in the catalog.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ItemDef {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub description: String,
    pub category: ItemCategory,
    #[serde(default)]
    pub stackable: bool,
    #[serde(default = "default_max_stack")]
    pub max_stack: u32,
    /// For equipment: which slot this occupies.
    #[serde(default)]
    pub slot: Option<EquipmentSlot>,
    /// Effects — stat bonuses for equipment, heal/reveal for consumables.
    #[serde(default)]
    pub effects: Vec<ItemEffect>,
}

fn default_max_stack() -> u32 {
    1
}

// ── Inventory ───────────────────────────────────────

/// A slot in the player's inventory — an item id + quantity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InventorySlot {
    pub item_id: String,
    pub quantity: u32,
}

/// A temporary buff the player gained from consuming an item. Persists on
/// the player and ticks down until expiry.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveBuff {
    /// What kind of buff this is ("speed", for now). String-typed so new
    /// buff kinds can be added without migrating saved state.
    pub kind: String,
    /// Multiplier applied while active (1.15 = +15%).
    pub multiplier: f32,
    /// Unix seconds when this buff stops applying.
    pub expires_unix: u64,
    /// Item id that granted this buff — useful for client tooltips/icons.
    #[serde(default)]
    pub source_item: String,
}

/// Catalog of all item definitions, loaded from JSON.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ItemCatalog {
    pub items: Vec<ItemDef>,
}

impl ItemCatalog {
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    pub fn get(&self, id: &str) -> Option<&ItemDef> {
        self.items.iter().find(|item| item.id == id)
    }
}

/// Add an item to an inventory, stacking if possible.
pub fn add_item(inventory: &mut Vec<InventorySlot>, item_id: &str, catalog: Option<&ItemCatalog>) -> bool {
    if let Some(slot) = inventory.iter_mut().find(|s| s.item_id == item_id) {
        let max = catalog
            .and_then(|c| c.get(item_id))
            .filter(|def| def.stackable)
            .map(|def| def.max_stack)
            .unwrap_or(1);
        if slot.quantity < max {
            slot.quantity += 1;
            return true;
        }
        return false;
    }
    inventory.push(InventorySlot {
        item_id: item_id.to_string(),
        quantity: 1,
    });
    true
}

/// Remove one unit of an item. Returns true if removed.
pub fn remove_item(inventory: &mut Vec<InventorySlot>, item_id: &str) -> bool {
    if let Some(idx) = inventory.iter().position(|s| s.item_id == item_id) {
        let slot = &mut inventory[idx];
        if slot.quantity > 1 {
            slot.quantity -= 1;
        } else {
            inventory.remove(idx);
        }
        true
    } else {
        false
    }
}

/// Check if inventory contains at least one of the given item.
pub fn has_item(inventory: &[InventorySlot], item_id: &str) -> bool {
    inventory.iter().any(|s| s.item_id == item_id && s.quantity > 0)
}

// ── Equipment ───────────────────────────────────────

/// What a player has equipped in each slot.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EquipmentLoadout {
    #[serde(default)]
    pub weapon: Option<String>,
    #[serde(default)]
    pub armor: Option<String>,
    #[serde(default)]
    pub accessory: Option<String>,
    #[serde(default)]
    pub feet: Option<String>,
}

impl EquipmentLoadout {
    /// Check if a specific item is equipped in any slot.
    pub fn has_equipped(&self, item_id: &str) -> bool {
        self.weapon.as_deref() == Some(item_id)
            || self.armor.as_deref() == Some(item_id)
            || self.accessory.as_deref() == Some(item_id)
            || self.feet.as_deref() == Some(item_id)
    }

    /// Get the item in a given slot.
    pub fn get_slot(&self, slot: EquipmentSlot) -> Option<&str> {
        match slot {
            EquipmentSlot::Weapon => self.weapon.as_deref(),
            EquipmentSlot::Armor => self.armor.as_deref(),
            EquipmentSlot::Accessory => self.accessory.as_deref(),
            EquipmentSlot::Feet => self.feet.as_deref(),
        }
    }

    fn set_slot(&mut self, slot: EquipmentSlot, item_id: Option<String>) {
        match slot {
            EquipmentSlot::Weapon => self.weapon = item_id,
            EquipmentSlot::Armor => self.armor = item_id,
            EquipmentSlot::Accessory => self.accessory = item_id,
            EquipmentSlot::Feet => self.feet = item_id,
        }
    }

    /// All slots iterable together. Useful for iterating every equipped item.
    pub fn all_slots() -> [EquipmentSlot; 4] {
        [EquipmentSlot::Weapon, EquipmentSlot::Armor, EquipmentSlot::Accessory, EquipmentSlot::Feet]
    }
}

/// Equip an item from inventory. Returns the previously equipped item_id (if any).
/// The old item goes back to inventory, the new item is removed from inventory.
pub fn equip_item(
    loadout: &mut EquipmentLoadout,
    inventory: &mut Vec<InventorySlot>,
    item_id: &str,
    catalog: &ItemCatalog,
) -> Result<Option<String>, &'static str> {
    let def = catalog.get(item_id).ok_or("item not in catalog")?;
    if def.category != ItemCategory::Equipment {
        return Err("not equipment");
    }
    let slot = def.slot.ok_or("equipment has no slot")?;

    // Must have the item in inventory
    if !has_item(inventory, item_id) {
        return Err("item not in inventory");
    }

    // Remove from inventory
    remove_item(inventory, item_id);

    // Swap with current equipment
    let old = loadout.get_slot(slot).map(|s| s.to_string());
    if let Some(ref old_id) = old {
        add_item(inventory, old_id, Some(catalog));
    }
    loadout.set_slot(slot, Some(item_id.to_string()));

    Ok(old)
}

/// Unequip an item from a slot back to inventory.
pub fn unequip_item(
    loadout: &mut EquipmentLoadout,
    inventory: &mut Vec<InventorySlot>,
    slot: EquipmentSlot,
    catalog: &ItemCatalog,
) -> bool {
    if let Some(item_id) = loadout.get_slot(slot).map(|s| s.to_string()) {
        loadout.set_slot(slot, None);
        add_item(inventory, &item_id, Some(catalog));
        true
    } else {
        false
    }
}

/// Compute total stat bonuses from all equipped items.
pub fn equipment_bonuses(loadout: &EquipmentLoadout, catalog: &ItemCatalog) -> (i32, i32, i32) {
    let mut attack = 0;
    let mut defense = 0;
    let mut max_hp = 0;
    for slot in EquipmentLoadout::all_slots() {
        if let Some(item_id) = loadout.get_slot(slot) {
            if let Some(def) = catalog.get(item_id) {
                for effect in &def.effects {
                    if let ItemEffect::StatBonus { stat, amount } = effect {
                        match stat {
                            StatType::Attack => attack += amount,
                            StatType::Defense => defense += amount,
                            StatType::MaxHp => max_hp += amount,
                        }
                    }
                }
            }
        }
    }
    (attack, defense, max_hp)
}

/// Check if player has item in inventory OR equipped.
pub fn has_item_or_equipped(inventory: &[InventorySlot], loadout: &EquipmentLoadout, item_id: &str) -> bool {
    has_item(inventory, item_id) || loadout.has_equipped(item_id)
}

/// Compute the passive speed multiplier from equipped boots (and any other
/// equipment with SpeedMultiplier effects). Returns 1.0 if nothing equipped.
pub fn equipment_speed_multiplier(loadout: &EquipmentLoadout, catalog: &ItemCatalog) -> f32 {
    let mut mult = 1.0_f32;
    for slot in EquipmentLoadout::all_slots() {
        if let Some(item_id) = loadout.get_slot(slot) {
            if let Some(def) = catalog.get(item_id) {
                for effect in &def.effects {
                    if let ItemEffect::SpeedMultiplier { multiplier } = effect {
                        mult *= multiplier;
                    }
                }
            }
        }
    }
    mult
}

// ── Tests ───────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_catalog() -> ItemCatalog {
        ItemCatalog::from_json(r#"{
            "items": [
                { "id": "health_potion", "display_name": "Health Potion", "category": "consumable", "stackable": true, "max_stack": 5, "effects": [{"type": "heal", "amount": 30}] },
                { "id": "iron_sword", "display_name": "Iron Sword", "category": "equipment", "slot": "weapon", "effects": [{"type": "stat_bonus", "stat": "attack", "amount": 5}] },
                { "id": "leather_vest", "display_name": "Leather Vest", "category": "equipment", "slot": "armor", "effects": [{"type": "stat_bonus", "stat": "defense", "amount": 2}] },
                { "id": "warm_cloak", "display_name": "Warm Cloak", "category": "equipment", "slot": "accessory", "effects": [{"type": "stat_bonus", "stat": "defense", "amount": 1}] },
                { "id": "forest_map", "display_name": "Forest Map", "category": "key_item" }
            ]
        }"#).unwrap()
    }

    #[test]
    fn catalog_lookup() {
        let cat = test_catalog();
        assert!(cat.get("health_potion").is_some());
        assert!(cat.get("nonexistent").is_none());
        assert_eq!(cat.get("iron_sword").unwrap().slot, Some(EquipmentSlot::Weapon));
    }

    #[test]
    fn add_and_stack() {
        let cat = test_catalog();
        let mut inv = vec![];
        for _ in 0..5 {
            assert!(add_item(&mut inv, "health_potion", Some(&cat)));
        }
        assert_eq!(inv[0].quantity, 5);
        assert!(!add_item(&mut inv, "health_potion", Some(&cat)));
    }

    #[test]
    fn remove_and_has() {
        let cat = test_catalog();
        let mut inv = vec![];
        add_item(&mut inv, "health_potion", Some(&cat));
        add_item(&mut inv, "health_potion", Some(&cat));
        assert!(has_item(&inv, "health_potion"));
        assert!(remove_item(&mut inv, "health_potion"));
        assert_eq!(inv[0].quantity, 1);
        assert!(remove_item(&mut inv, "health_potion"));
        assert!(inv.is_empty());
    }

    #[test]
    fn equip_and_unequip() {
        let cat = test_catalog();
        let mut inv = vec![];
        let mut loadout = EquipmentLoadout::default();

        add_item(&mut inv, "iron_sword", Some(&cat));
        let old = equip_item(&mut loadout, &mut inv, "iron_sword", &cat).unwrap();
        assert!(old.is_none());
        assert_eq!(loadout.weapon, Some("iron_sword".to_string()));
        assert!(!has_item(&inv, "iron_sword")); // removed from inventory

        // Unequip
        assert!(unequip_item(&mut loadout, &mut inv, EquipmentSlot::Weapon, &cat));
        assert!(loadout.weapon.is_none());
        assert!(has_item(&inv, "iron_sword")); // back in inventory
    }

    #[test]
    fn equip_swap() {
        let cat = test_catalog();
        let mut inv = vec![];
        let mut loadout = EquipmentLoadout::default();

        // Add two items that could conflict but they're different slots
        add_item(&mut inv, "iron_sword", Some(&cat));
        add_item(&mut inv, "leather_vest", Some(&cat));

        equip_item(&mut loadout, &mut inv, "iron_sword", &cat).unwrap();
        equip_item(&mut loadout, &mut inv, "leather_vest", &cat).unwrap();
        assert_eq!(loadout.weapon, Some("iron_sword".to_string()));
        assert_eq!(loadout.armor, Some("leather_vest".to_string()));
    }

    #[test]
    fn equipment_bonuses_sum() {
        let cat = test_catalog();
        let loadout = EquipmentLoadout {
            weapon: Some("iron_sword".to_string()),
            armor: Some("leather_vest".to_string()),
            accessory: Some("warm_cloak".to_string()),
        };
        let (atk, def, hp) = equipment_bonuses(&loadout, &cat);
        assert_eq!(atk, 5);
        assert_eq!(def, 3); // 2 + 1
        assert_eq!(hp, 0);
    }

    #[test]
    fn has_item_or_equipped_check() {
        let mut inv = vec![];
        let mut loadout = EquipmentLoadout::default();
        loadout.accessory = Some("warm_cloak".to_string());

        assert!(!has_item(&inv, "warm_cloak"));
        assert!(has_item_or_equipped(&inv, &loadout, "warm_cloak"));

        inv.push(InventorySlot { item_id: "rope".to_string(), quantity: 1 });
        assert!(has_item_or_equipped(&inv, &loadout, "rope"));
    }

    #[test]
    fn roundtrip_json() {
        let cat = test_catalog();
        let json = serde_json::to_string(&cat).unwrap();
        let parsed = ItemCatalog::from_json(&json).unwrap();
        assert_eq!(parsed.items.len(), 5);
    }
}
