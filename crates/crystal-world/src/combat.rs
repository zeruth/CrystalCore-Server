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

/// The server tick: HP comes back on it (DoTs, HoTs and statuses later).
pub const SERVER_TICK: Duration = Duration::from_secs(3);
/// How much of its maximum HP a player out of a fight gets back each server tick, percent.
pub const REGEN_PERCENT: u32 = 10;

/// How fast a battle NPC runs after its target, yalms a second.
pub const CHASE_SPEED: f32 = 6.0;
/// How far a battle NPC follows from where it stands before it gives up and goes home.
pub const LEASH: f32 = 40.0;

/// One of a battle NPC's swings at `level`.
pub fn bnpc_damage(level: u16) -> u32 {
    (3 + level as u32 * 2).max(1)
}

/// A battle NPC's HP at `level` (until BNpc parameters are read in).
pub fn bnpc_hp(level: u16) -> u32 {
    40 + level as u32 * 30
}

/// One auto-attack's damage from an attacker of `level` with `strength`.
pub fn auto_attack_damage(level: u8, strength: u32) -> u32 {
    (strength * 2 / 5 + level as u32).max(1)
}

/// The experience for defeating a battle NPC of `target_level` at `level`: more for stronger
/// ones, little for those far below (until the β's own rules are known).
pub fn kill_exp(level: u8, target_level: u16) -> u32 {
    let base = 30 + target_level as u32 * 20;
    let diff = target_level as i32 - level as i32;
    match diff {
        d if d <= -10 => 0,
        d if d < 0 => base * (10 + d) as u32 / 10,
        d => base + base * d.min(5) as u32 / 5,
    }
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

    #[test]
    fn kills_pay_by_how_strong_the_foe_was() {
        assert!(kill_exp(1, 1) > 0);
        assert!(kill_exp(1, 3) > kill_exp(1, 1));
        assert!(kill_exp(10, 5) < kill_exp(5, 5));
        assert_eq!(kill_exp(30, 5), 0, "far below: nothing");
    }
}
