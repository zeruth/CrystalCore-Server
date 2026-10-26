//! A character's items: containers of slots (the client's container ids), stacking, moving,
//! currency. Plain data: the zone service loads it from the characters database, changes it
//! through these methods and sends the client what changed.

use std::collections::BTreeMap;

/// Container ids, as the client numbers them (Sapphire's `InventoryType`, the 2.x-era values).
pub mod container {
    pub const BAG0: u16 = 0;
    pub const BAG3: u16 = 3;
    /// What the character wears (slots are `slot::*`).
    pub const EQUIPPED: u16 = 1000;
    /// Gil (slot 0) and the other currencies.
    pub const CURRENCY: u16 = 2000;
    pub const CRYSTAL: u16 = 2001;
    pub const KEY_ITEM: u16 = 2004;
    pub const ARMORY_OFF: u16 = 3200;
    pub const ARMORY_HEAD: u16 = 3201;
    pub const ARMORY_BODY: u16 = 3202;
    pub const ARMORY_HANDS: u16 = 3203;
    pub const ARMORY_WAIST: u16 = 3204;
    pub const ARMORY_LEGS: u16 = 3205;
    pub const ARMORY_FEET: u16 = 3206;
    pub const ARMORY_NECK: u16 = 3207;
    pub const ARMORY_EARS: u16 = 3208;
    pub const ARMORY_WRIST: u16 = 3209;
    pub const ARMORY_RINGS: u16 = 3300;
    pub const ARMORY_SOUL: u16 = 3400;
    pub const ARMORY_MAIN: u16 = 3500;

    /// The bags, in the order items go into them.
    pub const BAGS: [u16; 4] = [0, 1, 2, 3];

    /// The containers a character has, with their slot counts.
    pub const ALL: [(u16, u16); 21] = [
        (0, 25),
        (1, 25),
        (2, 25),
        (3, 25),
        (EQUIPPED, 14),
        (CURRENCY, 11),
        (CRYSTAL, 18),
        (KEY_ITEM, 35),
        (ARMORY_OFF, 25),
        (ARMORY_HEAD, 25),
        (ARMORY_BODY, 25),
        (ARMORY_HANDS, 25),
        (ARMORY_WAIST, 25),
        (ARMORY_LEGS, 25),
        (ARMORY_FEET, 25),
        (ARMORY_NECK, 25),
        (ARMORY_EARS, 25),
        (ARMORY_WRIST, 25),
        (ARMORY_RINGS, 50),
        (ARMORY_SOUL, 25),
        (ARMORY_MAIN, 25),
    ];

    pub fn capacity(container: u16) -> u16 {
        ALL.iter().find(|(c, _)| *c == container).map_or(0, |(_, n)| *n)
    }
}

/// [`container::EQUIPPED`]'s slots (Sapphire's `GearSetSlot`).
pub mod slot {
    pub const MAIN_HAND: u16 = 0;
    pub const OFF_HAND: u16 = 1;
    pub const HEAD: u16 = 2;
    pub const BODY: u16 = 3;
    pub const HANDS: u16 = 4;
    pub const WAIST: u16 = 5;
    pub const LEGS: u16 = 6;
    pub const FEET: u16 = 7;
    pub const EARS: u16 = 8;
    pub const NECK: u16 = 9;
    pub const WRIST: u16 = 10;
    pub const RING1: u16 = 11;
    pub const RING2: u16 = 12;
    pub const SOUL_CRYSTAL: u16 = 13;
}

/// Gil's item id (in [`container::CURRENCY`] slot 0).
pub const GIL: u32 = 1;
/// The most gil a character holds.
pub const MAX_GIL: u32 = 999_999_999;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Item {
    /// `Item` row.
    pub item_id: u32,
    pub quantity: u32,
    pub hq: bool,
    pub condition: u16,
    pub spiritbond: u16,
    pub stain: u8,
}

impl Item {
    pub fn new(item_id: u32, quantity: u32) -> Self {
        Self { item_id, quantity, condition: 30000, ..Default::default() }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryError {
    /// No room left for what was added.
    Full,
    /// The slot is not in a container the character has.
    NoSuchSlot,
    /// The source slot is empty.
    Empty,
    /// Not that much gil (or too much).
    Gil,
}

/// Every container's slots that hold something.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Inventory {
    slots: BTreeMap<(u16, u16), Item>,
}

impl Inventory {
    pub fn get(&self, container: u16, slot: u16) -> Option<&Item> {
        self.slots.get(&(container, slot))
    }

    /// Every filled slot of `container`, by slot.
    pub fn container(&self, container: u16) -> impl Iterator<Item = (u16, &Item)> {
        self.slots.range((container, 0)..=(container, u16::MAX)).map(|((_, s), i)| (*s, i))
    }

