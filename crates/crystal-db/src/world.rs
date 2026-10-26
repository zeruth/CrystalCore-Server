//! The world database: overrides of the client's EXD rows, and server-only content (battle NPC
//! spawns, loot, gathering points, shops).

use sqlx::Row;

use crate::{Database, DbError, Result};

/// The patch the β client is (`world.patch`).
pub const BETA_PATCH: &str = "2013.08.09.0001.0000";

#[derive(Debug, Clone, PartialEq)]
pub struct ExdOverride {
    pub sheet: String,
    pub row_id: u32,
    pub col: u32,
    pub value: String,
    pub comment: Option<String>,
}

/// A battle NPC's spawn point (`bnpc_spawn`).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BnpcSpawn {
    pub territory: u16,
    pub instance_id: Option<u32>,
    pub layer: String,
    pub base_id: u32,
    pub name_id: u32,
    pub level: u16,
    pub pos: [f32; 3],
    pub rotation: f32,
    pub wander_range: f32,
    pub respawn_secs: u32,
    pub pop_weather: u32,
    pub pop_time_start: u32,
    pub pop_time_end: u32,
    pub loot_table: u32,
    pub link_group: u32,
    pub link_family: u32,
    pub link_range: u32,
    pub normal_ai: u32,
    /// Attacks players who come near (rather than only fighting back).
    pub aggressive: bool,
    pub source: String,
}

fn insert_spawn<'q>(s: &'q BnpcSpawn, patch: &'q str) -> sqlx::query::Query<'q, sqlx::Any, sqlx::any::AnyArguments> {
    sqlx::query(
        "INSERT INTO bnpc_spawn (territory, instance_id, layer, base_id, name_id, level, x, y, z, rotation, \
         wander_range, respawn_secs, pop_weather, pop_time_start, pop_time_end, loot_table, link_group, \
         link_family, link_range, normal_ai, aggressive, source, patch) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, $21, $22, $23)",
    )
    .bind(s.territory as i32)
    .bind(s.instance_id.map(|i| i as i64))
    .bind(&s.layer)
    .bind(s.base_id as i64)
    .bind(s.name_id as i64)
    .bind(s.level as i32)
    .bind(s.pos[0] as f64)
    .bind(s.pos[1] as f64)
    .bind(s.pos[2] as f64)
    .bind(s.rotation as f64)
    .bind(s.wander_range as f64)
    .bind(s.respawn_secs as i64)
    .bind(s.pop_weather as i64)
    .bind(s.pop_time_start as i64)
    .bind(s.pop_time_end as i64)
    .bind(s.loot_table as i64)
    .bind(s.link_group as i64)
    .bind(s.link_family as i64)
    .bind(s.link_range as i64)
    .bind(s.normal_ai as i64)
    .bind(s.aggressive as i32)
    .bind(&s.source)
    .bind(patch)
}

impl Database {
    fn world_sql(&self, source: sqlx::Error) -> DbError {
        DbError::Sql { db: self.name, source }
    }

