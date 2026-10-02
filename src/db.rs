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
    /// When it was last copied or pasted (ms since 1970).
    pub last_used: i64,
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
        Self::open_at(&data_dir().join("clipr.db"))
    }

    pub fn open_at(path: &std::path::Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
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
        // The last deleted clips, for Undo (same columns as `clips`).
        conn.execute_batch("CREATE TABLE IF NOT EXISTS trash AS SELECT * FROM clips WHERE 0;")?;
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
                last_used: r.get(5)?,
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// All clips: pinned first, then most recently used.
    pub fn history(&self) -> Result<Vec<Clip>> {
        self.query(
            "SELECT id, content, pinned, saved_at, kind, last_used FROM clips
             ORDER BY pinned DESC, last_used DESC",
        )
    }

    /// Saved clips in the order they were saved, so their numbers stay stable.
    pub fn saved(&self) -> Result<Vec<Clip>> {
        self.query(
            "SELECT id, content, pinned, saved_at, kind, last_used FROM clips
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

    /// Deletes the clips matching `filter` (a WHERE clause on `clips`). With
    /// `undoable`, they replace the previous ones in the trash, for `undo`.
    fn remove(&self, filter: &str, undoable: bool) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        if undoable {
            tx.execute("DELETE FROM trash", [])?;
            tx.execute(&format!("INSERT INTO trash SELECT * FROM clips WHERE {filter}"), [])?;
        }
        let n = tx.execute(&format!("DELETE FROM clips WHERE {filter}"), [])?;
        tx.commit()?;
        Ok(n)
    }

    /// Deletes the whole history except pinned and saved clips. Not undoable
    /// is for privacy (clear after restart): it also empties the trash.
    pub fn clear(&self, undoable: bool) -> Result<usize> {
        if !undoable {
            self.conn.execute("DELETE FROM trash", [])?;
        }
        self.remove("pinned = 0 AND saved_at IS NULL", undoable)
    }

    /// Deletes these clips (undoable).
    pub fn delete_many(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let list: Vec<String> = ids.iter().map(i64::to_string).collect();
        self.remove(&format!("id IN ({})", list.join(",")), true)
    }

    pub fn delete(&self, id: i64) -> Result<()> {
        self.delete_many(&[id]).map(|_| ())
    }

    /// How many clips the last delete removed (what Undo would restore).
    pub fn trash_count(&self) -> usize {
        self.conn
            .query_row("SELECT COUNT(*) FROM trash", [], |r| r.get::<_, i64>(0))
            .map(|n| n as usize)
            .unwrap_or(0)
    }

    /// Puts the last deleted clips back; returns how many. Clips copied again
    /// since (same content) are kept as they are.
    pub fn undo(&self) -> Result<usize> {
        let tx = self.conn.unchecked_transaction()?;
        let n = tx.execute(
            "INSERT OR IGNORE INTO clips (content, created, last_used, pinned, saved_at, kind, data, thumb)
             SELECT content, created, last_used, pinned, saved_at, kind, data, thumb FROM trash",
            [],
        )?;
        tx.execute("DELETE FROM trash", [])?;
        tx.commit()?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delete_undo_and_clear() {
        let path = std::env::temp_dir().join(format!("clipr-test-{}.db", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let db = Db::open_at(&path).unwrap();
        for t in ["one", "two", "three"] {
            db.add(t).unwrap();
        }
        let ids: Vec<i64> = db.history().unwrap().iter().map(|c| c.id).collect();
        let pinned = ids[0];
        db.set_pinned(pinned, true).unwrap();

        // Delete two, undo brings them back.
        assert_eq!(db.delete_many(&ids[1..]).unwrap(), 2);
        assert_eq!(db.history().unwrap().len(), 1);
        assert_eq!(db.trash_count(), 2);
        assert_eq!(db.undo().unwrap(), 2);
        assert_eq!(db.history().unwrap().len(), 3);
        assert_eq!(db.trash_count(), 0);

        // Clear keeps the pinned clip and is undoable...
        assert_eq!(db.clear(true).unwrap(), 2);
        assert_eq!(db.history().unwrap().len(), 1);
        assert_eq!(db.undo().unwrap(), 2);
        // ...unless it's the privacy clear, which also empties the trash.
        db.delete(pinned).unwrap();
        db.clear(false).unwrap();
        assert_eq!(db.trash_count(), 0);
        assert_eq!(db.undo().unwrap(), 0);
        drop(db);
        let _ = std::fs::remove_file(&path);
    }
}
