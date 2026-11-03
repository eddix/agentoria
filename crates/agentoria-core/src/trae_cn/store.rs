use std::{
    cell::RefCell,
    io::{Read as _, Seek as _, SeekFrom},
    path::{Path, PathBuf},
};

use anyhow::{bail, ensure, Context as _, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

use super::{credentials::SessionKey, open_database, root};
use crate::{
    index::{
        CancellationToken, ConversationBody, ConversationKind, ConversationMessage, IndexedFile,
        IndexedLine, ProcessBody,
    },
    SessionSource,
};

struct CachedReader {
    path: PathBuf,
    fingerprint: Vec<u8>,
    identity: (u64, u64),
    database: Connection,
}
thread_local! { static READERS: RefCell<Vec<CachedReader>> = const { RefCell::new(Vec::new()) }; }

fn with_reader<T>(path: &Path, callback: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let key = SessionKey::load()?;
    let metadata = std::fs::metadata(path).context("The Trae CN database was removed")?;
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt as _;
        (metadata.dev(), metadata.ino())
    };
    #[cfg(not(unix))]
    let identity = (
        metadata.len(),
        metadata
            .modified()?
            .duration_since(std::time::UNIX_EPOCH)?
            .as_nanos() as u64,
    );
    let fingerprint = Sha256::digest(key.hex().as_bytes()).to_vec();
    READERS.with(|readers| {
        let mut readers = readers.borrow_mut();
        let position = readers.iter().position(|reader| {
            reader.path == path && reader.fingerprint == fingerprint && reader.identity == identity
        });
        let index = if let Some(index) = position {
            index
        } else {
            readers.retain(|reader| reader.path != path);
            if readers.len() >= 2 {
                readers.remove(0);
            }
            let database = open_database(path, &key)?;
            readers.push(CachedReader {
                path: path.into(),
                fingerprint,
                identity,
                database,
            });
            readers.len() - 1
        };
        callback(&readers[index].database)
    })
}

pub fn list_indexed_sessions(cancellation: &CancellationToken) -> Result<Vec<IndexedFile>> {
    let root = root()?;
    // ai-agent is the current catalog; do not duplicate its migrated history
    // by merging the older ai-chat store from the same installation.
    let paths = super::primary_database(&root)?.into_iter();
    let mut files = Vec::new();
    for path in paths {
        if cancellation.is_cancelled() {
            bail!("Trae CN discovery cancelled");
        }
        if !path.try_exists()? {
            continue;
        }
        let path = path.canonicalize()?;
        files.extend(with_reader(&path, |database| catalog(database, &path))?);
    }
    files.sort_by(|left, right| {
        right
            .updated_at
            .cmp(&left.updated_at)
            .then_with(|| left.path.cmp(&right.path))
    });
    Ok(files)
}