    /// Every override for `patch`.
    pub async fn exd_overrides(&self, patch: &str) -> Result<Vec<ExdOverride>> {
        let rows = sqlx::query(
            "SELECT sheet, row_id, col, value, comment FROM exd_override WHERE patch = $1 ORDER BY sheet, row_id, col",
        )
        .bind(patch)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| self.world_sql(e))?;
        rows.iter()
            .map(|r| {
                Ok(ExdOverride {
                    sheet: r.try_get(0)?,
                    row_id: r.try_get::<i64, _>(1)? as u32,
                    col: r.try_get::<i64, _>(2)? as u32,
                    value: r.try_get(3)?,
                    comment: r.try_get(4)?,
                })
            })
            .collect::<std::result::Result<_, sqlx::Error>>()
            .map_err(|e| self.world_sql(e))
    }

    /// Sets (or replaces) one override.
    pub async fn set_exd_override(&self, patch: &str, o: &ExdOverride) -> Result<()> {
        sqlx::query("DELETE FROM exd_override WHERE sheet = $1 AND row_id = $2 AND col = $3 AND patch = $4")
            .bind(&o.sheet)
            .bind(o.row_id as i64)
            .bind(o.col as i64)
            .bind(patch)
            .execute(&self.pool)
            .await
            .map_err(|e| self.world_sql(e))?;
        sqlx::query(
            "INSERT INTO exd_override (sheet, row_id, col, value, patch, comment) VALUES ($1, $2, $3, $4, $5, $6)",
        )
        .bind(&o.sheet)
        .bind(o.row_id as i64)
        .bind(o.col as i64)
        .bind(&o.value)
        .bind(patch)
        .bind(&o.comment)
        .execute(&self.pool)
        .await
        .map_err(|e| self.world_sql(e))?;
        Ok(())
    }

    /// Removes one override; false when there was none.
    pub async fn remove_exd_override(&self, patch: &str, sheet: &str, row_id: u32, col: u32) -> Result<bool> {
        let done = sqlx::query("DELETE FROM exd_override WHERE sheet = $1 AND row_id = $2 AND col = $3 AND patch = $4")
            .bind(sheet)
            .bind(row_id as i64)
            .bind(col as i64)
            .bind(patch)
            .execute(&self.pool)
            .await
            .map_err(|e| self.world_sql(e))?;
        Ok(done.rows_affected() > 0)
    }

    /// Replaces every spawn from `source` for `patch` with `spawns`, in one transaction.
    pub async fn replace_bnpc_spawns(&self, patch: &str, source: &str, spawns: &[BnpcSpawn]) -> Result<u64> {
        let mut tx = self.pool.begin().await.map_err(|e| self.world_sql(e))?;
        sqlx::query("DELETE FROM bnpc_spawn WHERE source = $1 AND patch = $2")
            .bind(source)
            .bind(patch)
            .execute(&mut *tx)
            .await
            .map_err(|e| self.world_sql(e))?;
        for s in spawns {
            insert_spawn(s, patch).execute(&mut *tx).await.map_err(|e| self.world_sql(e))?;
        }
        tx.commit().await.map_err(|e| self.world_sql(e))?;
        Ok(spawns.len() as u64)
    }

    /// Adds one spawn (placed by hand, say).
    pub async fn add_bnpc_spawn(&self, patch: &str, spawn: &BnpcSpawn) -> Result<()> {
        insert_spawn(spawn, patch).execute(&self.pool).await.map_err(|e| self.world_sql(e))?;
        Ok(())
    }

    /// A territory's spawns for `patch`, or every territory's.
    pub async fn bnpc_spawns(&self, patch: &str, territory: Option<u16>) -> Result<Vec<BnpcSpawn>> {
        let rows = sqlx::query(
            "SELECT territory, instance_id, layer, base_id, name_id, level, x, y, z, rotation, wander_range, \
             respawn_secs, pop_weather, pop_time_start, pop_time_end, loot_table, link_group, link_family, \
             link_range, normal_ai, aggressive, source FROM bnpc_spawn WHERE patch = $1 AND ($2 < 0 OR territory = $2) ORDER BY id",
        )
        .bind(patch)
        .bind(territory.map_or(-1, |t| t as i32))
        .fetch_all(&self.pool)
        .await
        .map_err(|e| self.world_sql(e))?;
        rows.iter()
            .map(|r| {
                let u = |i: usize| r.try_get::<i64, _>(i).map(|v| v as u32);
                let f = |i: usize| r.try_get::<f64, _>(i).map(|v| v as f32);
                Ok(BnpcSpawn {
                    territory: r.try_get::<i32, _>(0)? as u16,
                    instance_id: r.try_get::<Option<i64>, _>(1)?.map(|v| v as u32),
                    layer: r.try_get(2)?,
                    base_id: u(3)?,
                    name_id: u(4)?,
                    level: r.try_get::<i32, _>(5)? as u16,
                    pos: [f(6)?, f(7)?, f(8)?],
                    rotation: f(9)?,
                    wander_range: f(10)?,
                    respawn_secs: u(11)?,
                    pop_weather: u(12)?,
                    pop_time_start: u(13)?,
                    pop_time_end: u(14)?,
                    loot_table: u(15)?,
                    link_group: u(16)?,
                    link_family: u(17)?,
                    link_range: u(18)?,
                    normal_ai: u(19)?,
                    aggressive: u(20)? != 0,
                    source: r.try_get(21)?,
                })
            })
            .collect::<std::result::Result<_, sqlx::Error>>()
            .map_err(|e| self.world_sql(e))
    }

    /// Spawns per territory for `patch`: (territory, count), by territory.
    pub async fn bnpc_spawn_counts(&self, patch: &str) -> Result<Vec<(u16, u64)>> {
        let rows = sqlx::query(
            "SELECT territory, COUNT(*) FROM bnpc_spawn WHERE patch = $1 GROUP BY territory ORDER BY territory",
        )
        .bind(patch)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| self.world_sql(e))?;
        rows.iter()
            .map(|r| Ok((r.try_get::<i32, _>(0)? as u16, r.try_get::<i64, _>(1)? as u64)))
            .collect::<std::result::Result<_, sqlx::Error>>()
            .map_err(|e| self.world_sql(e))
    }
}
