//! A headless gladiator uses Fast Blade on a rat, against the 672261 install when
//! `CRYSTAL_TEST_GAME` names its `game` folder (actions come from its Action sheet; not in CI).

use std::path::PathBuf;
use std::time::Duration;

use crystal_bot::{Bot, Down};
use crystal_common::config::DatabaseEndpoint;
use crystal_common::{Config, Role};
use crystal_db::Position;
use crystal_protocol::zone::{self as ipc, chat};
use crystal_runtime::Server;

const T: Duration = Duration::from_secs(10);
const FAST_BLADE: u32 = 9;

#[tokio::test]
async fn fast_blade_hits_and_answers_the_request() {
    let Ok(game) = std::env::var("CRYSTAL_TEST_GAME") else {
        eprintln!("CRYSTAL_TEST_GAME not set: skipped");
        return;
    };
    let dir = tempfile::tempdir().unwrap();
    let path = |name: &str| dir.path().join(name).to_string_lossy().replace('\\', "/");
    let mut config = Config { roles: vec![Role::Zone], ..Config::default() };
    config.database.login = DatabaseEndpoint::sqlite(&path("var/login.db"));
    config.database.characters = DatabaseEndpoint::sqlite(&path("var/characters.db"));
    config.database.world = DatabaseEndpoint::sqlite(&path("var/world.db"));
    config.zone.bind = "127.0.0.1:0".parse().unwrap();
    config.data.game = Some(PathBuf::from(game));
    config.data.seed = None;
    let server = Server::start(config).await.unwrap();
    let addr = server.local_addr(Role::Zone).unwrap();

    // a level-1 gladiator (the default class), a GM to place the rat
    let account = server.db.login.ensure_account("gm", 0).await.unwrap();
    server.db.login.set_gm_level_of(account, 2).await.unwrap();
    let db = &server.db.characters;
    let id = db.create_character(account, 201, 1, "Gamma Tester", "", 0).await.unwrap();
    db.save_position(201, Position { zone_id: 129, x: 0.0, y: 0.0, z: 0.0, rotation: 0.0 }).await.unwrap();
    let me = 0x1000_0000 + id as u32;
    let mut bot = Bot::connect(addr, me).await.unwrap();
    bot.enter().await.unwrap();

    bot.chat(chat::SAY, ".bnpc 347 417 1").await.unwrap();
    let placed = bot
        .wait_for(T, |d| match d {
            Down::Chat(c) if c.text.starts_with("Placed") => Some(c.text.clone()),
            _ => None,
        })
        .await
        .unwrap();
    let rat = u32::from_str_radix(placed.rsplit("actor 0x").next().unwrap().trim_end_matches('.'), 16).unwrap();

    // Fast Blade: answered as action 9 with the request's sequence, a damage effect on the rat
    bot.use_action(FAST_BLADE, rat, 77).await.unwrap();
    let (action, sequence, target, effect) = bot
        .wait_for(T, |d| match d {
            Down::Action { source, action, sequence, target, effect } if *source == me => {
                Some((*action, *sequence, *target, *effect))
            }
            _ => None,
        })
        .await
        .unwrap();
    assert_eq!((action, sequence, target, effect.kind), (FAST_BLADE as u16, 77, rat, ipc::Effect::DAMAGE));
    assert!(effect.value > 0);

    // again at once: still on the global cooldown, so no answer; the auto-attack's is next
    bot.use_action(FAST_BLADE, rat, 78).await.unwrap();
    bot.trigger(ipc::trigger::AUTO_ATTACK, [1, rat, 0, 0]).await.unwrap();
    let next = bot
        .wait_for(T, |d| match d {
            Down::Action { source, action, sequence, .. } if *source == me => Some((*action, *sequence)),
            _ => None,
        })
        .await
        .unwrap();
    assert_eq!(next, (ipc::ActionResult::AUTO_ATTACK, 0));

    tokio::time::timeout(Duration::from_secs(5), server.shutdown()).await.expect("shutdown");
}