fn has_table(database: &Connection, name: &str) -> Result<bool> {
    Ok(database
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1",
            [name],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
}

fn catalog(database: &Connection, path: &Path) -> Result<Vec<IndexedFile>> {
    if !has_table(database, "chat_session")? {
        let tables: i64 = database.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '%migration%'", [], |row| row.get(0))?;
        ensure!(tables == 0, "Unsupported Trae CN session database schema");
        return Ok(Vec::new());
    }
    let size = std::fs::metadata(path)?.len();
    let mut query = database.prepare(
        "SELECT s.id,s.session_id,s.session_title,s.created_at,s.updated_at,
         p.absolute_path,p.single_root_workspace_folder,p.workspace_folders,
         (SELECT COUNT(*) FROM chat_message m WHERE m.session_id=s.session_id AND m.deleted_at=0 AND m.message_role='user'),
         (SELECT COUNT(*) FROM chat_message m WHERE m.session_id=s.session_id AND m.deleted_at=0 AND m.message_role='assistant'),
         (SELECT COUNT(*) FROM history_v2 h WHERE h.session_id=s.session_id AND h.deleted_at=0)
         FROM chat_session s LEFT JOIN project p ON p.id=(SELECT p2.id FROM project p2 WHERE p2.project_id=s.project_id ORDER BY p2.updated_at DESC,p2.id DESC LIMIT 1)
         WHERE s.deleted_at=0 ORDER BY s.updated_at DESC,s.id DESC"
    ).context("Unsupported Trae CN session database schema")?;
    let files = query
        .query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<String>>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, u64>(8)?,
                row.get::<_, u64>(9)?,
                row.get::<_, u64>(10)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    files
        .into_iter()
        .map(
            |(
                row_id,
                session_id,
                title,
                created,
                updated,
                absolute,
                single,
                folders,
                user_count,
                assistant_count,
                process_count,
            )| {
                let kind = i64::from(
                    path.parent()
                        .and_then(Path::file_name)
                        .is_some_and(|name| name == "ai-chat"),
                );
                let id = row_id
                    .checked_mul(2)
                    .and_then(|id| id.checked_add(kind + 1))
                    .and_then(i64::checked_neg)
                    .context("Trae CN session identity is too large")?;
                ensure!(row_id > 0, "Invalid Trae CN session identity");
                Ok(IndexedFile {
                    id,
                    source: SessionSource::TraeCn,
                    path: path.join(format!("session-{row_id}")),
                    file_size: size,
                    scanned_offset: size,
                    pending_bytes: 0,
                    line_count: user_count + assistant_count + process_count,
                    user_count,
                    assistant_count,
                    process_count,
                    error_count: 0,
                    started_at: Some(timestamp(created)),
                    updated_at: timestamp(updated),
                    session_id: Some(session_id),
                    title: (!title.is_empty()).then_some(title),
                    working_dir: workdir(
                        absolute.as_deref(),
                        single.as_deref(),
                        folders.as_deref(),
                    ),
                    git_branch: None,
                    model: None,
                    parent_session_id: None,
                    is_sidechain: false,
                })
            },
        )
        .collect()
}

fn workdir(absolute: Option<&str>, single: Option<&str>, folders: Option<&str>) -> Option<String> {
    let normalize = |value: &str| -> Option<String> {
        if value.starts_with("file:") {
            return url::Url::parse(value)
                .ok()?
                .to_file_path()
                .ok()?
                .to_str()
                .map(ToOwned::to_owned);
        }
        (value.starts_with('/') || value.contains("://")).then(|| value.to_string())
    };
    for path in [single, absolute].into_iter().flatten() {
        if let Some(path) = normalize(path) {
            return Some(path);
        }
    }
    let folders: Value = serde_json::from_str(folders?).ok()?;
    for folder in folders.as_array()? {
        let uri = folder
            .as_str()
            .or_else(|| folder.get("uri").and_then(Value::as_str))
            .or_else(|| folder.get("path").and_then(Value::as_str));
        if let Some(path) = uri.and_then(normalize) {
            return Some(path);
        }
    }
    None
}

pub(super) fn timestamp(value: i64) -> String {
    let value = if value.unsigned_abs() > 100_000_000_000 {
        DateTime::<Utc>::from_timestamp_millis(value)
    } else {
        DateTime::<Utc>::from_timestamp(value, 0)
    };
    value
        .map(|value| value.to_rfc3339_opts(SecondsFormat::Millis, true))
        .unwrap_or_default()
}

fn location(path: &Path) -> Result<(&Path, i64)> {
    let row_id = path
        .file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| name.strip_prefix("session-"))
        .and_then(|id| id.parse::<i64>().ok())
        .filter(|id| *id > 0)
        .context("Invalid Trae CN session location")?;
    let database = path.parent().context("Invalid Trae CN database location")?;
    ensure!(
        database
            .file_name()
            .is_some_and(|name| name == "database.db"),
        "Invalid Trae CN database location"
    );
    Ok((database, row_id))
}

/// References are SQL row IDs and raw-message positions, never file byte offsets.
pub fn load_snapshot(
    file: IndexedFile,
    cancellation: &CancellationToken,
) -> Result<(IndexedFile, Vec<IndexedLine>, Vec<IndexedLine>)> {
    let (database, _) = location(&file.path)?;
    with_reader(database, |database| {
        snapshot(database, file.clone(), cancellation)
    })
}

