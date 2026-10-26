//! Combat's numbers: how often auto-attacks land, how far they reach, how much they hurt, how long
//! the dead stay. Placeholders shaped like ARR's until the β's own (weapon delay and damage from
//! the Item sheet, BNpc stats) are read in: each is one function to replace.

use std::time::Duration;

/// Time between auto-attacks (a weapon's delay; ARR's swords are about 2.5-3 s).
pub const SWING: Duration = Duration::from_millis(3000);
/// How far an auto-attack reaches, yalms (ARR's melee range plus the target's size, roughly).
pub const MELEE_RANGE: f32 = 5.0;
/// How long a dead battle NPC's body stays.
pub const CORPSE: Duration = Duration::from_secs(5);
/// How long until a dead battle NPC is back where it stood.
pub const RESPAWN: Duration = Duration::from_secs(30);

/// A battle NPC's HP at `level` (until BNpc parameters are read in).
pub fn bnpc_hp(level: u16) -> u32 {
    40 + level as u32 * 30
}

/// One auto-attack's damage from an attacker of `level` with `strength`.
pub fn auto_attack_damage(level: u8, strength: u32) -> u32 {
    (strength * 2 / 5 + level as u32).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_level_1_gladiator_kills_a_level_1_rat_in_a_few_swings() {
        let hits = bnpc_hp(1).div_ceil(auto_attack_damage(1, 20));
        assert!((4..=10).contains(&hits), "{hits} swings");
        assert!(auto_attack_damage(50, 300) > auto_attack_damage(1, 20));
        assert_eq!(auto_attack_damage(1, 0), 1);
    }
}
