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

/// Sapphire's level table (`CalcStats.cpp` `levelTable`, levels 0-50): MAIN, SUB, DIV, HP,
/// ELMT, THREAT.
const LEVEL_TABLE: [[u32; 6]; 51] = [
    [1, 1, 1, 1, 1, 1],
    [20, 56, 56, 86, 52, 2],
    [21, 57, 57, 101, 54, 2],
    [22, 60, 60, 109, 56, 3],
    [24, 62, 62, 116, 58, 3],
    [26, 65, 65, 123, 60, 3],
    [27, 68, 68, 131, 62, 3],
    [29, 70, 70, 138, 64, 4],
    [31, 73, 73, 145, 66, 4],
    [33, 76, 76, 153, 68, 4],
    [35, 78, 78, 160, 70, 5],
    [36, 82, 82, 174, 73, 5],
    [38, 85, 85, 188, 75, 5],
    [41, 89, 89, 202, 78, 6],
    [44, 93, 93, 216, 81, 6],
    [46, 96, 96, 230, 84, 7],
    [49, 100, 100, 244, 86, 7],
    [52, 104, 104, 258, 89, 8],
    [54, 109, 109, 272, 93, 9],
    [57, 113, 113, 286, 95, 9],
    [60, 116, 116, 300, 98, 10],
    [63, 122, 122, 333, 102, 10],
    [67, 127, 127, 366, 105, 11],
    [71, 133, 133, 399, 109, 12],
    [74, 138, 138, 432, 113, 13],
    [78, 144, 144, 465, 117, 14],
    [81, 150, 150, 498, 121, 15],
    [85, 155, 155, 531, 125, 16],
    [89, 162, 162, 564, 129, 17],
    [92, 168, 168, 597, 133, 18],
    [97, 173, 173, 630, 137, 19],
    [101, 181, 181, 669, 143, 20],
    [106, 188, 188, 708, 148, 22],
    [110, 194, 194, 747, 153, 23],
    [115, 202, 202, 786, 159, 25],
    [119, 209, 209, 825, 165, 27],
    [124, 215, 215, 864, 170, 29],
    [128, 223, 223, 903, 176, 31],
    [134, 229, 229, 942, 181, 33],
    [139, 236, 236, 981, 186, 35],
    [144, 244, 244, 1020, 192, 38],
    [150, 253, 253, 1088, 200, 40],
    [155, 263, 263, 1156, 207, 43],
    [161, 272, 272, 1224, 215, 46],
    [166, 283, 283, 1292, 223, 49],
    [171, 292, 292, 1360, 231, 52],
    [177, 302, 302, 1428, 238, 55],
    [183, 311, 311, 1496, 246, 58],
    [189, 322, 322, 1564, 254, 62],
    [196, 331, 331, 1632, 261, 66],
    [202, 341, 341, 1700, 269, 70],
];

fn level_row(level: u8) -> [u32; 6] {
    LEVEL_TABLE[(level as usize).min(LEVEL_TABLE.len() - 1)]
}

/// The level's main attribute base (the table's MAIN: 20 at level 1, 202 at 50): an attribute
/// with a 100% class modifier and no bonus.
pub fn main_base(level: u8) -> u32 {
    level_row(level)[0]
}

/// Sapphire's `AUTO_ATTACK_POTENCY` (110) and `RANGED_AUTO_ATTACK_POTENCY` (100).
pub const AUTO_ATTACK_POTENCY: u32 = 110;
pub const RANGED_AUTO_ATTACK_POTENCY: u32 = 100;

/// The attribute (index into [`crate::PlayerLook::attributes`]: STR, DEX, VIT, INT, MND, PIE)
/// a class fights with (Sapphire's `getPrimaryStat`): dexterity for archers, intelligence for
/// thaumaturges and arcanists, mind for conjurers, strength for the rest.
pub fn primary_attribute(class: u8) -> usize {
    match class {
        5 | 23 => 1,
        7 | 25 | 26 | 27 | 28 => 3,
        6 | 24 => 4,
        _ => 0,
    }
}