const CONTENT: &str = "CASE WHEN json_type(r.value,'$.content')='array' THEN json_extract(r.value,'$.content') ELSE json_array(json_object('type','text','text',json_extract(r.value,'$.content'))) END";
// Older migrated rows have no user_message_context. Only their explicitly
// linked user_input history is eligible as a fallback, never arbitrary model
// context or tool-injected user-role messages.
// Some desktop versions encode query as a JSON string of typed text blocks.
// Decode only that complete shape; keep ordinary JSON input verbatim. Use the
// same expression for byte lengths, search, and paged reads.
const USER_TEXT: &str = "(WITH input(value) AS (SELECT COALESCE(CASE WHEN json_type(CASE WHEN json_valid(m.user_message_context) THEN m.user_message_context ELSE '{}' END,'$.query')='text'
    THEN json_extract(m.user_message_context,'$.query')
    ELSE (SELECT group_concat(json_extract(c.value,'$.text'),'')
          FROM history_v2 h JOIN json_each(CASE WHEN json_valid(h.messages) THEN h.messages ELSE '{}' END,'$.raw_messages') r
          JOIN json_each(CASE WHEN json_type(r.value,'$.content')='array' THEN json_extract(r.value,'$.content') ELSE json_array(json_object('type','text','text',json_extract(r.value,'$.content'))) END) c
          WHERE h.session_id=m.session_id AND h.message_id=m.message_id AND h.deleted_at=0 AND h.content_source='user_input'
          AND h.id=(SELECT MIN(h0.id) FROM history_v2 h0 WHERE h0.session_id=m.session_id AND h0.message_id=m.message_id AND h0.deleted_at=0 AND h0.content_source='user_input')
          AND json_extract(r.value,'$.role')='user' AND json_extract(c.value,'$.type')='text') END,'')),
    parts AS (SELECT p.type,CASE WHEN p.type='object' THEN p.value ELSE '{}' END part,CAST(p.key AS INTEGER) position
              FROM input,json_each(CASE WHEN json_valid(input.value) THEN input.value ELSE 'null' END) p)
    SELECT CASE WHEN json_type(CASE WHEN json_valid(value) THEN value ELSE 'null' END)='array'
                     AND EXISTS(SELECT 1 FROM parts)
                     AND NOT EXISTS(SELECT 1 FROM parts WHERE type<>'object' OR json_extract(part,'$.type') IS NOT 'text' OR json_type(part,'$.data.content') IS NOT 'text')
                THEN (SELECT group_concat(text,'') FROM (SELECT json_extract(part,'$.data.content') text FROM parts ORDER BY position))
                ELSE value END FROM input)";

