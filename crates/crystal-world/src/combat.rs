//! Combat's numbers: how often auto-attacks land, how far they reach, how much they hurt, how long
//! the dead stay. Placeholders shaped like ARR's until the β's own (weapon delay and damage from
//! the Item sheet, BNpc stats) are read in: each is one function to replace.

use std::time::Duration;

/// Time between auto-attacks (a weapon's delay; ARR's swords are about 2.5-3 s).
pub const SWING: Duration = Duration::from_millis(3000);
/// How far an auto-attack reaches, yalms (ARR's melee range plus the target's size, roughly).
pub const MELEE_RANGE: f32 = 5.0;
/// A ranged class's reach: its auto-attack (Shot) and its actions whose `Action` range (col 11)
/// is -1, as Sapphire resolves -1 (`Action.cpp`: 25 for archer and bard by the action's
/// class, 3 for the rest).
pub const RANGED_RANGE: f32 = 25.0;

/// Archer (5) and bard (23): the classes that fight from range.
pub fn is_ranged_class(class: u8) -> bool {
    matches!(class, 5 | 23)
}

/// How far a class's auto-attack reaches.
pub fn auto_attack_range(class: u8) -> f32 {
    if is_ranged_class(class) { RANGED_RANGE + 1.0 } else { MELEE_RANGE }
}
/// How long a dead battle NPC's body stays.
pub const CORPSE: Duration = Duration::from_secs(5);
/// How long until a dead battle NPC is back where it stood.
pub const RESPAWN: Duration = Duration::from_secs(30);

/// The server tick: HP comes back on it (DoTs, HoTs and statuses later).
pub const SERVER_TICK: Duration = Duration::from_secs(3);
/// How much of its maximum HP a player out of a fight gets back each server tick, percent.
pub const REGEN_PERCENT: u32 = 10;
/// How much of its maximum MP a player gets back each server tick, percent, rounded down: 2.x's
/// 6 out of a fight and 2 in one (a level-50 measurement, June 2014).
pub const MP_REGEN_PERCENT: u32 = 6;
pub const MP_REGEN_FIGHTING_PERCENT: u32 = 2;
/// TP: 2.x's 1000 at most, 60 back each server tick.
pub const TP_MAX: u32 = 1000;
/// Sprint's status (the β's Status row 50) and its param (Sapphire's `ActionSprint.cpp`: 30).
pub const SPRINT_STATUS: u16 = 50;
pub const SPRINT_PARAM: u16 = 30;
pub const TP_REGEN: u32 = 60;

/// An action's MP cost: its cost (`Action` col 28, cost type 3) is that percent of the level's
/// MP base (`ParamGrow` col 5; Sapphire's `calculateMpCost`), rounded down.
pub fn mp_cost(cost: u32, mp_base: u32) -> u32 {
    cost * mp_base / 100
}

/// How fast a battle NPC runs after its target, yalms a second.
pub const CHASE_SPEED: f32 = 6.0;
/// A walk, yalms a second (the move packet's walking speed, 24 tenths: `ActorMove::SPEED_WALK`).
pub const WALK_SPEED: f32 = 2.4;
/// How long a wandering battle NPC rests between walks, seconds (the server's choice: no source
/// for 2.0's timing yet).
pub const WANDER_PAUSE_MIN: f32 = 6.0;
pub const WANDER_PAUSE_MAX: f32 = 16.0;
/// How near a player comes before an aggressive battle NPC of the same level or above attacks,
/// yalms (Sapphire's `BNpc::checkAggro`, the range it uses without the BNpc's sense data).
pub const AGGRO_RANGE: f32 = 14.0;

/// How near a player of `player_level` comes before an aggressive battle NPC of `level` attacks
/// (Sapphire's `checkAggro`): the full range against those no higher than it; against higher
/// ones `14 - 1.53^(diff * 0.6)`, and never at 10 levels or more.
pub fn aggro_range(player_level: u8, level: u16) -> f32 {
    let diff = player_level as i32 - level as i32;
    if diff <= 0 {
        AGGRO_RANGE
    } else if diff >= 10 {
        0.0
    } else {
        (AGGRO_RANGE - 1.53f32.powf(diff as f32 * 0.6)).max(0.0)
    }
}

/// The hate a battle NPC starts with for one it went for itself (Sapphire's `aggro()`: 1000).
pub const AGGRO_HATE: u32 = 1000;
/// How far a battle NPC's target gets from it before it forgets that one (Sapphire's
/// `StateCombat`: 80 yalms).
pub const DEAGGRO_RANGE: f32 = 80.0;
/// How far a battle NPC follows from its spawn point before it gives up and goes home
/// (Sapphire's `SpawnPointDistanceGtMaxDistanceCondition`: 40 yalms).
pub const LEASH: f32 = 40.0;
/// Time between a battle NPC's auto-attacks (Sapphire's `BNpc::autoAttack`: 2500 ms).
pub const MOB_SWING: Duration = Duration::from_millis(2500);

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

/// The global cooldown weaponskills and spells share (ARR's 2.5 s; an action's own recast is
/// used when it names one).
pub const GCD: Duration = Duration::from_millis(2500);

/// An action's damage: the auto-attack's scaled by its potency (an auto-attack being about 100).
pub fn action_damage(level: u8, strength: u32, potency: u32) -> u32 {
    (auto_attack_damage(level, strength) * potency / 100).max(1)
}

/// A heal's HP: the user's level and mind scaled by its potency (Cure's 400; until the β's own
/// formula is known).
pub fn heal_amount(level: u8, mind: u32, potency: u32) -> u32 {
    ((mind * 2 / 5 + level as u32) * potency / 100).max(1)
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
