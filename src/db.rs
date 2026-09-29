use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Unpinned, unsaved clips beyond this count are dropped (oldest first).
const HISTORY_LIMIT: i64 = 1000;
/// Clips larger than this are not stored.
pub const MAX_CLIP_BYTES: usize = 1 << 20;

#[derive(Clone, Debug)]
pub struct Clip {
    pub id: i64,
    pub content: String,
    pub pinned: bool,
    pub saved: bool,
}

pub struct Db {
    conn: Connection,
}

pub fn data_dir() -> PathBuf {
    let dir = dirs::data_dir().unwrap_or_else(|| PathBuf::from(".")).join("clipr");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

impl Db {
    pub fn open() -> Result<Self> {
        let path = data_dir().join("clipr.db");
        let conn = Connection::open(&path).with_context(|| format!("opening {}", path.display()))?;
        conn.busy_timeout(std::time::Duration::from_secs(2))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS clips (
                 id        INTEGER PRIMARY KEY,
                 content   TEXT NOT NULL UNIQUE,
                 created   INTEGER NOT NULL,
                 last_used INTEGER NOT NULL,
                 pinned    INTEGER NOT NULL DEFAULT 0,
                 saved_at  INTEGER
             );
             CREATE INDEX IF NOT EXISTS clips_last_used ON clips(last_used);",
        )?;
        Ok(Self { conn })
    }

    /// Record a copied text. Re-copying an existing clip moves it to the top.
    pub fn add(&self, content: &str) -> Result<()> {
        if content.trim().is_empty() || content.len() > MAX_CLIP_BYTES {
            return Ok(());
        }
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO clips (content, created, last_used) VALUES (?1, ?2, ?2)
             ON CONFLICT(content) DO UPDATE SET last_used = excluded.last_used",
            params![content, now],
        )?;
        self.conn.execute(
            "DELETE FROM clips WHERE pinned = 0 AND saved_at IS NULL AND id NOT IN (
                 SELECT id FROM clips WHERE pinned = 0 AND saved_at IS NULL
                 ORDER BY last_used DESC LIMIT ?1)",
            params![HISTORY_LIMIT],
        )?;
        Ok(())
    }

    fn query(&self, sql: &str) -> Result<Vec<Clip>> {
        let mut stmt = self.conn.prepare(sql)?;
        let rows = stmt.query_map([], |r| {
            Ok(Clip {
                id: r.get(0)?,
                content: r.get(1)?,
                pinned: r.get(2)?,
                saved: r.get::<_, Option<i64>>(3)?.is_some(),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// All clips: pinned first, then most recently used.
    pub fn history(&self) -> Result<Vec<Clip>> {
        self.query(
            "SELECT id, content, pinned, saved_at FROM clips
             ORDER BY pinned DESC, last_used DESC",
        )
    }

    /// Saved clips in the order they were saved, so their numbers stay stable.
    pub fn saved(&self) -> Result<Vec<Clip>> {
        self.query(
            "SELECT id, content, pinned, saved_at FROM clips
             WHERE saved_at IS NOT NULL ORDER BY saved_at ASC",
        )
    }

    pub fn get(&self, id: i64) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT content FROM clips WHERE id = ?1", [id], |r| r.get(0))
            .optional()?)
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> Result<()> {
        self.conn
            .execute("UPDATE clips SET pinned = ?2 WHERE id = ?1", params![id, pinned])?;
        Ok(())
    }

    pub fn set_saved(&self, id: i64, saved: bool) -> Result<()> {
        let saved_at = saved.then(now_ms);
        self.conn
            .execute("UPDATE clips SET saved_at = ?2 WHERE id = ?1", params![id, saved_at])?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM clips WHERE id = ?1", [id])?;
        Ok(())
    }
}