fn snapshot(
    database: &Connection,
    mut file: IndexedFile,
    cancellation: &CancellationToken,
) -> Result<(IndexedFile, Vec<IndexedLine>, Vec<IndexedLine>)> {
    let (_, session_row) = location(&file.path)?;
    let transaction = database.unchecked_transaction()?;
    let session_id: Option<String> = transaction
        .query_row(
            "SELECT session_id FROM chat_session WHERE id=?1 AND deleted_at=0",
            [session_row],
            |row| row.get(0),
        )
        .optional()?;
    let session_id = session_id.context("This Trae CN session was removed")?;
    ensure!(
        file.session_id.as_deref() == Some(session_id.as_str()),
        "Trae CN session identity changed; refresh the catalog"
    );
    let mut ordered = Vec::new();
    let database_path = file
        .path
        .parent()
        .context("Invalid Trae CN session location")?
        .to_path_buf();
    let mut users = transaction.prepare(&format!("SELECT m.id,m.message_index,m.created_at,length(CAST({USER_TEXT} AS BLOB)) FROM chat_message m WHERE m.session_id=?1 AND m.deleted_at=0 AND m.message_role='user' ORDER BY m.message_index,m.id"))?;
    let rows = users.query_map([&session_id], |row| {
        Ok((
            row.get::<_, u64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, u64>(3)?,
        ))
    })?;
    for row in rows {
        if cancellation.is_cancelled() {
            bail!("Trae CN session load cancelled");
        }
        let (id, order, time, bytes) = row?;
        ordered.push((
            (order, 0, id, 0),
            native_line(
                &file,
                &database_path,
                id,
                bytes,
                "trae.user".into(),
                Some(ConversationKind::User),
                time,
            ),
        ));
    }
    drop(users);
    let has_runs = has_table(&transaction, "agent_run")?;
    let joins = if has_runs {
        "LEFT JOIN agent_run a ON a.agent_run_id=h.agent_run_id"
    } else {
        ""
    };
    let root_filter = if has_runs {
        "AND COALESCE(a.parent_run_id,'')=''"
    } else {
        ""
    };
    let mut assistants = transaction.prepare(&format!(
        "WITH parts AS (SELECT h.id,CAST(r.key AS INTEGER) slot,m.message_index,h.created_at,
         COALESCE((SELECT SUM(length(CAST(json_extract(c.value,'$.text') AS BLOB))) FROM json_each({CONTENT}) c WHERE json_extract(c.value,'$.type')='text'),0) bytes
         FROM history_v2 h JOIN chat_message m ON m.session_id=h.session_id AND m.message_id=h.message_id
         {joins} JOIN json_each(CASE WHEN json_valid(h.messages) THEN h.messages ELSE '{{}}' END,'$.raw_messages') r
         WHERE h.session_id=?1 AND h.deleted_at=0 AND m.deleted_at=0 AND json_extract(r.value,'$.role')='assistant' {root_filter})
         SELECT id,slot,message_index,created_at,bytes FROM parts WHERE bytes>0 ORDER BY message_index,id,slot"
    ))?;
    let rows = assistants.query_map([&session_id], |row| {
        Ok((
            row.get::<_, u64>(0)?,
            row.get::<_, u64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, u64>(4)?,
        ))
    })?;
    for row in rows {
        if cancellation.is_cancelled() {
            bail!("Trae CN session load cancelled");
        }
        let (id, slot, order, time, bytes) = row?;
        ordered.push((
            (order, 1, id, slot),
            native_line(
                &file,
                &database_path,
                id,
                bytes,
                format!("trae.assistant:{slot}"),
                Some(ConversationKind::Assistant),
                time,
            ),
        ));
    }
    drop(assistants);
    ordered.sort_by_key(|(key, _)| *key);
    let conversations = ordered
        .into_iter()
        .enumerate()
        .map(|(index, (_, mut line))| {
            line.ordinal = index as u64 + 1;
            line.line_number = line.ordinal;
            line
        })
        .collect::<Vec<_>>();
    let mut process_query=transaction.prepare("SELECT id,created_at,length(CAST(messages AS BLOB)),content_source FROM history_v2 WHERE session_id=?1 AND deleted_at=0 ORDER BY id")?;
    let mut processes = Vec::new();
    for row in process_query.query_map([&session_id], |row| {
        Ok((
            row.get::<_, u64>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, u64>(2)?,
            row.get::<_, String>(3)?,
        ))
    })? {
        if cancellation.is_cancelled() {
            bail!("Trae CN session load cancelled");
        }
        let (id, time, bytes, source) = row?;
        let mut line = native_line(
            &file,
            &database_path,
            id,
            bytes,
            format!("trae.history:{source}"),
            None,
            time,
        );
        line.ordinal = processes.len() as u64 + 1;
        line.line_number = line.ordinal;
        line.process_count = 1;
        processes.push(line);
    }
    drop(process_query);
    if cancellation.is_cancelled() {
        bail!("Trae CN session load cancelled");
    }
    transaction.commit()?;
    file.user_count = conversations
        .iter()
        .filter(|line| line.conversation_kind == Some(ConversationKind::User))
        .count() as u64;
    file.assistant_count = conversations.len() as u64 - file.user_count;
    file.process_count = processes.len() as u64;
    file.line_count = conversations.len() as u64 + file.process_count;
    Ok((file, conversations, processes))
}

fn native_line(
    file: &IndexedFile,
    path: &Path,
    id: u64,
    bytes: u64,
    event_type: String,
    kind: Option<ConversationKind>,
    time: i64,
) -> IndexedLine {
    IndexedLine {
        file_id: file.id,
        source: SessionSource::TraeCn,
        path: path.into(),
        ordinal: 0,
        line_number: 0,
        byte_offset: id,
        byte_len: bytes,
        event_type,
        timestamp: Some(timestamp(time)),
        conversation_kind: kind,
        process_count: 0,
    }
}

