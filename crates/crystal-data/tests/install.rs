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
    let items = world.starting_items(1);
    assert_eq!(items[0], (0, 1601));
    assert!(items.len() > 1, "armour too: {items:?}");
    let (main, _, looks) = world.equipment_look(&items);
    assert_eq!(main, shortsword.model_main);
    assert_ne!(looks[1], 0, "a body piece");
    // a Midlander gladiator at level 1: 20 × the class's modifier / 100 + the tribe's bonus
    let s = world.base_stats(1, 1, 1).unwrap();
    assert_eq!((s.strength, s.vitality, s.dexterity), (20, 19, 18));
    assert_eq!(s.hp, 104 * 110 / 100);
    let thm = world.base_stats(7, 1, 1).unwrap();
    assert!(thm.intelligence > s.intelligence, "a thaumaturge is the cleverer: {thm:?} {s:?}");
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
