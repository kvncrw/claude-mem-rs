//! SQLite data access layer.
//!
//! Wraps `rusqlite::Connection` with the claude-mem schema.

pub mod migrations;
pub mod observations;
pub mod pending_messages;
pub mod prompts;
pub mod sessions;
pub mod summaries;
pub mod timeline;
pub mod transactions;

use rusqlite::{Connection, Result};
use std::path::Path;

/// Open a connection to an existing claude-mem database (read/write).
pub fn open<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;",
    )?;
    Ok(conn)
}

/// Open a database file and apply all migrations.
pub fn open_or_create<P: AsRef<Path>>(path: P) -> Result<Connection> {
    let conn = open(path)?;
    migrations::apply_all(&conn)?;
    Ok(conn)
}

/// Open an in-memory database with the full schema applied.
/// Used by tests, and also as the "fresh install" path.
pub fn open_in_memory() -> Result<Connection> {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = MEMORY;",
    )?;
    migrations::apply_all(&conn)?;
    Ok(conn)
}
