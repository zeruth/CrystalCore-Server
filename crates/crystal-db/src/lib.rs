//! The three databases, as in AzerothCore: `login` (accounts, worlds), `characters` (everything a
//! player owns) and `world` (server-only content and overrides of the client's EXD rows). Each has
//! its own URL, so they can live on different hosts, and each is PostgreSQL (hosted) or SQLite
//! (single process, embedded).
//!
//! Queries go through `sqlx::Any`, so code is written once for both backends. Migrations differ
//! where SQL does, so each database has a migration directory per backend under `sql/`.
//!
//! The database is never queried on a tick path: characters are loaded when they enter a zone and
//! saved on a timer and on logout.

use std::sync::Once;
use std::time::Duration;

use crystal_common::config::{DatabaseConfig, DatabaseEndpoint};
use sqlx::any::{AnyConnectOptions, AnyPoolOptions};
use sqlx::migrate::Migrator;
use sqlx::{AnyPool, Executor};
use tracing::info;

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("{db}: {source}")]
    Sql { db: &'static str, source: sqlx::Error },
    #[error("{db}: migration: {source}")]
    Migrate { db: &'static str, source: sqlx::migrate::MigrateError },
    #[error("{db}: unsupported database URL `{url}` (postgres:// or sqlite:)")]
    Url { db: &'static str, url: String },
    #[error("{db}: {source}")]
    Io { db: &'static str, source: std::io::Error },
    #[error("seed {path}: {what}")]
    Seed { path: String, what: String },
}

pub type Result<T> = std::result::Result<T, DbError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Postgres,
    Sqlite,
}

impl Backend {
    fn of(url: &str) -> Option<Self> {
        if url.starts_with("postgres://") || url.starts_with("postgresql://") {
            Some(Self::Postgres)
        } else if url.starts_with("sqlite:") {
            Some(Self::Sqlite)
        } else {
            None
        }
    }
}

/// One database: a pool and the migrations that belong to it.
#[derive(Clone, Debug)]
pub struct Database {
    pub name: &'static str,
    pub backend: Backend,
    pub pool: AnyPool,
}

#[derive(Clone, Debug)]
pub struct Databases {
    pub login: Database,
    pub characters: Database,
    pub world: Database,
}

static LOGIN_POSTGRES: Migrator = sqlx::migrate!("../../sql/login/postgres");
static LOGIN_SQLITE: Migrator = sqlx::migrate!("../../sql/login/sqlite");
static CHARACTERS_POSTGRES: Migrator = sqlx::migrate!("../../sql/characters/postgres");
static CHARACTERS_SQLITE: Migrator = sqlx::migrate!("../../sql/characters/sqlite");
static WORLD_POSTGRES: Migrator = sqlx::migrate!("../../sql/world/postgres");
static WORLD_SQLITE: Migrator = sqlx::migrate!("../../sql/world/sqlite");

impl Databases {
    pub async fn connect(config: &DatabaseConfig) -> Result<Self> {
        // Concurrently: three hosts should not cost three sequential connects.
        let (login, characters, world) = tokio::join!(
            Database::connect("login", &config.login),
            Database::connect("characters", &config.characters),
            Database::connect("world", &config.world),
        );
        Ok(Self { login: login?, characters: characters?, world: world? })
    }

    pub async fn migrate(&self) -> Result<()> {
        self.login.migrate().await?;
        self.characters.migrate().await?;
        self.world.migrate().await
    }

    pub async fn close(&self) {
        self.login.pool.close().await;
        self.characters.pool.close().await;
        self.world.pool.close().await;
    }
}

impl Database {
    pub async fn connect(name: &'static str, endpoint: &DatabaseEndpoint) -> Result<Self> {
        static DRIVERS: Once = Once::new();
        DRIVERS.call_once(sqlx::any::install_default_drivers);

        let url = endpoint.url.as_str();
        let backend = Backend::of(url).ok_or_else(|| DbError::Url { db: name, url: url.to_owned() })?;
        if backend == Backend::Sqlite {
            create_sqlite_dir(url).map_err(|source| DbError::Io { db: name, source })?;
        }
        let options: AnyConnectOptions = url.parse().map_err(|source| DbError::Sql { db: name, source })?;
        let pool = AnyPoolOptions::new()
            .max_connections(endpoint.max_connections.max(1))
            .acquire_timeout(Duration::from_secs(10))
            .after_connect(move |conn, _| {
                Box::pin(async move {
                    if backend == Backend::Sqlite {
                        // WAL lets readers run beside the writer; NORMAL is durable at checkpoints,
                        // which is what a game save needs.
                        conn.execute(
                            "PRAGMA journal_mode = WAL; PRAGMA synchronous = NORMAL; \
                             PRAGMA foreign_keys = ON; PRAGMA busy_timeout = 5000;",
                        )
                        .await?;
                    }
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .map_err(|source| DbError::Sql { db: name, source })?;
        info!(db = name, ?backend, "connected");
        Ok(Self { name, backend, pool })
    }

    fn migrator(&self) -> &'static Migrator {
        match (self.name, self.backend) {
            ("login", Backend::Postgres) => &LOGIN_POSTGRES,
            ("login", Backend::Sqlite) => &LOGIN_SQLITE,
            ("characters", Backend::Postgres) => &CHARACTERS_POSTGRES,
            ("characters", Backend::Sqlite) => &CHARACTERS_SQLITE,
            ("world", Backend::Postgres) => &WORLD_POSTGRES,
            ("world", Backend::Sqlite) => &WORLD_SQLITE,
            (name, _) => panic!("no migrations for database `{name}`"),
        }
    }

    pub async fn migrate(&self) -> Result<()> {
        let migrator = self.migrator();
        migrator.run(&self.pool).await.map_err(|source| DbError::Migrate { db: self.name, source })?;
        info!(db = self.name, migrations = migrator.iter().count(), "migrated");
        Ok(())
    }
}

/// `sqlite:var/login.db?mode=rwc`: make sure `var/` exists.
fn create_sqlite_dir(url: &str) -> std::io::Result<()> {
    let path = url.trim_start_matches("sqlite://").trim_start_matches("sqlite:");
    let path = path.split('?').next().unwrap_or_default();
    if path.is_empty() || path.starts_with(":memory:") {
        return Ok(());
    }
    match std::path::Path::new(path).parent() {
        Some(dir) if !dir.as_os_str().is_empty() => std::fs::create_dir_all(dir),
        _ => Ok(()),
    }
}

impl Database {
    /// The id of the account named `username`, made (with no password) if it does not exist. For
    /// the client's test ticket until accounts and sessions are real.
    pub async fn ensure_account(&self, username: &str, now: i64) -> Result<i64> {
        use sqlx::Row;
        let found = sqlx::query("SELECT id FROM account WHERE lower(username) = lower($1)")
            .bind(username)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| DbError::Sql { db: self.name, source })?;
        if let Some(row) = found {
            return Ok(row.get(0));
        }
        sqlx::query("INSERT INTO account (username, password_hash, created_at) VALUES ($1, '', $2) RETURNING id")
            .bind(username)
            .bind(now)
            .fetch_one(&self.pool)
            .await
            .map(|row| row.get(0))
            .map_err(|source| DbError::Sql { db: self.name, source })
    }

    /// The account's GM level (AzerothCore's security level: 0 a player, 1 and up a GM); 0 for no
    /// such account.
    pub async fn gm_level(&self, account_id: i64) -> Result<u8> {
        use sqlx::Row;
        let row = sqlx::query("SELECT gm_level FROM account WHERE id = $1")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| DbError::Sql { db: self.name, source })?;
        Ok(row.map_or(0, |r| r.get::<i16, _>(0).clamp(0, u8::MAX as i16) as u8))
    }

    pub async fn account_name(&self, account_id: i64) -> Result<Option<String>> {
        use sqlx::Row;
        let row = sqlx::query("SELECT username FROM account WHERE id = $1")
            .bind(account_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(|source| DbError::Sql { db: self.name, source })?;
        Ok(row.map(|r| r.get(0)))
    }

    /// Sets the GM level of the account named `username`; false when there is no such account.
    pub async fn set_gm_level(&self, username: &str, level: u8) -> Result<bool> {
        let done = sqlx::query("UPDATE account SET gm_level = $1 WHERE lower(username) = lower($2)")
            .bind(level as i16)
            .bind(username)
            .execute(&self.pool)
            .await
            .map_err(|source| DbError::Sql { db: self.name, source })?;
        Ok(done.rows_affected() > 0)
    }

    /// Sets the GM level of an account by id; false when there is no such account.
    pub async fn set_gm_level_of(&self, account_id: i64, level: u8) -> Result<bool> {
        let done = sqlx::query("UPDATE account SET gm_level = $1 WHERE id = $2")
            .bind(level as i16)
            .bind(account_id)
            .execute(&self.pool)
            .await
            .map_err(|source| DbError::Sql { db: self.name, source })?;
        Ok(done.rows_affected() > 0)
    }
}

pub mod characters;
pub mod seed;
pub mod world;
pub use characters::{Character, InventoryRow, Position, QuestRow};
pub use world::{BETA_PATCH, BnpcSpawn, ExdOverride};

#[cfg(test)]
mod tests;
