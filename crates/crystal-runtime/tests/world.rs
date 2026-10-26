//! Two headless clients in one territory of a whole server (SQLite, in-process state, no game
//! data): they see each other, see each other move and talk, and see each other go. GM commands
//! work for a GM account only, and only in GM mode.

use std::time::Duration;

use crystal_bot::{Bot, Down};
use crystal_common::config::DatabaseEndpoint;
use crystal_common::{Config, Role};
use crystal_db::{InventoryRow, Position};
use crystal_protocol::zone::chat;
use crystal_runtime::Server;

const T: Duration = Duration::from_secs(10);

async fn character(server: &Server, account: i64, name: &str, content_id: u64, pos: [f64; 3]) -> u32 {
    let db = &server.db.characters;
    let id = db.create_character(account, content_id, 1, name, "", 0).await.unwrap();
    db.save_position(content_id, Position { zone_id: 129, x: pos[0], y: pos[1], z: pos[2], rotation: 0.0 })
        .await
        .unwrap();
    0x1000_0000 + id as u32
}

/// The next chat line from the server.
async fn line(bot: &mut Bot) -> String {
    let actor = bot.actor;
    bot.wait_for(T, |d| if let Down::Chat(c) = d { Some(c.text.clone()) } else { None })
        .await
        .unwrap_or_else(|e| panic!("no chat line for {actor:#x}: {e}"))
}