/// Whether a class's actions hit with magic (its weapon's magic damage, its intelligence).
fn casts(class: u8) -> bool {
    matches!(primary_attribute(class), 3 | 4)
}

/// Sapphire's `calcAttackPower` (levels to 50): `trunc2(75 (power - MAIN) / MAIN) + 100`.
fn attack_power(level: u8, power: u32) -> f32 {
    let main = level_row(level)[0] as f32;
    (75.0 * (power as f32 - main) / main * 100.0).trunc() / 100.0 + 100.0
}

/// Sapphire's `weaponDamage`: `floor(MAIN × job modifier / 1000) + weapon damage`.
fn weapon_damage(level: u8, job_mod: u32, weapon: u32) -> f32 {
    (level_row(level)[0] as f32 * job_mod as f32 / 1000.0).floor() + weapon as f32
}

/// The weapon damage a player's hits use: physical, or magic for casters.
fn weapon_of(look: &crate::PlayerLook) -> (u32, usize) {
    let stat = primary_attribute(look.class);
    let wd = if casts(look.class) { look.weapon.magic } else { look.weapon.physical };
    (wd, stat)
}

/// One auto-attack's damage before the crit and variance rolls (Sapphire's
/// `calcAutoAttackDamage`; determination and speed at their neutral 1000, as Sapphire's players
/// have them without gear stats): `⌊⌊⌊pot × ap × det⌋/1000 × spd⌋/1000 × aa⌋/100`, `aa` the
/// weapon damage scaled by delay / 3 s.
pub fn auto_attack_damage(look: &crate::PlayerLook) -> u32 {
    let stat = primary_attribute(look.class);
    let pot = if is_ranged_class(look.class) { RANGED_AUTO_ATTACK_POTENCY } else { AUTO_ATTACK_POTENCY } as f32 / 100.0;
    let ap = attack_power(look.level, look.attributes[stat]);
    let delay = if look.weapon.delay_ms > 0 { look.weapon.delay_ms as f32 / 1000.0 } else { 2.0 };
    let aa = (weapon_damage(look.level, look.job_mods[stat], look.weapon.physical) * (delay / 3.0)).floor();
    let (det, spd) = (1000.0, 1000.0);
    let factor = (pot * ap * det).floor() / 1000.0;
    let factor = ((((factor.floor() * spd).floor() / 1000.0).floor() * aa).floor() / 100.0).floor();
    factor.max(1.0) as u32
}

/// How long after an action its combo can follow (Sapphire's `MAX_COMBO_LENGTH`: 12.5 s).
pub const COMBO_WINDOW: Duration = Duration::from_millis(12_500);

/// The global cooldown weaponskills and spells share (ARR's 2.5 s; an action's own recast is
/// used when it names one).
pub const GCD: Duration = Duration::from_millis(2500);

/// An action's damage before the crit and variance rolls (Sapphire's `calcActionDamage`):
/// `⌊⌊pot × ap × det⌋/1000 × wd⌋/100`, with the class's fighting attribute and weapon.
pub fn action_damage(look: &crate::PlayerLook, potency: u32) -> u32 {
    let (wd, stat) = weapon_of(look);
    let ap = attack_power(look.level, look.attributes[stat]);
    let det = 1000.0;
    let factor = ((potency as f32 / 100.0) * ap * det).floor() / 1000.0;
    let factor = (factor.floor() * weapon_damage(look.level, look.job_mods[stat], wd)).floor() / 100.0;
    factor.floor().max(1.0) as u32
}

/// A heal's HP before the crit and variance rolls (Sapphire's `calcActionHealing`):
/// `⌊(wd × MND/200 + DET/10) × ⌊potency/100⌋ × 1.3⌋`, DET at the level's MAIN.
pub fn heal_amount(look: &crate::PlayerLook, potency: u32) -> u32 {
    let det = level_row(look.level)[0] as f32;
    let mind = look.attributes[4] as f32;
    let wd = look.weapon.magic as f32;
    ((wd * (mind / 200.0) + det / 10.0) * (potency / 100) as f32 * 1.3).floor().max(1.0) as u32
}

