use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use rusqlite::{Connection, Error, ErrorCode};

const SCHEMA_VERSION: i64 = 2;

pub(super) fn open_database(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create index directory {}", parent.display()))?;
    }

    match try_open(path) {
        Ok(connection) => Ok(connection),
        Err(error) if path.exists() && !is_busy(&error) => {
            archive_database(path)?;
            try_open(path).with_context(|| {
                format!(
                    "failed to recreate Agentoria index after archiving {}",
                    path.display()
                )
            })
        }
        Err(error) => Err(error),
    }
}

pub(super) fn open_memory_database() -> Result<Connection> {
    let connection = Connection::open_in_memory().context("failed to open in-memory index")?;
    configure(&connection)?;
    initialize(&connection)?;
    Ok(connection)
}

fn try_open(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("failed to open Agentoria index {}", path.display()))?;
    configure(&connection)?;
    initialize(&connection)?;
    Ok(connection)
}

fn configure(connection: &Connection) -> Result<()> {
    connection
        .busy_timeout(Duration::from_secs(5))
        .context("failed to configure SQLite busy timeout")?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable SQLite foreign keys")?;
    connection
        .pragma_update(None, "journal_mode", "WAL")
        .context("failed to enable SQLite WAL mode")?;
    connection
        .pragma_update(None, "synchronous", "NORMAL")
        .context("failed to configure SQLite synchronization")?;
    Ok(())
}

fn initialize(connection: &Connection) -> Result<()> {
    let integrity: String = connection
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .context("failed to validate Agentoria index")?;
    if integrity != "ok" {
        bail!("Agentoria index failed quick_check: {integrity}");
    }

    let version: i64 = connection
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .context("failed to read Agentoria index schema version")?;
    match version {
        0 => create_schema(connection),
        SCHEMA_VERSION => Ok(()),
        _ => {
            bail!("unsupported Agentoria index schema version {version}; expected {SCHEMA_VERSION}")
        }
    }
}

fn create_schema(connection: &Connection) -> Result<()> {
    connection
        .execute_batch(
            r#"
            BEGIN IMMEDIATE;

            CREATE TABLE files (
                id INTEGER PRIMARY KEY,
                source TEXT NOT NULL,
                path TEXT NOT NULL,
                device INTEGER NOT NULL,
                inode INTEGER NOT NULL,
                file_size INTEGER NOT NULL,
                modified_ns INTEGER NOT NULL,
                scanned_offset INTEGER NOT NULL,
                prefix_len INTEGER NOT NULL,
                prefix_hash BLOB NOT NULL,
                boundary_len INTEGER NOT NULL,
                boundary_hash BLOB NOT NULL,
                line_count INTEGER NOT NULL DEFAULT 0,
                user_count INTEGER NOT NULL DEFAULT 0,
                assistant_count INTEGER NOT NULL DEFAULT 0,
                process_count INTEGER NOT NULL DEFAULT 0,
                error_count INTEGER NOT NULL DEFAULT 0,
                started_at TEXT,
                updated_at TEXT NOT NULL DEFAULT '',
                session_id TEXT,
                title TEXT,
                working_dir TEXT,
                git_branch TEXT,
                model TEXT,
                parent_session_id TEXT,
                is_sidechain INTEGER NOT NULL DEFAULT 0,
                last_record_was_assistant INTEGER NOT NULL DEFAULT 0,
                last_assistant_hash BLOB,
                last_assistant_source TEXT,
                indexed_at_ns INTEGER NOT NULL,
                UNIQUE(source, path)
            );

            CREATE TABLE lines (
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                ordinal INTEGER NOT NULL,
                line_number INTEGER NOT NULL,
                byte_offset INTEGER NOT NULL,
                byte_len INTEGER NOT NULL,
                event_type TEXT NOT NULL,
                timestamp TEXT,
                conversation_kind INTEGER,
                conversation_suppressed INTEGER NOT NULL DEFAULT 0,
                process_count INTEGER NOT NULL DEFAULT 0,
                message_key TEXT,
                message_hash BLOB,
                assistant_source TEXT,
                PRIMARY KEY(file_id, ordinal)
            );

            CREATE INDEX lines_conversation
                ON lines(file_id, conversation_kind, conversation_suppressed, ordinal);
            CREATE INDEX lines_process
                ON lines(file_id, process_count, ordinal);
            CREATE INDEX lines_message_key
                ON lines(file_id, message_key)
                WHERE message_key IS NOT NULL;

            CREATE TABLE index_errors (
                file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
                line_number INTEGER NOT NULL,
                byte_offset INTEGER NOT NULL,
                message TEXT NOT NULL,
                PRIMARY KEY(file_id, line_number)
            );

            PRAGMA user_version = 2;
            COMMIT;
            "#,
        )
        .context("failed to initialize Agentoria index schema")?;
    Ok(())
}

fn is_busy(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause.downcast_ref::<Error>().is_some_and(|error| {
            matches!(
                error,
                Error::SqliteFailure(inner, _)
                    if matches!(inner.code, ErrorCode::DatabaseBusy | ErrorCode::DatabaseLocked)
            )
        })
    })
}

fn archive_database(path: &Path) -> Result<PathBuf> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let archive = path.with_extension(format!("corrupt-{stamp}.sqlite"));
    fs::rename(path, &archive).with_context(|| {
        format!(
            "failed to archive corrupt Agentoria index {}",
            path.display()
        )
    })?;

    for suffix in ["-wal", "-shm"] {
        let sidecar = sidecar_path(path, suffix);
        if sidecar.exists() {
            let archived_sidecar = sidecar_path(&archive, suffix);
            fs::rename(&sidecar, &archived_sidecar).with_context(|| {
                format!("failed to archive SQLite sidecar {}", sidecar.display())
            })?;
        }
    }

    Ok(archive)
}

fn sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    PathBuf::from(format!("{}{}", path.to_string_lossy(), suffix))
}
