use serde::{Deserialize, Serialize};

/// Category of an item — determines behavior and UI grouping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemCategory {
    /// Can be used/consumed (potions, food, scrolls).
    Consumable,
    /// Wearable gear (cloaks, boots, charms).
    Equipment,
    /// Story/quest items that cannot be dropped or consumed.
    KeyItem,
}

/// An item definition in the catalog. Describes what the item IS.
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
}

fn default_max_stack() -> u32 {
    1
}

/// A slot in the player's inventory — an item id + quantity.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct InventorySlot {
    pub item_id: String,
    pub quantity: u32,
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
/// Returns true if the item was added, false if inventory is full or stack is maxed.
pub fn add_item(inventory: &mut Vec<InventorySlot>, item_id: &str, catalog: Option<&ItemCatalog>) -> bool {
    // Check if item already exists in inventory
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
        // Stack full — for non-stackable items (max=1), this means already owned
        return false;
    }
    // New item
    inventory.push(InventorySlot {
        item_id: item_id.to_string(),
        quantity: 1,
    });
    true
}

/// Remove one unit of an item. Returns true if removed, false if not found.
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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_catalog() -> ItemCatalog {
        ItemCatalog::from_json(r#"{
            "items": [
                { "id": "health_potion", "display_name": "Health Potion", "description": "Heals you.", "category": "consumable", "stackable": true, "max_stack": 5 },
                { "id": "warm_cloak", "display_name": "Warm Cloak", "description": "Warm.", "category": "equipment" },
                { "id": "forest_map", "display_name": "Forest Map", "description": "A map.", "category": "key_item" }
            ]
        }"#).unwrap()
    }

    #[test]
    fn catalog_lookup() {
        let cat = test_catalog();
        assert!(cat.get("health_potion").is_some());
        assert!(cat.get("nonexistent").is_none());
        assert_eq!(cat.get("warm_cloak").unwrap().category, ItemCategory::Equipment);
    }

    #[test]
    fn add_new_item() {
        let cat = test_catalog();
        let mut inv = vec![];
        assert!(add_item(&mut inv, "health_potion", Some(&cat)));
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].quantity, 1);
    }

    #[test]
    fn stack_consumable() {
        let cat = test_catalog();
        let mut inv = vec![];
        for _ in 0..5 {
            assert!(add_item(&mut inv, "health_potion", Some(&cat)));
        }
        assert_eq!(inv.len(), 1);
        assert_eq!(inv[0].quantity, 5);
        // Max stack reached
        assert!(!add_item(&mut inv, "health_potion", Some(&cat)));
    }

    #[test]
    fn no_stack_equipment() {
        let cat = test_catalog();
        let mut inv = vec![];
        assert!(add_item(&mut inv, "warm_cloak", Some(&cat)));
        // Already have one, not stackable
        assert!(!add_item(&mut inv, "warm_cloak", Some(&cat)));
        assert_eq!(inv[0].quantity, 1);
    }

    #[test]
    fn remove_and_has() {
        let cat = test_catalog();
        let mut inv = vec![];
        add_item(&mut inv, "health_potion", Some(&cat));
        add_item(&mut inv, "health_potion", Some(&cat));
        assert!(has_item(&inv, "health_potion"));
        assert!(!has_item(&inv, "warm_cloak"));

        assert!(remove_item(&mut inv, "health_potion"));
        assert_eq!(inv[0].quantity, 1);
        assert!(remove_item(&mut inv, "health_potion"));
        assert!(inv.is_empty());
        assert!(!remove_item(&mut inv, "health_potion"));
    }

    #[test]
    fn add_without_catalog() {
        let mut inv = vec![];
        assert!(add_item(&mut inv, "mystery_thing", None));
        assert_eq!(inv[0].quantity, 1);
        // Without catalog, max_stack defaults to 1 (non-stackable)
        assert!(!add_item(&mut inv, "mystery_thing", None));
    }

    #[test]
    fn roundtrip_json() {
        let cat = test_catalog();
        let json = serde_json::to_string(&cat).unwrap();
        let parsed = ItemCatalog::from_json(&json).unwrap();
        assert_eq!(parsed.items.len(), 3);
    }
}
