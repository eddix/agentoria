pub mod credentials;
mod events;
mod store;

pub use store::{
    list_indexed_sessions, load_conversation, load_process, load_snapshot, read_chunk,
};

use anyhow::{Context as _, Result};
use credentials::{KeyError, SessionKey};
use rusqlite::{Connection, OpenFlags};
use std::{
    path::{Path, PathBuf},
    time::Duration,
};

pub fn root() -> Result<PathBuf> {
    if let Some(path) =
        std::env::var_os("AGENTORIA_TRAE_CN_DATA_DIR").filter(|path| !path.is_empty())
    {
        return Ok(PathBuf::from(path));
    }
    Ok(dirs::data_dir()
        .context("Could not locate Application Support")?
        .join("Trae CN/ModularData"))
}

pub(super) fn open_database(path: &Path, key: &SessionKey) -> Result<Connection> {
    let database = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .context("Could not open the local Trae CN database")?;
    database
        .pragma_update(None, "key", format!("x'{}'", key.hex()))
        .map_err(|_| KeyError::Rejected)?;
    database.busy_timeout(Duration::from_millis(300))?;
    database
        .query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| match error.sqlite_error_code() {
            Some(rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt) => {
                anyhow::Error::from(KeyError::Rejected)
            }
            _ => anyhow::Error::new(error).context("Could not read the Trae CN database"),
        })?;
    database.execute_batch(
        "PRAGMA query_only=ON; PRAGMA trusted_schema=OFF; PRAGMA cache_size=-2048;",
    )?;
    Ok(database)
}

pub fn validate_key(key: &SessionKey) -> Result<()> {
    validate_key_at(&root()?, key)
}

pub fn validate_key_at(root: &Path, key: &SessionKey) -> Result<()> {
    let path = primary_database(root)?.context("No local Trae CN session database was found")?;
    open_database(&path, key)?;
    Ok(())
}

fn primary_database(root: &Path) -> Result<Option<PathBuf>> {
    for kind in ["ai-agent", "ai-chat"] {
        let path = root.join(kind).join("database.db");
        if path.try_exists()? {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

fn summary(file: &crate::index::IndexedFile) -> crate::SessionSummary {
    let mut summary = crate::SessionSummary::empty(
        file.session_id.clone().unwrap_or_default(),
        crate::SessionSource::TraeCn,
        file.path.to_string_lossy().into_owned(),
    );
    summary.title = file.title.clone().unwrap_or_else(|| summary.id.clone());
    summary.updated_at = file.updated_at.clone();
    summary.working_dir = file.working_dir.clone();
    summary.user_count = file.user_count as usize;
    summary.assistant_count = file.assistant_count as usize;
    summary.message_count = summary.user_count + summary.assistant_count;
    summary.is_partial = false;
    summary
}

pub fn list_sessions() -> Result<Vec<crate::SessionSummary>> {
    Ok(
        list_indexed_sessions(&crate::index::CancellationToken::default())?
            .iter()
            .map(summary)
            .collect(),
    )
}

pub fn read_session_detail(path: &Path) -> Result<crate::SessionDetail> {
    let file = list_indexed_sessions(&crate::index::CancellationToken::default())?
        .into_iter()
        .find(|file| file.path == path)
        .context("Trae CN session was not found in the local catalog")?;
    let (file, conversations, process) =
        load_snapshot(file, &crate::index::CancellationToken::default())?;
    let mut summary = summary(&file);
    let mut stats = crate::SessionStats::new(
        file.path.to_string_lossy().into_owned(),
        file.updated_at.clone(),
    );
    stats.started_at = file.started_at.clone();
    let mut events = Vec::new();
    for line in conversations {
        if let Some(crate::index::ConversationBody::Loaded(message)) =
            load_conversation(&line, 512 * 1024)?
        {
            let meta =
                crate::EventMeta::new(format!("trae-message-{}", line.ordinal), line.event_type)
                    .with_timestamp(line.timestamp);
            stats.content_chars += message.text.chars().count();
            match message.kind {
                crate::index::ConversationKind::User => {
                    stats.user_count += 1;
                    events.push(crate::SessionEvent::User {
                        meta,
                        text: message.text,
                    });
                }
                crate::index::ConversationKind::Assistant => {
                    stats.assistant_count += 1;
                    events.push(crate::SessionEvent::AssistantMessage {
                        meta,
                        text: message.text,
                        model: message.model,
                        stop_reason: None,
                    });
                }
            }
        } else {
            summary.is_partial = true;
        }
    }
    for line in process {
        if let crate::index::ProcessBody::Loaded(process) = load_process(&line, 512 * 1024)? {
            events.extend(process);
        } else {
            summary.is_partial = true;
        }
    }
    stats.event_count = events.len();
    stats.tool_call_count = events
        .iter()
        .filter(|event| matches!(event, crate::SessionEvent::ToolCall { .. }))
        .count();
    stats.reasoning_count = events
        .iter()
        .filter(|event| matches!(event, crate::SessionEvent::Reasoning { .. }))
        .count();
    summary.tool_call_count = stats.tool_call_count;
    Ok(crate::SessionDetail {
        summary,
        stats,
        events,
    })
}
