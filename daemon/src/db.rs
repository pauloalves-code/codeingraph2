//! Thin wrapper around rusqlite with a tiny hand-rolled connection pool.
//!
//! We don't need r2d2 here — the daemon is single-writer, and readers (MCP
//! server) open their own connection against the same WAL-backed DB file.

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;
use std::path::Path;
use std::sync::{Arc, Mutex};

use crate::config::Config;

/// One shared writer connection protected by a Mutex. All indexer code grabs
/// this lock briefly to upsert rows; MCP readers open their own Connection.
#[derive(Clone)]
pub struct Pool {
    inner: Arc<Mutex<Connection>>,
    pub path: std::path::PathBuf,
}

impl Pool {
    pub fn with_conn<R>(&self, f: impl FnOnce(&Connection) -> Result<R>) -> Result<R> {
        let guard = self.inner.lock().map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))?;
        f(&guard)
    }
    pub fn with_conn_mut<R>(&self, f: impl FnOnce(&mut Connection) -> Result<R>) -> Result<R> {
        let mut guard = self.inner.lock().map_err(|e| anyhow::anyhow!("db lock poisoned: {e}"))?;
        f(&mut guard)
    }
}

pub fn open(cfg: &Config) -> Result<Pool> {
    if let Some(parent) = cfg.db_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let conn = Connection::open(&cfg.db_path)
        .with_context(|| format!("opening sqlite at {}", cfg.db_path.display()))?;
    conn.pragma_update(None, "journal_mode", "WAL")?;
    conn.pragma_update(None, "synchronous",  "NORMAL")?;
    conn.pragma_update(None, "foreign_keys", "ON")?;

    apply_migrations(&conn, &cfg.migrations_dir)
        .with_context(|| format!("applying migrations from {}", cfg.migrations_dir.display()))?;

    Ok(Pool { inner: Arc::new(Mutex::new(conn)), path: cfg.db_path.clone() })
}

/// Open a dedicated read-only connection. Used by mcp_server.
pub fn open_readonly(path: &Path) -> Result<Connection> {
    let conn = Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
            | rusqlite::OpenFlags::SQLITE_OPEN_URI,
    )?;
    conn.pragma_update(None, "query_only", "ON")?;
    Ok(conn)
}

fn apply_migrations(conn: &Connection, dir: &Path) -> Result<()> {
    if !dir.exists() {
        tracing::warn!(dir = %dir.display(), "migrations dir missing — skipping");
        return Ok(());
    }
    let mut entries: Vec<_> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|x| x == "sql").unwrap_or(false))
        .collect();
    entries.sort_by_key(|e| e.path());
    for e in entries {
        let sql = std::fs::read_to_string(e.path())?;
        tracing::debug!(file = %e.path().display(), "applying migration");
        conn.execute_batch(&sql)?;
    }
    Ok(())
}

pub fn health(pool: &Pool) -> Result<()> {
    pool.with_conn(|c| {
        let v: String = c.query_row(
            "SELECT value FROM schema_meta WHERE key = 'version'",
            [], |r| r.get(0),
        )?;
        anyhow::ensure!(!v.is_empty(), "schema_meta.version empty");
        Ok(())
    })
}

#[derive(Serialize)]
pub struct Stats {
    pub files: i64,
    pub symbols: i64,
    pub relations: i64,
    pub db_path: String,
}

pub fn stats(pool: &Pool) -> Result<Stats> {
    pool.with_conn(|c| {
        let files:     i64 = c.query_row("SELECT COUNT(*) FROM files",     [], |r| r.get(0))?;
        let symbols:   i64 = c.query_row("SELECT COUNT(*) FROM symbols",   [], |r| r.get(0))?;
        let relations: i64 = c.query_row("SELECT COUNT(*) FROM relations",[], |r| r.get(0))?;
        Ok(Stats {
            files, symbols, relations,
            db_path: pool.path.to_string_lossy().to_string(),
        })
    })
}