/// Sapphire's critical hit chance without gear (critical hit at the level's SUB): 5%, and its
/// damage ×1.4.
pub const CRIT_PERCENT: f32 = 5.0;
pub const CRIT_BONUS: f32 = 1.4;

/// A hit's roll: critical when `crit_roll` (0..100) is under [`CRIT_PERCENT`], then ±5% by
/// `variance_roll` (0..100: `1 + (roll - 50) / 1000`), as Sapphire's damage and healing have it.
/// The amount and whether it was critical.
pub fn roll(base: u32, crit_roll: f32, variance_roll: f32) -> (u32, bool) {
    let crit = CRIT_PERCENT > crit_roll;
    let mut amount = base as f32;
    if crit {
        amount *= CRIT_BONUS;
    }
    amount *= 1.0 + (variance_roll - 50.0) / 1000.0;
    (amount.floor().max(1.0) as u32, crit)
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

    fn gladiator(level: u8, strength: u32, weapon: u32) -> crate::PlayerLook {
        crate::PlayerLook {
            class: 1,
            level,
            attributes: [strength, 20, 20, 20, 20, 20],
            job_mods: [100; 6],
            weapon: crate::Weapon { physical: weapon, magic: 0, delay_ms: 3000 },
            ..Default::default()
        }
    }

    #[test]
    fn auto_attacks_follow_sapphires_formula() {
        // level 1, strength at MAIN (20): ap 100; wd ⌊20 × 100 / 1000⌋ + 8 = 10; aa 10 (3 s);
        // ⌊⌊1.1 × 100 × 1000⌋ / 1000 × 1000 / 1000 × 10⌋ / 100 = 11
        assert_eq!(auto_attack_damage(&gladiator(1, 20, 8)), 11);
        // more strength, a better weapon, a higher level: more
        assert!(auto_attack_damage(&gladiator(1, 30, 8)) > 11);
        assert!(auto_attack_damage(&gladiator(1, 20, 20)) > 11);
        assert!(auto_attack_damage(&gladiator(50, 300, 40)) > auto_attack_damage(&gladiator(1, 20, 8)));
        // a level-1 rat in a few swings
        let hits = bnpc_hp(1).div_ceil(11);
        assert!((4..=10).contains(&hits), "{hits} swings");
    }

    #[test]
    fn actions_scale_by_potency_and_heals_by_mind() {
        let g = gladiator(1, 20, 8);
        // ⌊⌊1.5 × 100 × 1000⌋/1000 × 10⌋/100 = 15
        assert_eq!(action_damage(&g, 150), 15);
        assert!(action_damage(&g, 300) > action_damage(&g, 150));
        let conjurer = crate::PlayerLook {
            class: 6,
            level: 1,
            attributes: [20, 20, 20, 20, 30, 20],
            weapon: crate::Weapon { physical: 5, magic: 8, delay_ms: 3200 },
            ..Default::default()
        };
        // ⌊(8 × 30/200 + 20/10) × 4 × 1.3⌋ = ⌊(1.2 + 2) × 5.2⌋ = 16
        assert_eq!(heal_amount(&conjurer, 400), 16);
    }

    #[test]
    fn rolls_crit_at_five_percent_and_vary_five() {
        assert_eq!(roll(100, 50.0, 50.0), (100, false));
        assert_eq!(roll(100, 4.0, 50.0), (140, true));
        assert_eq!(roll(100, 50.0, 0.0), (95, false));
        // 100 × 1.05f is 104.99..., floored as Sapphire's float is
        assert_eq!(roll(100, 50.0, 100.0), (104, false));
    }

    #[test]
    fn kills_pay_by_how_strong_the_foe_was() {
        assert!(kill_exp(1, 1) > 0);
        assert!(kill_exp(1, 3) > kill_exp(1, 1));
        assert!(kill_exp(10, 5) < kill_exp(5, 5));
        assert_eq!(kill_exp(30, 5), 0, "far below: nothing");
    }
}
