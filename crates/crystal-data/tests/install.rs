//! Against the 672261 install when `CRYSTAL_TEST_GAME` names its `game` folder (not in CI: the game
//! data is not ours to ship).

use std::path::PathBuf;

use crystal_data::WorldData;

fn game() -> Option<PathBuf> {
    match std::env::var("CRYSTAL_TEST_GAME") {
        Ok(p) => Some(PathBuf::from(p)),
        Err(_) => {
            eprintln!("CRYSTAL_TEST_GAME not set: skipped");
            None
        }
    }
}

#[test]
fn territory_type_and_level_sheets() {
    let Some(game) = game() else { return };
    let world = WorldData::open(&game).unwrap();
    let tt = world.pack().sheet("TerritoryType").unwrap();
    assert_eq!(tt.row(129).unwrap()[0].as_str(), Some("s1t2"));
    assert_eq!(tt.row(181).unwrap()[0].as_str(), Some("s1t1_opening"));
    // the event NPC the Limsa start point stands beside
    let level = world.pack().sheet("Level").unwrap();
    let npc = level.row(4069046).unwrap();
    assert!((npc[0].as_f32().unwrap() + 60.44).abs() < 0.01);
    assert_eq!(npc[9].as_u64(), Some(129));
}

#[test]
fn classes_start_in_their_towns() {
    let Some(game) = game() else { return };
    let world = WorldData::open(&game).unwrap();
    // gladiator, pugilist, marauder, lancer, archer, conjurer, thaumaturge, arcanist
    let towns: Vec<_> = [1, 2, 3, 4, 5, 6, 7, 26].into_iter().map(|c| world.starting_town(c)).collect();
    assert_eq!(towns, [3, 3, 1, 2, 2, 2, 3, 1].map(Some));
    assert_eq!(world.starting_town(8), None, "carpenter: not a starting class");
    // each town's opening: its POS_START (Opening's variables) is the Level row the lobby uses
    let opening = world.pack().sheet("Opening").unwrap();
    let level = world.pack().sheet("Level").unwrap();
    for town in 1..=3u8 {
        let row = opening.row(0x130000 + town as u32).unwrap();
        let at = (2..42).find(|&i| row[i].as_str() == Some("POS_START")).unwrap();
        let start = level.row(row[at + 40].as_u64().unwrap() as u32).unwrap();
        let (zone, point) = crystal_common::chara::opening_point(town).unwrap();
        assert_eq!(start[9].as_u64(), Some(zone as u64), "town {town}");
        assert!((start[0].as_f32().unwrap() as f64 - point[0]).abs() < 0.001, "town {town}");
    }
    // the range the Gridania opening's way out is (the client sent it walking into it)
    assert_eq!(world.opening_value(0x130002, "ERANGE_SEQ_1_CLOSED_1"), Some(2351918));
    assert_eq!(world.opening_value(0x130002, "NO_SUCH_NAME"), None);
}

#[test]
fn aetherytes_and_shards() {
    let Some(game) = game() else { return };
    let world = WorldData::open(&game).unwrap();
    // New Gridania's aetheryte and an aethernet shard, with their pop ranges in 132's layout
    let a = world.aetheryte(2).unwrap();
    assert!(a.main && a.territory == 132);
    let pos = a.arrival.unwrap().pos;
    assert!((pos[0] - 40.2).abs() < 0.1 && (pos[2] - 33.5).abs() < 0.1, "{pos:?}");
    let s = world.aetheryte(25).unwrap();
    assert!(!s.main && s.territory == 132);
    let pos = s.arrival.unwrap().pos;
    assert!((pos[0] - 165.2).abs() < 0.1 && (pos[2] - 83.0).abs() < 0.1, "{pos:?}");
}

#[test]
fn quest_exp_as_the_client_works_it_out() {
    let Some(game) = game() else { return };
    let world = WorldData::open(&game).unwrap();
    // Coming to Gridania, level 1, factor 50: 2 x 50 x 50 / 100 (Garland Tools lists 50)
    assert_eq!(world.quest_exp(0x10027), Some(50));
}

#[test]
fn quest_scripts_read() {
    let Some(game) = game() else { return };
    let world = WorldData::open(&game).unwrap();
    // Coming to Gridania's script, as unluac decompiles it
    let script = world.quest_script(0x10027).unwrap();
    let scenes = script.scenes();
    assert_eq!(scenes.keys().copied().collect::<Vec<_>>(), (0..=9).collect::<Vec<u16>>());
    let uses = |s: u16, what: &str| scenes[&s].uses.iter().any(|u| u == what);
    assert!(uses(0, "QuestOffer") && uses(2, "QuestAccepted") && uses(4, "PlayCutScene") && uses(5, "QuestReward"));
    assert_eq!(scenes[&0].returns, [1.0, 0.0]);
    assert_eq!(scenes[&2].returns, [1.0]);
    assert!(scenes[&6].uses.is_empty(), "6-9 are empty");
    assert!(script.functions.contains_key("IsAcceptEvent"));
}

