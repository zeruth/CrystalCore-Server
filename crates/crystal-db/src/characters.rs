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
    /// How far through its town's opening (`OPENING_SEQ_0`-`2`): 0 until its cutscenes played.
    pub opening: u8,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Position {
    pub zone_id: u32,
    pub x: f64,
    pub y: f64,
    pub z: f64,
    pub rotation: f64,
}

/// A filled inventory slot (`inventory`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InventoryRow {
    pub container: u16,
    pub slot: u16,
    pub item_id: u32,
    pub quantity: u32,
    pub hq: bool,
    pub condition: u16,
    pub spiritbond: u16,
    pub stain: u8,
}

const COLUMNS: &str =
    "id, account_id, content_id, world_id, name, appearance, zone_id, pos_x, pos_y, pos_z, rotation, opening";

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
        opening: row.try_get::<i32, _>(11)? as u8,
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

    /// A character by name on any world, ignoring case (the oldest when several worlds have one).
    pub async fn character_by_name(&self, name: &str) -> Result<Option<Character>> {
        let sql = format!(
            "SELECT {COLUMNS} FROM characters WHERE lower(name) = lower($1) AND deleted_at IS NULL ORDER BY id LIMIT 1"
        );
        let row = sqlx::query(sqlx::AssertSqlSafe(sql))
            .bind(name)
            .fetch_optional(&self.pool)
            .await
            .map_err(|e| self.sql(e))?;
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

    /// Records how far the character is through its opening.
    pub async fn save_opening(&self, content_id: u64, opening: u8) -> Result<()> {
        sqlx::query("UPDATE characters SET opening = $1 WHERE content_id = $2")
            .bind(opening as i32)
            .bind(content_id as i64)
            .execute(&self.pool)
            .await
            .map_err(|e| self.sql(e))?;
        Ok(())
    }

    /// A character's filled slots, by container and slot.
    pub async fn inventory(&self, character_id: i64) -> Result<Vec<InventoryRow>> {
        let rows = sqlx::query(
            "SELECT container, slot, item_id, quantity, hq, condition, spiritbond, stain FROM inventory \
             WHERE character_id = $1 ORDER BY container, slot",
        )
        .bind(character_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| self.sql(e))?;
        rows.iter()
            .map(|r| {
                Ok(InventoryRow {
                    container: r.try_get::<i64, _>(0)? as u16,
                    slot: r.try_get::<i64, _>(1)? as u16,
                    item_id: r.try_get::<i64, _>(2)? as u32,
                    quantity: r.try_get::<i64, _>(3)? as u32,
                    hq: r.try_get::<i64, _>(4)? != 0,
                    condition: r.try_get::<i64, _>(5)? as u16,
                    spiritbond: r.try_get::<i64, _>(6)? as u16,
                    stain: r.try_get::<i64, _>(7)? as u8,
                })
            })
            .collect::<std::result::Result<_, sqlx::Error>>()
            .map_err(|e| self.sql(e))
    }

    /// Replaces a character's items with `rows`, in one transaction.
    pub async fn save_inventory(&self, character_id: i64, rows: &[InventoryRow]) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(|e| self.sql(e))?;
        sqlx::query("DELETE FROM inventory WHERE character_id = $1")
            .bind(character_id)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.sql(e))?;
        for r in rows {
            sqlx::query(
                "INSERT INTO inventory (character_id, container, slot, item_id, quantity, hq, condition, spiritbond, stain) \
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(character_id)
            .bind(r.container as i32)
            .bind(r.slot as i32)
            .bind(r.item_id as i64)
            .bind(r.quantity as i64)
            .bind(r.hq as i32)
            .bind(r.condition as i32)
            .bind(r.spiritbond as i32)
            .bind(r.stain as i32)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.sql(e))?;
        }
        tx.commit().await.map_err(|e| self.sql(e))
    }

    /// A character's (slot, level, experience) for every class slot it has levelled.
    pub async fn class_progress(&self, character_id: i64) -> Result<Vec<(u8, u8, u32)>> {
        let rows = sqlx::query("SELECT slot, level, exp FROM class_progress WHERE character_id = $1 ORDER BY slot")
            .bind(character_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| self.sql(e))?;
        rows.iter()
            .map(|r| Ok((r.try_get::<i64, _>(0)? as u8, r.try_get::<i64, _>(1)? as u8, r.try_get::<i64, _>(2)? as u32)))
            .collect::<std::result::Result<_, sqlx::Error>>()
            .map_err(|e| self.sql(e))
    }

    /// A character's active quests: (slot, quest id, sequence), by slot.
    pub async fn quests(&self, character_id: i64) -> Result<Vec<(u8, u32, u8)>> {
        let rows = sqlx::query("SELECT slot, quest_id, sequence FROM quests WHERE character_id = $1 ORDER BY slot")
            .bind(character_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| self.sql(e))?;
        rows.iter()
            .map(|r| Ok((r.try_get::<i64, _>(0)? as u8, r.try_get::<i64, _>(1)? as u32, r.try_get::<i64, _>(2)? as u8)))
            .collect::<std::result::Result<_, sqlx::Error>>()
            .map_err(|e| self.sql(e))
    }

    /// Puts a quest in a slot at a sequence (replacing what the slot held).
    pub async fn set_quest(&self, character_id: i64, slot: u8, quest_id: u32, sequence: u8) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(|e| self.sql(e))?;
        sqlx::query("DELETE FROM quests WHERE character_id = $1 AND slot = $2")
            .bind(character_id)
            .bind(slot as i32)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.sql(e))?;
        sqlx::query("INSERT INTO quests (character_id, slot, quest_id, sequence) VALUES ($1, $2, $3, $4)")
            .bind(character_id)
            .bind(slot as i32)
            .bind(quest_id as i32)
            .bind(sequence as i32)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.sql(e))?;
        tx.commit().await.map_err(|e| self.sql(e))
    }

    /// The quests a character has completed.
    pub async fn completed_quests(&self, character_id: i64) -> Result<Vec<u32>> {
        let rows = sqlx::query("SELECT quest_id FROM quest_complete WHERE character_id = $1 ORDER BY quest_id")
            .bind(character_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|e| self.sql(e))?;
        rows.iter()
            .map(|r| Ok(r.try_get::<i64, _>(0)? as u32))
            .collect::<std::result::Result<_, sqlx::Error>>()
            .map_err(|e| self.sql(e))
    }

    /// Completes a quest: out of its slot, into the completed ones.
    pub async fn complete_quest(&self, character_id: i64, slot: u8, quest_id: u32) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(|e| self.sql(e))?;
        sqlx::query("DELETE FROM quests WHERE character_id = $1 AND slot = $2")
            .bind(character_id)
            .bind(slot as i32)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.sql(e))?;
        sqlx::query("DELETE FROM quest_complete WHERE character_id = $1 AND quest_id = $2")
            .bind(character_id)
            .bind(quest_id as i32)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.sql(e))?;
        sqlx::query("INSERT INTO quest_complete (character_id, quest_id) VALUES ($1, $2)")
            .bind(character_id)
            .bind(quest_id as i32)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.sql(e))?;
        tx.commit().await.map_err(|e| self.sql(e))
    }

    /// Sets one class slot's level and experience.
    pub async fn set_class_progress(&self, character_id: i64, slot: u8, level: u8, exp: u32) -> Result<()> {
        let mut tx = self.pool.begin().await.map_err(|e| self.sql(e))?;
        sqlx::query("DELETE FROM class_progress WHERE character_id = $1 AND slot = $2")
            .bind(character_id)
            .bind(slot as i32)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.sql(e))?;
        sqlx::query("INSERT INTO class_progress (character_id, slot, level, exp) VALUES ($1, $2, $3, $4)")
            .bind(character_id)
            .bind(slot as i32)
            .bind(level as i32)
            .bind(exp as i64)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.sql(e))?;
        tx.commit().await.map_err(|e| self.sql(e))
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