    /// Every filled slot: (container, slot, item).
    pub fn all(&self) -> impl Iterator<Item = (u16, u16, &Item)> {
        self.slots.iter().map(|((c, s), i)| (*c, *s, i))
    }

    /// Puts `item` in a slot (replacing what was there).
    pub fn set(&mut self, container: u16, slot: u16, item: Item) -> Result<(), InventoryError> {
        if slot >= container::capacity(container) {
            return Err(InventoryError::NoSuchSlot);
        }
        if item.quantity == 0 {
            self.slots.remove(&(container, slot));
        } else {
            self.slots.insert((container, slot), item);
        }
        Ok(())
    }

    pub fn take(&mut self, container: u16, slot: u16) -> Option<Item> {
        self.slots.remove(&(container, slot))
    }

    pub fn first_free(&self, container: u16) -> Option<u16> {
        (0..container::capacity(container)).find(|s| !self.slots.contains_key(&(container, *s)))
    }

    /// Adds `quantity` of `item_id` to the bags: onto stacks of it first (up to `stack_size`, the
    /// Item row's), then into free slots. Nothing is added unless all of it fits. The slots that
    /// changed.
    pub fn add_to_bags(
        &mut self,
        item_id: u32,
        quantity: u32,
        hq: bool,
        stack_size: u32,
    ) -> Result<Vec<(u16, u16)>, InventoryError> {
        let stack = stack_size.max(1);
        let mut plan: Vec<((u16, u16), u32)> = Vec::new();
        let mut left = quantity;
        for &bag in &container::BAGS {
            for (s, it) in self.container(bag) {
                if left == 0 {
                    break;
                }
                if it.item_id == item_id && it.hq == hq && it.quantity < stack {
                    let n = (stack - it.quantity).min(left);
                    plan.push(((bag, s), it.quantity + n));
                    left -= n;
                }
            }
        }
        let mut taken: Vec<(u16, u16)> = Vec::new();
        'bags: for &bag in &container::BAGS {
            for s in 0..container::capacity(bag) {
                if left == 0 {
                    break 'bags;
                }
                if !self.slots.contains_key(&(bag, s)) && !taken.contains(&(bag, s)) {
                    let n = stack.min(left);
                    plan.push(((bag, s), n));
                    taken.push((bag, s));
                    left -= n;
                }
            }
        }
        if left > 0 {
            return Err(InventoryError::Full);
        }
        let mut changed = Vec::new();
        for ((c, s), q) in plan {
            let base = self.slots.get(&(c, s)).copied().unwrap_or(Item { hq, ..Item::new(item_id, 0) });
            self.slots.insert((c, s), Item { quantity: q, ..base });
            changed.push((c, s));
        }
        Ok(changed)
    }

    /// Moves what is in one slot to another, swapping with what is there.
    pub fn swap(&mut self, from: (u16, u16), to: (u16, u16)) -> Result<(), InventoryError> {
        if from.1 >= container::capacity(from.0) || to.1 >= container::capacity(to.0) {
            return Err(InventoryError::NoSuchSlot);
        }
        let a = self.slots.remove(&from).ok_or(InventoryError::Empty)?;
        if let Some(b) = self.slots.remove(&to) {
            self.slots.insert(from, b);
        }
        self.slots.insert(to, a);
        Ok(())
    }

    /// What dragging one slot onto another does: into an empty slot it moves; onto the same item
    /// (and quality) it merges up to `stack_size`, the rest staying behind; onto anything else it
    /// swaps. The slots that changed.
    pub fn move_item(
        &mut self,
        from: (u16, u16),
        to: (u16, u16),
        stack_size: u32,
    ) -> Result<Vec<(u16, u16)>, InventoryError> {
        if from.1 >= container::capacity(from.0) || to.1 >= container::capacity(to.0) {
            return Err(InventoryError::NoSuchSlot);
        }
        if from == to {
            return Ok(vec![]);
        }
        let src = *self.slots.get(&from).ok_or(InventoryError::Empty)?;
        if let Some(dst) = self.slots.get(&to).copied()
            && dst.item_id == src.item_id
            && dst.hq == src.hq
            && stack_size > 1
        {
            let moved = (stack_size.saturating_sub(dst.quantity)).min(src.quantity);
            self.slots.insert(to, Item { quantity: dst.quantity + moved, ..dst });
            if moved == src.quantity {
                self.slots.remove(&from);
            } else {
                self.slots.insert(from, Item { quantity: src.quantity - moved, ..src });
            }
        } else {
            self.swap(from, to)?;
        }
        Ok(vec![from, to])
    }

    pub fn gil(&self) -> u32 {
        self.get(container::CURRENCY, 0).map_or(0, |i| i.quantity)
    }

