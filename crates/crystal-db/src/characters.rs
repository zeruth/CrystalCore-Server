//! The characters database's `characters` table.

use sqlx::Row;

use crate::{Database, DbError, Result};

/// A stored character. `content_id` is the u64 the client knows it by (stored as i64).
#[derive(Debug, Clone, PartialEq)]
pub struct Character {
    pub id: i64,
    pub account_id: i64,
    pub content_id: u64,
    pub world_id: u16,
    pub name: String,
    /// The CharaMake JSON the client sent when the character was made.
    pub appearance: String,
    /// Where it stands; `None` until it first enters a zone.
    pub position: Option<Position>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub zone_id: u32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub rotation: f64,
}

const COLUMNS: &str = "id, account_id, content_id, world_id, name, appearance, zone_id, pos_x, pos_y, pos_z, rotation";

fn from_row(row: &sqlx::any::AnyRow) -> std::result::Result<Character, sqlx::Error> {
    let zone_id: Option<i32> = row.try_get(6)?;
    let position = match zone_id {
        Some(zone_id) => Some(Position {
            zone_id: zone_id as u32,
            x: row.try_get(7)?,
            y: row.try_get(8)?,
            z: row.try_get(9)?,
            rotation: row.try_get(10)?,
        }),
        None => None,
    };
    Ok(Character {
        id: row.try_get(0)?,
        account_id: row.try_get(1)?,
        content_id: row.try_get::<i64, _>(2)? as u64,
        world_id: row.try_get::<i32, _>(3)? as u16,
        name: row.try_get(4)?,
        appearance: row.try_get(5)?,
        position,
    })
}

impl Database {
    fn sql(&self, source: sqlx::Error) -> DbError {
        DbError::Sql { db: self.name, source }
    }

    /// Stores a new character; fails if its name is taken on its world (case-insensitively).
    pub async fn create_character(
        &self,
        account_id: i64,
        content_id: u64,
        world_id: u16,
        name: &str,
        appearance: &str,
        now: i64,
    ) -> Result<i64> {
        sqlx::query(
            "INSERT INTO characters (account_id, content_id, world_id, name, appearance, created_at) \
             VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
        )
        .bind(account_id)
        .bind(content_id as i64)
        .bind(world_id as i32)
        .bind(name)
        .bind(appearance)
        .bind(now)
        .fetch_one(&self.pool)
        .await
        .map(|row| row.get(0))
        .map_err(|e| self.sql(e))
    }

    /// Whether `name` is free on `world_id` (case-insensitively), among characters not deleted.
    pub async fn character_name_free(&self, world_id: u16, name: &str) -> Result<bool> {
        let row = sqlx::query(
            "SELECT COUNT(*) FROM characters WHERE world_id = $1 AND lower(name) = lower($2) AND deleted_at IS NULL",
        )
        .bind(world_id as i32)
        .bind(name)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| self.sql(e))?;
        Ok(row.get::<i64, _>(0) == 0)
    }

    /// The account's characters, oldest first.
    pub async fn characters_of(&self, account_id: i64) -> Result<Vec<Character>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM characters WHERE account_id = $1 AND deleted_at IS NULL \
             AND content_id IS NOT NULL ORDER BY id"
        );
        let rows = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(account_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| self.sql(e))?;
        rows.iter().map(from_row).collect::<std::result::Result<_, _>>().map_err(|e| self.sql(e))
    }

    pub async fn character_by_content_id(&self, content_id: u64) -> Result<Option<Character>> {
        let sql = format!("SELECT {COLUMNS} FROM characters WHERE content_id = $1 AND deleted_at IS NULL");
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(content_id as i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| self.sql(e))?;
        row.as_ref().map(from_row).transpose().map_err(|e| self.sql(e))
    }

    pub async fn character_by_id(&self, id: i64) -> Result<Option<Character>> {
        let sql = format!("SELECT {COLUMNS} FROM characters WHERE id = $1 AND deleted_at IS NULL");
        let row =
            sqlx::query(sqlx::AssertSqlSafe(sql)).bind(id).fetch_optional(&self.pool).await.map_err(|e| self.sql(e))?;
        row.as_ref().map(from_row).transpose().map_err(|e| self.sql(e))
    }

    pub async fn save_position(&self, content_id: u64, p: Position) -> Result<()> {
        sqlx::query(
            "UPDATE characters SET zone_id = $1, pos_x = $2, pos_y = $3, pos_z = $4, rotation = $5 \
             WHERE content_id = $6",
        )
        .bind(p.zone_id as i32)
        .bind(p.x)
        .bind(p.y)
        .bind(p.z)
        .bind(p.rotation)
        .bind(content_id as i64)
        .execute(&self.pool)
        .await
        .map_err(|e| self.sql(e))?;
        Ok(())
    }

    /// The highest content id given so far (0 when none), to continue from.
    pub async fn max_content_id(&self) -> Result<u64> {
        let row = sqlx::query("SELECT COALESCE(MAX(content_id), 0) FROM characters")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| self.sql(e))?;
        Ok(row.get::<i64, _>(0) as u64)
    }
}