#[tokio::test]
async fn players_see_each_other() {
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name).to_string_lossy().replace('\\', "/");
    let mut config = Config { roles: vec![Role::Zone], ..Config::default() };
    config.database.login = DatabaseEndpoint::sqlite(&path("var/login.db"));
    config.database.characters = DatabaseEndpoint::sqlite(&path("var/characters.db"));
    config.database.world = DatabaseEndpoint::sqlite(&path("var/world.db"));
    config.zone.bind = "127.0.0.1:0".parse().unwrap();
    let server = Server::start(config).await.unwrap();
    let addr = server.local_addr(Role::Zone).unwrap();

    let gm_account = server.db.login.ensure_account("gm", 0).await.unwrap();
    let player_account = server.db.login.ensure_account("player", 0).await.unwrap();
    let a = character(&server, gm_account, "Alpha Tester", 101, [0.0, 0.0, 0.0]).await;
    let b = character(&server, player_account, "Beta Tester", 102, [5.0, 0.0, 0.0]).await;

    // Alpha has a weapon on and some gil
    let weapon = InventoryRow {
        container: 1000,
        slot: 0,
        item_id: 1601,
        quantity: 1,
        hq: false,
        condition: 30000,
        spiritbond: 0,
        stain: 0,
    };
    let gil = InventoryRow { container: 2000, slot: 0, item_id: 1, quantity: 1234, ..weapon };
    server.db.characters.save_inventory((a - 0x1000_0000) as i64, &[weapon, gil]).await.unwrap();

    let mut alpha = Bot::connect(addr, a).await.unwrap();
    alpha.enter().await.unwrap();
    assert_eq!(alpha.territory, 129);
    // its items came before InitZone, a batch per container
    let got: Vec<(u16, u16, u32, u32)> =
        alpha.items.iter().map(|i| (i.container, i.slot, i.item_id, i.quantity)).collect();
    assert_eq!(got, [(1000, 0, 1601, 1), (2000, 0, 1, 1234)]);
    assert_eq!(alpha.containers.len(), 2);
    assert!(alpha.containers.iter().all(|(seq, n, _)| *n == 1 && alpha.items.iter().any(|i| i.sequence == *seq)));

    // Alpha takes its weapon off into its first bag: answered, both slots sent as they are now, saved
    alpha.move_item(0x1000_0001, (1000, 0), (0, 0)).await.unwrap();
    let done = alpha
        .wait_for(T, |d| if let Down::OperationDone { context, error } = d { Some((*context, *error)) } else { None })
        .await
        .unwrap();
    assert_eq!(done, (0x1000_0001, 0));
    let mut updates = Vec::new();
    while updates.len() < 2 {
        let i = alpha.wait_for(T, |d| if let Down::Item(i) = d { Some(*i) } else { None }).await.unwrap();
        updates.push((i.container, i.slot, i.item_id));
    }
    assert_eq!(updates, [(1000, 0, 0), (0, 0, 1601)]);
    let saved = server.db.characters.inventory((a - 0x1000_0000) as i64).await.unwrap();
    assert!(saved.iter().any(|r| (r.container, r.slot, r.item_id) == (0, 0, 1601)));
    assert!(!saved.iter().any(|r| r.container == 1000));
    let mut beta = Bot::connect(addr, b).await.unwrap();
    beta.enter().await.unwrap();

    // each spawns for the other, as a player with its name
    let seen = alpha
        .wait_for(T, |d| match d {
            Down::Spawn { actor, spawn } if *actor == b => Some(spawn.clone()),
            _ => None,
        })
        .await
        .unwrap();
    assert_eq!((seen.name.as_str(), seen.kind, seen.index), ("Beta Tester", 1, 1));
    beta.wait_for(T, |d| matches!(d, Down::Spawn { actor, .. } if *actor == a).then_some(())).await.unwrap();

    // Alpha walks; Beta sees it
    alpha.move_to([2.0, 0.0, 1.0], 0.5).await.unwrap();
    let moved = beta
        .wait_for(T, |d| if let Down::Move { actor, update } = d { (*actor == a).then_some(*update) } else { None })
        .await
        .unwrap();
    assert!((moved.pos[0] - 2.0).abs() < 0.05 && (moved.pos[2] - 1.0).abs() < 0.05);

    // Alpha puts the sword back on: Beta sees its gear change (sent as Alpha), and so does Alpha
    alpha.move_item(0x1000_0002, (0, 0), (1000, 0)).await.unwrap();
    beta.wait_for(T, |d| matches!(d, Down::Equip { actor, .. } if *actor == a).then_some(())).await.unwrap();
    alpha.wait_for(T, |d| matches!(d, Down::Equip { actor, .. } if *actor == a).then_some(())).await.unwrap();

    // and says something
    alpha.chat(chat::SAY, "hello Beta").await.unwrap();
    let heard = beta.wait_for(T, |d| if let Down::Chat(c) = d { Some(c.clone()) } else { None }).await.unwrap();
    assert_eq!(
        (heard.text.as_str(), heard.name.as_str(), heard.actor, heard.content_id),
        ("hello Beta", "Alpha Tester", a, 101)
    );

    // A player's command-looking text is chat
    beta.chat(chat::SAY, ".who").await.unwrap();
    assert_eq!(line(&mut alpha).await, ".who");

    // Alpha's account is not a GM yet: the same
    alpha.chat(chat::SAY, ".who").await.unwrap();
    assert_eq!(line(&mut beta).await, ".who");

    // promoted (as the console does), it runs, and the promotion holds at once
    server.db.login.set_gm_level_of(gm_account, 2).await.unwrap();
    alpha.chat(chat::SAY, ".gm").await.unwrap();
    assert_eq!(line(&mut alpha).await, "GM mode is off (GM level 2).", "it was a player when it logged in");
    alpha.chat(chat::SAY, ".who").await.unwrap();
    assert_eq!(line(&mut alpha).await, "GM mode is off: .gm on to use commands.");
    alpha.chat(chat::SAY, ".gm on").await.unwrap();
    assert_eq!(line(&mut alpha).await, "GM mode is on.");
    alpha.chat(chat::SAY, ".who").await.unwrap();
    assert_eq!(line(&mut alpha).await, "2 online: Alpha Tester (129), Beta Tester (129)");
    alpha.chat(chat::SAY, ".tele 129 40 0 40").await.unwrap();
    let to = alpha
        .wait_for(T, |d| if let Down::InitZone { territory, pos } = d { Some((*territory, *pos)) } else { None })
        .await
        .unwrap();
    assert_eq!(to, (129, [40.0, 0.0, 40.0]));
    alpha.loaded().await.unwrap();
    // gone from where it was, back where it went (both in range)
    beta.wait_for(T, |d| matches!(d, Down::Despawn { actor, .. } if *actor == a).then_some(())).await.unwrap();
    beta.wait_for(T, |d| matches!(d, Down::Spawn { actor, .. } if *actor == a).then_some(())).await.unwrap();

    // a battle NPC placed by hand: saved, and Beta (53 yalms off, in range) sees it appear
    alpha.chat(chat::SAY, ".bnpc 37 37 2").await.unwrap();
    let placed = line(&mut alpha).await;
    assert!(placed.starts_with("Placed #37 (base 37, level 2) at 40.0 0.0 40.0"), "{placed}");
    let bnpc = beta
        .wait_for(T, |d| match d {
            Down::Spawn { spawn, .. } if spawn.kind == 2 => Some(spawn.clone()),
            _ => None,
        })
        .await
        .unwrap();
    assert_eq!((bnpc.base_id, bnpc.name_id, bnpc.level), (37, 37, 2));
    alpha.chat(chat::SAY, ".bnpc near 5").await.unwrap();
    assert_eq!(line(&mut alpha).await, "1 battle NPCs within 5 yalms:");
    assert!(line(&mut alpha).await.contains("(hand)"));

    // demoted: refused at once (shouted: Alpha is out of say range now)
    server.db.login.set_gm_level_of(gm_account, 0).await.unwrap();
    alpha.chat(chat::SHOUT, ".who").await.unwrap();
    assert_eq!(line(&mut beta).await, ".who");

    // Beta disconnects; Alpha sees it go
    drop(beta);
    alpha.wait_for(T, |d| matches!(d, Down::Despawn { actor, .. } if *actor == b).then_some(())).await.unwrap();
    assert_eq!(server.world().unwrap().online().len(), 1);

    tokio::time::timeout(Duration::from_secs(5), server.shutdown()).await.expect("shutdown");
}
