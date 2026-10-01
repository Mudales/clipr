use anyhow::{Context, Result};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Unpinned, unsaved clips beyond this count are dropped (oldest first).
const HISTORY_LIMIT: i64 = 1000;
/// Unpinned, unsaved images beyond this count are dropped (they are big).
const IMAGE_LIMIT: i64 = 100;
/// Clips larger than this are not stored.
pub const MAX_CLIP_BYTES: usize = 1 << 20;

const KIND_IMAGE: i64 = 1;

#[derive(Clone, Debug)]
pub struct Clip {
    pub id: i64,
    /// The text, or for images a key like `image:1920x1080:<hash>`.
    pub content: String,
    pub pinned: bool,
    pub saved: bool,
    pub is_image: bool,
}

/// What gets put back on the clipboard.
pub enum Payload {
    Text(String),
    /// PNG bytes.
    Image(Vec<u8>),
}

pub struct Db {
    conn: Connection,
    history_limit: i64,
    image_limit: i64,
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
        // Columns added for image support (older databases lack them).
        let has_kind: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('clips') WHERE name = 'kind'")?
            .exists([])?;
        if !has_kind {
            conn.execute_batch(
                "ALTER TABLE clips ADD COLUMN kind INTEGER NOT NULL DEFAULT 0;
                 ALTER TABLE clips ADD COLUMN data BLOB;
                 ALTER TABLE clips ADD COLUMN thumb BLOB;",
            )?;
        }
        Ok(Self { conn, history_limit: HISTORY_LIMIT, image_limit: IMAGE_LIMIT })
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
        self.trim()
    }

    /// Record a copied image.
    pub fn add_image(&self, image: &crate::images::Stored) -> Result<()> {
        if image.png.len() > crate::images::MAX_IMAGE_BYTES {
            return Ok(());
        }
        let now = now_ms();
        self.conn.execute(
            "INSERT INTO clips (content, created, last_used, kind, data, thumb)
             VALUES (?1, ?2, ?2, ?3, ?4, ?5)
             ON CONFLICT(content) DO UPDATE SET last_used = excluded.last_used",
            params![crate::images::content_key(image), now, KIND_IMAGE, image.png, image.thumb],
        )?;
        self.trim()
    }

    fn trim(&self) -> Result<()> {
        for (filter, limit) in [("", self.history_limit), ("AND kind = 1", self.image_limit)] {
            self.conn.execute(
                &format!(
                    "DELETE FROM clips WHERE pinned = 0 AND saved_at IS NULL {filter} AND id NOT IN (
                         SELECT id FROM clips WHERE pinned = 0 AND saved_at IS NULL {filter}
                         ORDER BY last_used DESC LIMIT ?1)"
                ),
                params![limit],
            )?;
        }
        Ok(())
    }

    /// Sets how many unpinned, unsaved clips / images are kept (from Settings).
    pub fn set_limits(&mut self, history: usize, images: usize) {
        self.history_limit = history as i64;
        self.image_limit = images as i64;
    }

    /// Moves a clip to the top of the history.
    pub fn touch(&self, id: i64) -> Result<()> {
        self.conn
            .execute("UPDATE clips SET last_used = ?2 WHERE id = ?1", params![id, now_ms()])?;
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
                is_image: r.get::<_, i64>(4)? == KIND_IMAGE,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// All clips: pinned first, then most recently used.
    pub fn history(&self) -> Result<Vec<Clip>> {
        self.query(
            "SELECT id, content, pinned, saved_at, kind FROM clips
             ORDER BY pinned DESC, last_used DESC",
        )
    }

    /// Saved clips in the order they were saved, so their numbers stay stable.
    pub fn saved(&self) -> Result<Vec<Clip>> {
        self.query(
            "SELECT id, content, pinned, saved_at, kind FROM clips
             WHERE saved_at IS NOT NULL ORDER BY saved_at ASC",
        )
    }

    pub fn payload(&self, id: i64) -> Result<Option<Payload>> {
        Ok(self
            .conn
            .query_row("SELECT kind, content, data FROM clips WHERE id = ?1", [id], |r| {
                Ok(if r.get::<_, i64>(0)? == KIND_IMAGE {
                    Payload::Image(r.get(2)?)
                } else {
                    Payload::Text(r.get(1)?)
                })
            })
            .optional()?)
    }

    /// The most recently copied/used clip (what the clipboard last held).
    #[cfg_attr(not(target_os = "linux"), allow(dead_code))]
    pub fn latest(&self) -> Result<Option<Payload>> {
        let id: Option<i64> = self
            .conn
            .query_row("SELECT id FROM clips ORDER BY last_used DESC LIMIT 1", [], |r| r.get(0))
            .optional()?;
        match id {
            Some(id) => self.payload(id),
            None => Ok(None),
        }
    }

    pub fn thumb(&self, id: i64) -> Result<Option<Vec<u8>>> {
        Ok(self
            .conn
            .query_row("SELECT thumb FROM clips WHERE id = ?1", [id], |r| r.get(0))
            .optional()?
            .flatten())
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

    /// Deletes the whole history except pinned and saved clips.
    pub fn clear(&self) -> Result<usize> {
        Ok(self.conn.execute("DELETE FROM clips WHERE pinned = 0 AND saved_at IS NULL", [])?)
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.conn.execute("DELETE FROM clips WHERE id = ?1", [id])?;
        Ok(())
    }
}
