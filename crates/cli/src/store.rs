//! SQLite backed session registry (PLAN.md M5). Stores the full Session as JSON
//! so a session created on one run is visible to the next. This is the dev
//! adapter; the same surface can be backed by Postgres later for a hosted
//! daemon.

use std::path::Path;
use std::sync::Mutex;

use anyhow::{Context, Result};
use rusqlite::Connection;

use shepherd_core::ids::SessionId;
use shepherd_core::session::Session;

pub struct Store {
    conn: Mutex<Connection>,
}

impl Store {
    /// Open (creating if needed) the SQLite database at `path`.
    pub fn open(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).ok();
        }
        let conn = Connection::open(path).with_context(|| format!("open store at {path:?}"))?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, data TEXT NOT NULL)",
            [],
        )?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    pub fn upsert(&self, session: &Session) -> Result<()> {
        let data = serde_json::to_string(session)?;
        let conn = self.conn.lock().unwrap();
        conn.execute(
            "INSERT INTO sessions (id, data) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            (session.id.to_string(), data),
        )?;
        Ok(())
    }

    pub fn get(&self, id: &SessionId) -> Result<Option<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT data FROM sessions WHERE id = ?1")?;
        let mut rows = stmt.query([id.to_string()])?;
        match rows.next()? {
            Some(row) => {
                let data: String = row.get(0)?;
                Ok(Some(serde_json::from_str(&data)?))
            }
            None => Ok(None),
        }
    }

    pub fn list(&self) -> Result<Vec<Session>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare("SELECT data FROM sessions ORDER BY id")?;
        let rows = stmt.query_map([], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for data in rows {
            out.push(serde_json::from_str(&data?)?);
        }
        Ok(out)
    }

    pub fn delete(&self, id: &SessionId) -> Result<()> {
        let conn = self.conn.lock().unwrap();
        conn.execute("DELETE FROM sessions WHERE id = ?1", [id.to_string()])?;
        Ok(())
    }
}