fn text_parts(database: &Connection, line: &IndexedLine) -> Result<Vec<(i64, i64, u64)>> {
    if line.event_type == "trae.user" {
        return Ok(vec![(0, 0, line.byte_len)]);
    }
    let slot = line
        .event_type
        .strip_prefix("trae.assistant:")
        .and_then(|slot| slot.parse::<i64>().ok())
        .context("Invalid Trae CN message reference")?;
    let mut query=database.prepare(&format!("SELECT CAST(c.key AS INTEGER),length(CAST(json_extract(c.value,'$.text') AS BLOB)) FROM history_v2 h JOIN json_each(h.messages,'$.raw_messages') r JOIN json_each({CONTENT}) c WHERE h.id=?1 AND h.deleted_at=0 AND CAST(r.key AS INTEGER)=?2 AND json_extract(r.value,'$.role')='assistant' AND json_extract(c.value,'$.type')='text' AND json_type(c.value,'$.text')='text' ORDER BY CAST(c.key AS INTEGER)"))?;
    let parts = query
        .query_map(params![line.byte_offset, slot], |row| {
            Ok((slot, row.get(0)?, row.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(parts)
}

fn read_text_chunk(
    database: &Connection,
    line: &IndexedLine,
    offset: u64,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    if max_bytes == 0 {
        return Ok(Vec::new());
    }
    if line.event_type == "trae.user" {
        return database.query_row(&format!("SELECT COALESCE(substr(CAST({USER_TEXT} AS BLOB),?2,?3),X'') FROM chat_message m WHERE m.id=?1 AND m.deleted_at=0 AND m.message_role='user'"),params![line.byte_offset,offset.saturating_add(1),max_bytes],|row|row.get(0)).context("This Trae CN user message was removed");
    }
    let mut skip = offset;
    let mut output = Vec::new();
    let mut query=database.prepare(&format!("SELECT substr(CAST(json_extract(c.value,'$.text') AS BLOB),?4,?5) FROM history_v2 h JOIN json_each(h.messages,'$.raw_messages') r JOIN json_each({CONTENT}) c WHERE h.id=?1 AND h.deleted_at=0 AND CAST(r.key AS INTEGER)=?2 AND CAST(c.key AS INTEGER)=?3 AND json_extract(r.value,'$.role')='assistant'"))?;
    for (slot, part, len) in text_parts(database, line)? {
        if skip >= len {
            skip -= len;
            continue;
        }
        let bytes: Vec<u8> = query.query_row(
            params![
                line.byte_offset,
                slot,
                part,
                skip + 1,
                max_bytes - output.len()
            ],
            |row| row.get(0),
        )?;
        output.extend(bytes);
        skip = 0;
        if output.len() >= max_bytes {
            break;
        }
    }
    Ok(output)
}

pub fn load_conversation(line: &IndexedLine, max_bytes: u64) -> Result<Option<ConversationBody>> {
    let Some(kind) = line.conversation_kind else {
        return Ok(None);
    };
    if line.byte_len > max_bytes {
        return Ok(Some(ConversationBody::Oversized {
            kind,
            source_bytes: line.byte_len,
        }));
    }
    with_reader(&line.path, |database| {
        let transaction = database.unchecked_transaction()?;
        let bytes = read_text_chunk(
            &transaction,
            line,
            0,
            usize::try_from(max_bytes)?.saturating_add(1),
        )?;
        if bytes.len() as u64 > max_bytes {
            return Ok(Some(ConversationBody::Oversized {
                kind,
                source_bytes: bytes.len() as u64,
            }));
        }
        let text = String::from_utf8(bytes).context("Trae CN message contains invalid UTF-8")?;
        transaction.commit()?;
        Ok(Some(ConversationBody::Loaded(ConversationMessage {
            kind,
            text,
            model: None,
            stop_reason: None,
            source_bytes: line.byte_len,
        })))
    })
}

pub fn read_chunk(line: &IndexedLine, offset: u64, max_bytes: usize) -> Result<Vec<u8>> {
    if offset >= line.byte_len || max_bytes == 0 {
        return Ok(Vec::new());
    }
    let max_bytes = (line.byte_len - offset).min(max_bytes as u64) as usize;
    with_reader(&line.path, |database| {
        if line.conversation_kind.is_some() {
            return read_text_chunk(database, line, offset, max_bytes);
        }
        let present: bool = database.query_row(
            "SELECT EXISTS(SELECT 1 FROM history_v2 WHERE id=?1 AND deleted_at=0)",
            [line.byte_offset],
            |row| row.get(0),
        )?;
        ensure!(present, "This Trae CN process record was removed");
        let mut blob = database.blob_open(
            rusqlite::DatabaseName::Main,
            "history_v2",
            "messages",
            i64::try_from(line.byte_offset)?,
            true,
        )?;
        blob.seek(SeekFrom::Start(offset))?;
        let mut bytes = Vec::new();
        blob.take(max_bytes as u64).read_to_end(&mut bytes)?;
        Ok(bytes)
    })
}

pub fn load_process(line: &IndexedLine, max_bytes: u64) -> Result<ProcessBody> {
    if line.byte_len > max_bytes {
        return Ok(ProcessBody::Oversized {
            source_bytes: line.byte_len,
        });
    }
    let bytes = read_chunk(line, 0, usize::try_from(max_bytes)?.saturating_add(1))?;
    if bytes.len() as u64 > max_bytes {
        return Ok(ProcessBody::Oversized {
            source_bytes: bytes.len() as u64,
        });
    }
    let value: Value =
        serde_json::from_slice(&bytes).context("Could not decode the Trae CN process record")?;
    Ok(ProcessBody::Loaded(super::events::process_events(
        &value, line,
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SessionEvent;
    use serde_json::json;

    const SCHEMA:&str="
        CREATE TABLE project(id INTEGER PRIMARY KEY,project_id TEXT,absolute_path TEXT,single_root_workspace_folder TEXT,workspace_folders TEXT,updated_at INTEGER);
        CREATE TABLE chat_session(id INTEGER PRIMARY KEY,session_id TEXT,session_title TEXT,project_id TEXT,created_at INTEGER,updated_at INTEGER,deleted_at INTEGER DEFAULT 0);
        CREATE TABLE chat_message(id INTEGER PRIMARY KEY,session_id TEXT,message_id TEXT,message_role TEXT,message_index INTEGER,created_at INTEGER,user_message_context TEXT,deleted_at INTEGER DEFAULT 0,is_archived INTEGER DEFAULT 0);
        CREATE TABLE history_v2(id INTEGER PRIMARY KEY,session_id TEXT,message_id TEXT,messages TEXT,content_source TEXT,created_at INTEGER,deleted_at INTEGER DEFAULT 0,agent_run_id TEXT);
        CREATE TABLE agent_run(agent_run_id TEXT PRIMARY KEY,parent_run_id TEXT);
        CREATE INDEX history_session ON history_v2(session_id);
        INSERT INTO project VALUES(1,'p',NULL,NULL,'[\"file:///work/project%20space\"]',1000);
        INSERT INTO chat_session VALUES(1,'s','Example','p',1000,1001,0);
        INSERT INTO agent_run VALUES('root',NULL),('child','root');";

    fn fixture() -> (tempfile::TempDir, PathBuf, Connection, SessionKey) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("ai-agent/database.db");
        std::fs::create_dir(path.parent().unwrap()).unwrap();
        let key = SessionKey::parse(&"1a".repeat(32)).unwrap();
        let database = Connection::open(&path).unwrap();
        database
            .pragma_update(None, "key", format!("x'{}'", key.hex()))
            .unwrap();
        database.execute_batch("PRAGMA journal_mode=WAL;").unwrap();
        database.execute_batch(SCHEMA).unwrap();
        for (id, role, text, archived, deleted) in [
            (1, "user", "Original 用户", 0, 0),
            (2, "assistant", "", 0, 0),
            (3, "user", "Archived original", 1, 0),
            (4, "user", "Deleted", 0, 1),
        ] {
            database
                .execute(
                    "INSERT INTO chat_message VALUES(?1,'s',?2,?3,?1,1000,?4,?5,?6)",
                    params![
                        id,
                        format!("m{id}"),
                        role,
                        json!({"query":text}).to_string(),
                        deleted,
                        archived
                    ],
                )
                .unwrap();
        }
        let records = [
            (
                1,
                "m1",
                "root",
                json!({"raw_messages":[{"role":"user","content":[{"type":"text","text":"Injected model context"}]}]}),
            ),
            (
                2,
                "m1",
                "root",
                json!({"raw_messages":[{"role":"assistant","content":[{"type":"text","text":"Hel"},{"type":"text","text":"lo"}],"tool_calls":[{"id":"call","function_call":{"name":"Shell","arguments":"{\"command\":\"true\"}"}}]},{"role":"tool","tool_call_id":"call","content":[{"type":"text","text":"result"}]}]}),
            ),
            (
                3,
                "m1",
                "child",
                json!({"raw_messages":[{"role":"assistant","content":[{"type":"text","text":"Internal child reply"}]}]}),
            ),
            (
                4,
                "m2",
                "root",
                json!({"raw_messages":[{"role":"assistant","content":[{"type":"text","text":"Final **answer**"}]}]}),
            ),
        ];
        for (id, message, run, value) in records {
            database
                .execute(
                    "INSERT INTO history_v2 VALUES(?1,'s',?2,?3,'fixture',1000,0,?4)",
                    params![id, message, value.to_string(), run],
                )
                .unwrap();
        }
        (directory, path, database, key)
    }

    #[test]
    fn encrypted_catalog_and_lazy_snapshot_preserve_original_users_and_exclude_child_replies() {
        let (_directory, path, writer, key) = fixture();
        let before = std::fs::read(&path).unwrap();
        let wal_path = PathBuf::from(format!("{}-wal", path.display()));
        let wal_before = std::fs::read(&wal_path).unwrap();
        let database = open_database(&path, &key).unwrap();
        let files = catalog(&database, &path).unwrap();
        assert_eq!(files.len(), 1);
        assert!(files[0].id < 0);
        assert_eq!(files[0].working_dir.as_deref(), Some("/work/project space"));
        let (file, lines, process) =
            snapshot(&database, files[0].clone(), &CancellationToken::default()).unwrap();
        assert_eq!(
            (file.user_count, file.assistant_count, process.len()),
            (2, 2, 4)
        );
        let bodies = lines
            .iter()
            .map(|line| {
                String::from_utf8(read_text_chunk(&database, line, 0, 512 * 1024).unwrap()).unwrap()
            })
            .collect::<Vec<_>>();
        assert_eq!(
            bodies,
            [
                "Original 用户",
                "Hello",
                "Final **answer**",
                "Archived original"
            ]
        );
        assert!(!bodies.join("\n").contains("Injected"));
        assert!(!bodies.join("\n").contains("Internal child"));
        assert_eq!(read_text_chunk(&database, &lines[1], 2, 2).unwrap(), b"ll");
        assert!(
            crate::index::read_line_chunk(&process[0], process[0].byte_len, 64)
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            load_conversation(&lines[2], 3).unwrap(),
            Some(ConversationBody::Oversized { .. })
        ));
        assert!(database.execute("DELETE FROM chat_session", []).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        assert_eq!(std::fs::read(&wal_path).unwrap(), wal_before);
        let value: String = writer
            .query_row("SELECT messages FROM history_v2 WHERE id=2", [], |row| {
                row.get(0)
            })
            .unwrap();
        let events = super::super::events::process_events(
            &serde_json::from_str(&value).unwrap(),
            &process[1],
        );
        assert!(events
            .iter()
            .any(|event| matches!(event,SessionEvent::ToolCall{name,..} if name=="Shell")));
        assert!(events.iter().any(
            |event| matches!(event,SessionEvent::ToolResult{content,..} if content=="result")
        ));
    }

    #[test]
    fn migrated_user_rows_only_fall_back_to_their_own_user_input_history() {
        let (_directory, path, database, key) = fixture();
        for (id, context, source, text) in [
            (5, "", "user_input", "Legacy saved input"),
            (6, "{}", "Shell", "Injected tool context"),
            (
                7,
                "{\"query\":\"\"}",
                "user_input",
                "Expanded model context",
            ),
        ] {
            database
                .execute(
                    "INSERT INTO chat_message VALUES(?1,'s',?2,'user',?1,1001,?3,0,0)",
                    params![id, format!("m{id}"), context],
                )
                .unwrap();
            let payload =
                json!({"raw_messages":[{"role":"user","content":[{"type":"text","text":text}]}]})
                    .to_string();
            database
                .execute(
                    "INSERT INTO history_v2 VALUES(?1,'s',?2,?3,?4,1001,0,'root')",
                    params![id, format!("m{id}"), payload, source],
                )
                .unwrap();
        }
        let reader = open_database(&path, &key).unwrap();
        let (_, lines, _) = snapshot(
            &reader,
            catalog(&reader, &path).unwrap().remove(0),
            &CancellationToken::default(),
        )
        .unwrap();
        for (id, expected) in [(5, "Legacy saved input"), (6, ""), (7, "")] {
            let line = lines
                .iter()
                .find(|line| line.event_type == "trae.user" && line.byte_offset == id)
                .unwrap();
            assert_eq!(
                read_text_chunk(&reader, line, 0, 64 * 1024).unwrap(),
                expected.as_bytes()
            );
        }
    }

    #[test]
    fn encoded_user_text_blocks_share_normalized_lengths_and_paged_utf8_reads() {
        let (_directory, path, database, key) = fixture();
        let encoded = json!([
            {"type":"text","data":{"content":"Hello 用户\n"}},
            {"type":"text","data":{"content":"**world**"}}
        ])
        .to_string();
        let plain_json = r#"[{"type":"text","text":"Keep literal JSON"}]"#;
        for (id, query) in [
            (5, encoded.as_str()),
            (6, plain_json),
            (7, "[1,\"text\"]"),
            (8, "[]"),
        ] {
            database
                .execute(
                    "INSERT INTO chat_message VALUES(?1,'s',?2,'user',?1,1001,?3,0,0)",
                    params![id, format!("m{id}"), json!({"query":query}).to_string()],
                )
                .unwrap();
        }
        let reader = open_database(&path, &key).unwrap();
        let (_, lines, _) = snapshot(
            &reader,
            catalog(&reader, &path).unwrap().remove(0),
            &CancellationToken::default(),
        )
        .unwrap();
        for (id, expected) in [
            (5, "Hello 用户\n**world**"),
            (6, plain_json),
            (7, "[1,\"text\"]"),
            (8, "[]"),
        ] {
            let line = lines
                .iter()
                .find(|line| line.event_type == "trae.user" && line.byte_offset == id)
                .unwrap();
            assert_eq!(line.byte_len, expected.len() as u64);
            assert_eq!(
                read_text_chunk(&reader, line, 0, 64 * 1024).unwrap(),
                expected.as_bytes()
            );
            assert_eq!(
                read_text_chunk(&reader, line, 2, 7).unwrap(),
                expected.as_bytes()[2..9.min(expected.len())]
            );
        }
    }

    #[test]
    fn wrong_key_is_redacted_and_readonly_connections_observe_wal_updates_and_deletions() {
        let (_directory, path, writer, key) = fixture();
        let wrong = SessionKey::parse(&"2b".repeat(32)).unwrap();
        let error = open_database(&path, &wrong).err().unwrap();
        assert_eq!(
            error.downcast_ref::<super::super::credentials::KeyError>(),
            Some(&super::super::credentials::KeyError::Rejected)
        );
        assert!(!format!("{error:?}").contains(wrong.hex()));
        let database = open_database(&path, &key).unwrap();
        assert_eq!(catalog(&database, &path).unwrap().len(), 1);
        writer
            .execute(
                "INSERT INTO chat_session VALUES(2,'s2','New','p',1002,1002,0)",
                [],
            )
            .unwrap();
        let files = catalog(&database, &path).unwrap();
        assert_eq!(files.len(), 2);
        assert_ne!(files[0].path, files[1].path);
        writer
            .execute("UPDATE chat_session SET deleted_at=1003 WHERE id=1", [])
            .unwrap();
        assert_eq!(catalog(&database, &path).unwrap().len(), 1);
        let cancelled = CancellationToken::default();
        cancelled.cancel();
        assert!(snapshot(
            &database,
            files
                .into_iter()
                .find(|file| file.session_id.as_deref() == Some("s2"))
                .unwrap(),
            &cancelled
        )
        .is_err());
    }
}