    /// Adds (or with a negative `delta`, spends) gil; the new amount.
    pub fn add_gil(&mut self, delta: i64) -> Result<u32, InventoryError> {
        let now = self.gil() as i64 + delta;
        if !(0..=MAX_GIL as i64).contains(&now) {
            return Err(InventoryError::Gil);
        }
        self.slots.insert((container::CURRENCY, 0), Item { quantity: now as u32, ..Item::new(GIL, 0) });
        Ok(now as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stacks_then_fills_free_slots() {
        let mut inv = Inventory::default();
        assert_eq!(inv.add_to_bags(5056, 90, false, 99).unwrap(), vec![(0, 0)]);
        // 9 fill the stack, the other 11 start a new one
        assert_eq!(inv.add_to_bags(5056, 20, false, 99).unwrap(), vec![(0, 0), (0, 1)]);
        assert_eq!(inv.get(0, 0).unwrap().quantity, 99);
        assert_eq!(inv.get(0, 1).unwrap().quantity, 11);
        // HQ does not stack with NQ
        assert_eq!(inv.add_to_bags(5056, 1, true, 99).unwrap(), vec![(0, 2)]);
        // unstackable items take a slot each
        assert_eq!(inv.add_to_bags(1601, 2, false, 1).unwrap(), vec![(0, 3), (0, 4)]);
    }

    #[test]
    fn nothing_is_added_when_it_does_not_all_fit() {
        let mut inv = Inventory::default();
        let room: u32 = container::BAGS.iter().map(|b| container::capacity(*b) as u32).sum();
        inv.add_to_bags(1601, room - 1, false, 1).unwrap();
        assert_eq!(inv.add_to_bags(1601, 2, false, 1), Err(InventoryError::Full));
        assert_eq!(inv.all().count() as u32, room - 1);
        assert!(inv.add_to_bags(1601, 1, false, 1).is_ok());
        assert_eq!(inv.first_free(container::BAG3), None);
    }

    #[test]
    fn swaps_and_equips() {
        let mut inv = Inventory::default();
        inv.add_to_bags(1601, 1, false, 1).unwrap();
        inv.swap((0, 0), (container::EQUIPPED, slot::MAIN_HAND)).unwrap();
        assert_eq!(inv.get(container::EQUIPPED, slot::MAIN_HAND).unwrap().item_id, 1601);
        assert!(inv.get(0, 0).is_none());
        assert_eq!(inv.swap((0, 0), (0, 1)), Err(InventoryError::Empty));
        assert_eq!(inv.swap((0, 99), (0, 1)), Err(InventoryError::NoSuchSlot));
        assert_eq!(inv.set(container::EQUIPPED, 14, Item::new(1, 1)), Err(InventoryError::NoSuchSlot));
    }

    #[test]
    fn moving_moves_merges_or_swaps() {
        let mut inv = Inventory::default();
        inv.set(0, 0, Item::new(5056, 60)).unwrap();
        inv.set(0, 1, Item::new(5056, 50)).unwrap();
        inv.set(0, 2, Item::new(1601, 1)).unwrap();
        // merge: 49 fit on top of 50, 11 stay behind
        assert_eq!(inv.move_item((0, 0), (0, 1), 99).unwrap(), vec![(0, 0), (0, 1)]);
        assert_eq!((inv.get(0, 0).unwrap().quantity, inv.get(0, 1).unwrap().quantity), (11, 99));
        // swap with something else
        inv.move_item((0, 0), (0, 2), 99).unwrap();
        assert_eq!((inv.get(0, 0).unwrap().item_id, inv.get(0, 2).unwrap().item_id), (1601, 5056));
        // into an empty slot
        inv.move_item((0, 0), (container::EQUIPPED, slot::MAIN_HAND), 1).unwrap();
        assert!(inv.get(0, 0).is_none());
        // a whole stack merges away
        inv.move_item((0, 2), (0, 1), 999).unwrap();
        assert!(inv.get(0, 2).is_none());
        assert_eq!(inv.get(0, 1).unwrap().quantity, 110);
        assert_eq!(inv.move_item((0, 5), (0, 6), 1), Err(InventoryError::Empty));
    }

    #[test]
    fn gil_has_bounds() {
        let mut inv = Inventory::default();
        assert_eq!(inv.add_gil(100).unwrap(), 100);
        assert_eq!(inv.add_gil(-40).unwrap(), 60);
        assert_eq!(inv.add_gil(-61), Err(InventoryError::Gil));
        assert_eq!(inv.add_gil(MAX_GIL as i64), Err(InventoryError::Gil));
        assert_eq!(inv.gil(), 60);
        assert_eq!(inv.get(container::CURRENCY, 0).unwrap().item_id, GIL);
    }
}
