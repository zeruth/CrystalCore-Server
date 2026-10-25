//! SQLite always; PostgreSQL when `CRYSTAL_TEST_POSTGRES` names a server
//! (`postgres://crystal@127.0.0.1:5432`, see `tools/dev.ps1`). Each Postgres run creates and
//! drops its own databases.

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

    let build: i32 = sqlx::query("SELECT build FROM patch WHERE version = $1")
        .bind("2013.08.09.0001.0000")
        .fetch_one(&db.world.pool)
        .await
        .unwrap()
        .get(0);
    assert_eq!(build, 672261);
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