#[test]
fn lower_decks_npcs() {
    let Some(game) = game() else { return };
    let world = WorldData::open(&game).unwrap();
    let npcs = world.npcs(129).unwrap();
    assert_eq!(npcs.len(), 45);
    assert!(npcs.iter().any(|n| n.enpc == 1003604 && (n.pos[0] + 60.44).abs() < 0.01));
}

#[test]
fn looks_gear_and_warps() {
    let Some(game) = game() else { return };
    let world = WorldData::open(&game).unwrap();
    // the wharf rat and the little ladybug: ModelChara rows, no customize or gear
    assert_eq!(world.bnpc_look(347).unwrap().model, 24);
    assert_eq!(world.bnpc_look(49).unwrap().model, 57);
    // a gladiator starts with armour model 42 (variant 1) on five slots and a weapon
    let (main, _, models) = world.starting_gear(1).unwrap();
    assert_eq!(models[..5], [0x1002a; 5]);
    assert_ne!(main, 0);
    // the Limsa lift (Warp 0x20014) goes to the Upper Decks
    let to = world.warp(0x20014).unwrap();
    assert_eq!(to.territory, 128);
    assert!((to.pos[1] - 91.5).abs() < 0.01);
    // the gladiator's starting items: the weathered shortsword, and armour found by model
    let shortsword = world.item(1601).unwrap();
    assert_eq!(
        (shortsword.name.as_str(), shortsword.stack_size, shortsword.slot_category),
        ("Weathered Shortsword", 1, 1)
    );
    // a male Hyur gladiator: the weapon, then the race's tunic, gloves, breeches, thighboots
    let items = world.starting_items(1, 1, 0);
    assert_eq!(items, [(0, 1601), (3, 2983), (4, 3520), (6, 3296), (7, 3750)]);
    let (main, _, looks) = world.equipment_look(&items);
    assert_eq!(main, shortsword.model_main);
    assert_ne!(looks[1], 0, "a body piece");
    // a female Miqo'te archer: the shortbow, the race's female set
    let archer = world.starting_items(5, 4, 1);
    assert_eq!(archer, [(0, 1889), (3, 2990), (4, 3527), (6, 3303), (7, 3757)]);
    // a Midlander gladiator at level 1: 20 × the class's modifier / 100 + the tribe's bonus
    let s = world.base_stats(1, 1, 1).unwrap();
    assert_eq!((s.strength, s.vitality, s.dexterity), (20, 19, 18));
    assert_eq!(s.hp, 104 * 110 / 100);
    let thm = world.base_stats(7, 1, 1).unwrap();
    assert!(thm.intelligence > s.intelligence, "a thaumaturge is the cleverer: {thm:?} {s:?}");
    // Fast Blade: a gladiator's level-1 weaponskill, melee, 70 TP, the 2.5 s recast, potency 150
    let fb = world.action(9).unwrap();
    assert_eq!((fb.name.as_str(), fb.category, fb.class, fb.level), ("Fast Blade", 3, 1, 1));
    assert_eq!((fb.range, fb.hostile, fb.cost, fb.recast_100ms, fb.potency), (-1, true, 70, 25, 150));
    assert_eq!(crystal_data::world::potency_in("Deals damage with a potency of 40."), 40);
    // Cure: a conjurer's spell, friendly, its 2.0 s cast and 2.5 s recast, "Cure Potency: 400"
    let cure = world.action(120).unwrap();
    assert_eq!(cure.name, "Cure");
    assert_eq!((cure.hostile, cure.cast_100ms, cure.recast_100ms, cure.potency), (false, 20, 25, 400));
    // its MP: cost type 3, 5% of the level's MP base, 2660 at 50 (2.x's measured white mage
    // MP, `(1330 + 6.58 PIE) * 120 / 100`, has 2660 = 1330 + 6.58 x 202 at its base piety)
    assert_eq!((cure.cost_type, cure.cost), (3, 5));
    assert_eq!((world.mp_base(1), world.mp_base(50)), (Some(104), Some(2660)));
    assert_eq!(cure.cost as u32 * world.mp_base(50).unwrap() / 100, 133);
    // a white mage's MP at level 50 and base piety, the 2014 line's (1596 + 7.90 x PIE)
    let whm = world.base_stats(24, 1, 50).unwrap();
    let measured = 1596.0 + 7.90 * whm.piety as f64;
    assert!((whm.mp as f64 - measured).abs() / measured < 0.02, "{} against {measured}", whm.mp);
    // layout files in the older format (f1d1's planner.lgb) are left out, not fatal
    assert!(world.layer_names(164).is_ok());
}

#[test]
fn lower_decks_exit_to_middle_la_noscea() {
    let Some(game) = game() else { return };
    let world = WorldData::open(&game).unwrap();
    // the exit the client reported walking into (up 0x190, exit 3876614)
    let exit = world.layout(129).unwrap().find(3876614).unwrap().exit_range().unwrap();
    assert_eq!((exit.territory, exit.destination, exit.return_point), (134, 2464047, 3876616));
    let arrival = world.zone_line(129, 3876614).unwrap();
    assert_eq!(arrival.territory, 134);
}
