//! SQLite always; PostgreSQL when `CRYSTAL_TEST_POSTGRES` names a server
//! (`postgres://crystal@127.0.0.1:5432`, see `tools/dev.ps1`). Each Postgres run creates and
//! drops its own databases.

use std::path::Path;
use std::sync::atomic::{AtomicU32, Ordering};

use crystal_common::config::{DatabaseConfig, DatabaseEndpoint};
use sqlx::{AssertSqlSafe, Row};

use super::*;

async fn exercise(db: &Databases) {
    db.migrate().await.unwrap();
    // A second run applies nothing.
    db.migrate().await.unwrap();

    let id: i64 =
        sqlx::query("INSERT INTO account (username, password_hash, created_at) VALUES ($1, $2, $3) RETURNING id")
            .bind("Tester")
            .bind("x")
            .bind(1_700_000_000i64)
            .fetch_one(&db.login.pool)
            .await
            .unwrap()
            .get(0);
    let dup = sqlx::query("INSERT INTO account (username, password_hash, created_at) VALUES ($1, $2, $3)")
        .bind("tester")
        .bind("x")
        .bind(0i64)
        .execute(&db.login.pool)
        .await;
    assert!(dup.is_err(), "usernames are unique regardless of case");

    assert_eq!(db.login.gm_level(id).await.unwrap(), 0);
    assert!(db.login.set_gm_level("TESTER", 2).await.unwrap());
    assert_eq!(db.login.gm_level(id).await.unwrap(), 2);
    assert!(db.login.set_gm_level_of(id, 0).await.unwrap());
    assert_eq!(db.login.gm_level(id).await.unwrap(), 0);
    assert!(!db.login.set_gm_level("nobody", 1).await.unwrap());
    assert_eq!(db.login.gm_level(12345).await.unwrap(), 0);

    sqlx::query("INSERT INTO characters (account_id, world_id, name, created_at) VALUES ($1, $2, $3, $4)")
        .bind(id)
        .bind(1i32)
        .bind("Tester Name")
        .bind(0i64)
        .execute(&db.characters.pool)
        .await
        .unwrap();
    let name: String = sqlx::query("SELECT name FROM characters WHERE account_id = $1")
        .bind(id)
        .fetch_one(&db.characters.pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(name, "Tester Name");

    // the characters repository
    let account = db.login.ensure_account("test", 1).await.unwrap();
    assert_eq!(db.login.ensure_account("TEST", 2).await.unwrap(), account);
    let chars = &db.characters;
    assert_eq!(chars.max_content_id().await.unwrap(), 0);
    let content_id = 0x0040_0000_0000_0001u64;
    assert!(chars.character_name_free(1, "Null Crystal").await.unwrap());
    chars.create_character(account, content_id, 1, "Null Crystal", "{}", 1).await.unwrap();
    assert!(!chars.character_name_free(1, "null crystal").await.unwrap());
    assert!(chars.create_character(account, content_id + 1, 1, "NULL CRYSTAL", "{}", 1).await.is_err());
    assert_eq!(chars.max_content_id().await.unwrap(), content_id);
    let list = chars.characters_of(account).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!((list[0].content_id, list[0].world_id, list[0].name.as_str()), (content_id, 1, "Null Crystal"));
    assert_eq!(list[0].position, None);
    let at = Position { zone_id: 132, x: 1.5, y: 2.0, z: -3.25, rotation: 0.5 };
    chars.save_position(content_id, at).await.unwrap();
    let back = chars.character_by_content_id(content_id).await.unwrap().unwrap();
    assert_eq!(back.position, Some(at));

    let build: i32 = sqlx::query("SELECT build FROM patch WHERE version = $1")
        .bind("2013.08.09.0001.0000")
        .fetch_one(&db.world.pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(build, 672261);

    // the world repository: overrides and battle NPC spawns
    let w = &db.world;
    let o = ExdOverride { sheet: "BNpcName".into(), row_id: 2, col: 0, value: "ruins sprinter".into(), comment: None };
    w.set_exd_override(BETA_PATCH, &o).await.unwrap();
    w.set_exd_override(BETA_PATCH, &ExdOverride { value: "ruins dasher".into(), ..o.clone() }).await.unwrap();
    let all = w.exd_overrides(BETA_PATCH).await.unwrap();
    assert_eq!(all.len(), 1, "replaced, not added");
    assert_eq!(all[0].value, "ruins dasher");
    assert!(w.exd_overrides("2014.05.27.0000.0000").await.unwrap().is_empty());
    assert!(w.remove_exd_override(BETA_PATCH, "BNpcName", 2, 0).await.unwrap());

    let spawn = BnpcSpawn {
        territory: 134,
        instance_id: Some(1132780),
        layer: "LVD_BNPC_01".into(),
        base_id: 37,
        name_id: 37,
        level: 2,
        pos: [42.75, 6.5, -222.125],
        rotation: 0.75,
        wander_range: 6.0,
        source: "sapphire".into(),
        ..Default::default()
    };
    let hand = BnpcSpawn { instance_id: None, source: "hand".into(), ..spawn.clone() };
    assert_eq!(w.replace_bnpc_spawns(BETA_PATCH, "sapphire", std::slice::from_ref(&spawn)).await.unwrap(), 1);
    w.replace_bnpc_spawns(BETA_PATCH, "hand", &[hand.clone(), hand.clone()]).await.unwrap();
    // importing again replaces that source's rows only
    w.replace_bnpc_spawns(BETA_PATCH, "sapphire", std::slice::from_ref(&spawn)).await.unwrap();
    let back = w.bnpc_spawns(BETA_PATCH, Some(134)).await.unwrap();
    assert_eq!(w.bnpc_spawns(BETA_PATCH, None).await.unwrap().len(), 3);
    assert!(w.bnpc_spawns(BETA_PATCH, Some(135)).await.unwrap().is_empty());
    assert_eq!(back, vec![hand.clone(), hand, spawn]);
    assert_eq!(w.bnpc_spawn_counts(BETA_PATCH).await.unwrap(), vec![(134, 3)]);

    // seed files: written, read back, and loaded only into an empty table
    let dir = tempfile::tempdir().unwrap();
    let all = w.bnpc_spawns(BETA_PATCH, None).await.unwrap();
    seed::write_bnpcs(&dir.path().join(seed::BNPC_SPAWNS), &all).unwrap();
    assert_eq!(seed::read_bnpcs(&dir.path().join(seed::BNPC_SPAWNS)).unwrap(), all);
    assert_eq!(seed::seed_world(w, dir.path(), BETA_PATCH).await.unwrap().bnpc_spawns, 0, "not empty: left alone");
    let other = "2013.12.05.0000.0000";
    assert_eq!(seed::seed_world(w, dir.path(), other).await.unwrap().bnpc_spawns, 3);
    assert_eq!(w.bnpc_spawns(other, None).await.unwrap().len(), 3);
    assert_eq!(seed::seed_world(w, Path::new("no/such/dir"), "x").await.unwrap(), seed::Seeded::default());
}

#[tokio::test]
async fn sqlite() {
    let dir = tempfile::tempdir().unwrap();
    let at =
        |name: &str| DatabaseEndpoint::sqlite(&dir.path().join("var").join(name).to_string_lossy().replace('\\', "/"));
    let config = DatabaseConfig {
        auto_migrate: true,
        login: at("login.db"),
        characters: at("characters.db"),
        world: at("world.db"),
    };
    let db = Databases::connect(&config).await.unwrap();
    assert_eq!(db.login.backend, Backend::Sqlite);
    exercise(&db).await;
    db.close().await;
}

#[tokio::test]
async fn postgres() {
    let Ok(server) = std::env::var("CRYSTAL_TEST_POSTGRES") else {
        eprintln!("CRYSTAL_TEST_POSTGRES not set: skipped");
        return;
    };
    static RUN: AtomicU32 = AtomicU32::new(0);
    let suffix = format!("{}_{}", std::process::id(), RUN.fetch_add(1, Ordering::Relaxed));
    let admin = Database::connect("admin", &DatabaseEndpoint { url: format!("{server}/postgres"), max_connections: 1 })
        .await
        .unwrap();
    let names: Vec<String> =
        ["login", "characters", "world"].iter().map(|n| format!("crystal_test_{n}_{suffix}")).collect();
    for name in &names {
        // Names are generated above, never input.
        sqlx::raw_sql(AssertSqlSafe(format!("CREATE DATABASE {name}"))).execute(&admin.pool).await.unwrap();
    }
    let at = |name: &str| DatabaseEndpoint { url: format!("{server}/{name}"), max_connections: 4 };
    let config =
        DatabaseConfig { auto_migrate: true, login: at(&names[0]), characters: at(&names[1]), world: at(&names[2]) };

    let db = Databases::connect(&config).await.unwrap();
    assert_eq!(db.login.backend, Backend::Postgres);
    exercise(&db).await;
    db.close().await;

    for name in &names {
        sqlx::raw_sql(AssertSqlSafe(format!("DROP DATABASE {name}"))).execute(&admin.pool).await.unwrap();
    }
    admin.pool.close().await;
}
