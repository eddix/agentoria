mod classify;
mod schema;
pub mod search;

use std::{
    fs::{self, File, Metadata},
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use rusqlite::{params, Connection, OptionalExtension, Transaction, TransactionBehavior};
use serde_json::Value;
use sha2::{Digest, Sha256};

use self::classify::{
    classify_line, extract_conversation_text, extract_process_events, LineDescriptor,
};
use crate::{SessionEvent, SessionSource};

const FINGERPRINT_BYTES: u64 = 4096;
const PROGRESS_BYTE_INTERVAL: u64 = 1024 * 1024;
const PROGRESS_LINE_INTERVAL: u64 = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i64)]
pub enum ConversationKind {
    User = 1,
    Assistant = 2,
}

impl ConversationKind {
    fn from_sql(value: i64) -> rusqlite::Result<Self> {
        match value {
            1 => Ok(Self::User),
            2 => Ok(Self::Assistant),
            _ => Err(rusqlite::Error::IntegralValueOutOfRange(0, value)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedFile {
    pub id: i64,
    pub source: SessionSource,
    pub path: PathBuf,
    pub file_size: u64,
    pub scanned_offset: u64,
    pub pending_bytes: u64,
    pub line_count: u64,
    pub user_count: u64,
    pub assistant_count: u64,
    pub process_count: u64,
    pub error_count: u64,
    pub started_at: Option<String>,
    pub updated_at: String,
    pub session_id: Option<String>,
    pub title: Option<String>,
    pub working_dir: Option<String>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub parent_session_id: Option<String>,
    pub is_sidechain: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IndexedLine {
    pub file_id: i64,
    pub source: SessionSource,
    pub path: PathBuf,
    pub ordinal: u64,
    pub line_number: u64,
    pub byte_offset: u64,
    pub byte_len: u64,
    pub event_type: String,
    pub timestamp: Option<String>,
    pub conversation_kind: Option<ConversationKind>,
    pub process_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationMessage {
    pub kind: ConversationKind,
    pub text: String,
    pub model: Option<String>,
    pub stop_reason: Option<String>,
    pub source_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConversationBody {
    Loaded(ConversationMessage),
    Oversized {
        kind: ConversationKind,
        source_bytes: u64,
    },
}

#[derive(Debug, Clone)]
pub enum ProcessBody {
    Loaded(Vec<SessionEvent>),
    Oversized { source_bytes: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScanStatus {
    Unchanged,
    Appended,
    Rebuilt,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanProgress {
    pub bytes_scanned: u64,
    pub total_bytes: u64,
    pub complete_lines: u64,
    pub error_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanOutcome {
    pub status: ScanStatus,
    pub file: IndexedFile,
    pub progress: ScanProgress,
}

#[derive(Debug, Clone, Default)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }
}

pub struct IndexDatabase {
    connection: Connection,
}

impl IndexDatabase {
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self {
            connection: schema::open_database(path.as_ref())?,
        })
    }

    pub fn open_in_memory() -> Result<Self> {
        Ok(Self {
            connection: schema::open_memory_database()?,
        })
    }

    pub fn scan_file(
        &mut self,
        source: SessionSource,
        path: impl AsRef<Path>,
        cancellation: &CancellationToken,
        on_progress: impl FnMut(ScanProgress),
    ) -> Result<ScanOutcome> {
        let canonical = path
            .as_ref()
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {}", path.as_ref().display()))?;
        let initial_metadata = fs::metadata(&canonical)
            .with_context(|| format!("failed to read metadata for {}", canonical.display()))?;
        if !initial_metadata.is_file() {
            bail!(
                "session path is not a regular file: {}",
                canonical.display()
            );
        }

        let identity = FileIdentity::from_metadata(&initial_metadata)?;
        let stored = self.stored_file(source, &canonical)?;
        let mode = scan_mode(&canonical, identity, stored.as_ref())?;
        if mode == ScanStatus::Unchanged {
            if source == SessionSource::Grok {
                let transaction = self
                    .connection
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                refresh_grok_metadata(
                    &transaction,
                    stored
                        .as_ref()
                        .context("unchanged session requires stored metadata")?
                        .id,
                    &canonical,
                )?;
                if cancellation.is_cancelled() {
                    bail!("session index scan cancelled");
                }
                transaction.commit()?;
            }
            let file = self
                .indexed_file(source, &canonical)?
                .context("unchanged index entry disappeared")?;
            return Ok(ScanOutcome {
                status: ScanStatus::Unchanged,
                progress: ScanProgress {
                    bytes_scanned: file.scanned_offset,
                    total_bytes: file.file_size,
                    complete_lines: file.line_count,
                    error_count: file.error_count,
                },
                file,
            });
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .context("failed to start session index transaction")?;
        let file_id = ensure_file_row(&transaction, source, &canonical, identity)?;
        let state = prepare_scan(&transaction, file_id, mode, stored.as_ref())?;
        let progress = scan_lines(
            &transaction,
            file_id,
            source,
            &canonical,
            identity,
            state,
            cancellation,
            on_progress,
        )?;
        if source == SessionSource::Grok {
            refresh_grok_metadata(&transaction, file_id, &canonical)?;
        }
        if cancellation.is_cancelled() {
            bail!("session index scan cancelled");
        }
        transaction
            .commit()
            .context("failed to commit session index transaction")?;

        let file = self
            .indexed_file(source, &canonical)?
            .context("indexed file disappeared after commit")?;
        Ok(ScanOutcome {
            status: mode,
            file,
            progress,
        })
    }

    pub fn indexed_file(
        &self,
        source: SessionSource,
        path: impl AsRef<Path>,
    ) -> Result<Option<IndexedFile>> {
        let path = path.as_ref().to_string_lossy();
        self.connection
            .query_row(
                r#"
                SELECT id, source, path, file_size, scanned_offset, line_count,
                       user_count, assistant_count, process_count, error_count,
                       started_at, updated_at, session_id, title, working_dir,
                       git_branch, model, parent_session_id, is_sidechain
                FROM files
                WHERE source = ?1 AND path = ?2
                "#,
                params![source_name(source), path.as_ref()],
                map_indexed_file,
            )
            .optional()
            .context("failed to query indexed session")
    }

    pub fn list_files(&self, source: SessionSource) -> Result<Vec<IndexedFile>> {
        let mut statement = self
            .connection
            .prepare(
                r#"
                SELECT id, source, path, file_size, scanned_offset, line_count,
                       user_count, assistant_count, process_count, error_count,
                       started_at, updated_at, session_id, title, working_dir,
                       git_branch, model, parent_session_id, is_sidechain
                FROM files
                WHERE source = ?1
                ORDER BY updated_at DESC, path ASC
                "#,
            )
            .context("failed to prepare indexed session list")?;
        let files = statement
            .query_map([source_name(source)], map_indexed_file)
            .context("failed to list indexed sessions")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to decode indexed sessions")?;
        Ok(files)
    }

    pub fn conversation_lines(&self, file_id: i64) -> Result<Vec<IndexedLine>> {
        self.query_lines(
            file_id,
            "l.conversation_kind IS NOT NULL AND l.conversation_suppressed = 0",
        )
    }

    pub fn process_lines(&self, file_id: i64) -> Result<Vec<IndexedLine>> {
        self.query_lines(file_id, "l.process_count > 0")
    }

    pub fn set_title(&mut self, file_id: i64, title: Option<&str>) -> Result<()> {
        self.connection
            .execute(
                "UPDATE files SET title = ?2 WHERE id = ?1",
                params![file_id, title],
            )
            .context("failed to update indexed session title")?;
        Ok(())
    }

    pub fn load_conversation(
        &self,
        line: &IndexedLine,
        max_source_bytes: u64,
    ) -> Result<Option<ConversationBody>> {
        load_conversation_line(line, max_source_bytes)
    }

    pub fn raw_preview(
        &self,
        line: &IndexedLine,
        head_bytes: usize,
        tail_bytes: usize,
    ) -> Result<(Vec<u8>, Vec<u8>)> {
        let mut file = File::open(&line.path)
            .with_context(|| format!("failed to open {}", line.path.display()))?;
        let head_len = usize::try_from(line.byte_len.min(head_bytes as u64))?;
        file.seek(SeekFrom::Start(line.byte_offset))?;
        let mut head = vec![0; head_len];
        file.read_exact(&mut head)?;

        let remaining = line.byte_len.saturating_sub(head_len as u64);
        let tail_len = usize::try_from(remaining.min(tail_bytes as u64))?;
        let tail_offset = line.byte_offset + line.byte_len - tail_len as u64;
        file.seek(SeekFrom::Start(tail_offset))?;
        let mut tail = vec![0; tail_len];
        file.read_exact(&mut tail)?;
        Ok((head, tail))
    }

    fn stored_file(&self, source: SessionSource, path: &Path) -> Result<Option<StoredFile>> {
        self.connection
            .query_row(
                r#"
                SELECT id, device, inode, file_size, modified_ns, scanned_offset,
                       prefix_len, prefix_hash, boundary_len, boundary_hash,
                       line_count, session_id, title, working_dir, git_branch,
                       model, parent_session_id, is_sidechain,
                       last_record_was_assistant,
                       last_assistant_hash, last_assistant_source
                FROM files
                WHERE source = ?1 AND path = ?2
                "#,
                params![source_name(source), path.to_string_lossy().as_ref()],
                |row| {
                    Ok(StoredFile {
                        id: row.get(0)?,
                        identity: FileIdentity {
                            device: sql_u64(row.get(1)?, 1)?,
                            inode: sql_u64(row.get(2)?, 2)?,
                            size: sql_u64(row.get(3)?, 3)?,
                            modified_ns: row.get(4)?,
                        },
                        scanned_offset: sql_u64(row.get(5)?, 5)?,
                        prefix_len: sql_u64(row.get(6)?, 6)?,
                        prefix_hash: row.get(7)?,
                        boundary_len: sql_u64(row.get(8)?, 8)?,
                        boundary_hash: row.get(9)?,
                        line_count: sql_u64(row.get(10)?, 10)?,
                        session_id: row.get(11)?,
                        title: row.get(12)?,
                        working_dir: row.get(13)?,
                        git_branch: row.get(14)?,
                        model: row.get(15)?,
                        parent_session_id: row.get(16)?,
                        is_sidechain: row.get(17)?,
                        previous_assistant: if row.get::<_, bool>(18)? {
                            Some(PreviousAssistant {
                                hash: row.get(19)?,
                                source: row.get(20)?,
                            })
                        } else {
                            None
                        },
                    })
                },
            )
            .optional()
            .context("failed to query stored file identity")
    }

    fn query_lines(&self, file_id: i64, predicate: &str) -> Result<Vec<IndexedLine>> {
        let sql = format!(
            r#"
            SELECT l.file_id, f.source, f.path, l.ordinal, l.line_number,
                   l.byte_offset, l.byte_len, l.event_type, l.timestamp,
                   l.conversation_kind, l.process_count
            FROM lines l
            JOIN files f ON f.id = l.file_id
            WHERE l.file_id = ?1 AND {predicate}
            ORDER BY l.ordinal ASC
            "#
        );
        let mut statement = self
            .connection
            .prepare(&sql)
            .context("failed to prepare indexed line query")?;
        let lines = statement
            .query_map([file_id], |row| {
                let conversation_kind = row
                    .get::<_, Option<i64>>(9)?
                    .map(ConversationKind::from_sql)
                    .transpose()?;
                Ok(IndexedLine {
                    file_id: row.get(0)?,
                    source: source_from_name(row.get::<_, String>(1)?.as_str())?,
                    path: PathBuf::from(row.get::<_, String>(2)?),
                    ordinal: sql_u64(row.get(3)?, 3)?,
                    line_number: sql_u64(row.get(4)?, 4)?,
                    byte_offset: sql_u64(row.get(5)?, 5)?,
                    byte_len: sql_u64(row.get(6)?, 6)?,
                    event_type: row.get(7)?,
                    timestamp: row.get(8)?,
                    conversation_kind,
                    process_count: u32::try_from(row.get::<_, i64>(10)?).map_err(|error| {
                        rusqlite::Error::FromSqlConversionFailure(
                            10,
                            rusqlite::types::Type::Integer,
                            Box::new(error),
                        )
                    })?,
                })
            })
            .context("failed to query indexed lines")?
            .collect::<rusqlite::Result<Vec<_>>>()
            .context("failed to decode indexed lines")?;
        Ok(lines)
    }
}

pub fn load_conversation_line(
    line: &IndexedLine,
    max_source_bytes: u64,
) -> Result<Option<ConversationBody>> {
    let Some(kind) = line.conversation_kind else {
        return Ok(None);
    };
    if line.byte_len > max_source_bytes {
        return Ok(Some(ConversationBody::Oversized {
            kind,
            source_bytes: line.byte_len,
        }));
    }

    let bytes = read_range(&line.path, line.byte_offset, line.byte_len)?;
    if line.source == SessionSource::Grok {
        let mut text = String::new();
        let mut model = None;
        for value in serde_json::Deserializer::from_slice(&bytes).into_iter::<Value>() {
            let value = value.context("failed to decode indexed Grok message")?;
            let (loaded_kind, content, message_model, _) =
                extract_conversation_text(line.source, &value)
                    .context("indexed Grok conversation changed")?;
            if loaded_kind != kind {
                bail!("indexed Grok conversation kind changed");
            }
            text.push_str(&content);
            if message_model.is_some() {
                model = message_model;
            }
        }
        return Ok(Some(ConversationBody::Loaded(ConversationMessage {
            kind,
            text,
            model,
            stop_reason: None,
            source_bytes: line.byte_len,
        })));
    }
    let value: Value = serde_json::from_slice(trim_line_ending(&bytes)).with_context(|| {
        format!(
            "failed to decode indexed line {} from {}",
            line.line_number,
            line.path.display()
        )
    })?;
    let Some((loaded_kind, text, model, stop_reason)) =
        extract_conversation_text(line.source, &value)
    else {
        return Ok(None);
    };
    if loaded_kind != kind {
        bail!(
            "indexed conversation kind changed for {} line {}",
            line.path.display(),
            line.line_number
        );
    }

    Ok(Some(ConversationBody::Loaded(ConversationMessage {
        kind,
        text,
        model,
        stop_reason,
        source_bytes: line.byte_len,
    })))
}

pub fn load_process_line(line: &IndexedLine, max_source_bytes: u64) -> Result<ProcessBody> {
    if line.byte_len > max_source_bytes {
        return Ok(ProcessBody::Oversized {
            source_bytes: line.byte_len,
        });
    }
    let bytes = read_range(&line.path, line.byte_offset, line.byte_len)?;
    let value: Value = serde_json::from_slice(trim_line_ending(&bytes)).with_context(|| {
        format!(
            "failed to decode process line {} from {}",
            line.line_number,
            line.path.display()
        )
    })?;
    Ok(ProcessBody::Loaded(extract_process_events(
        line.source,
        &value,
        line.line_number,
    )))
}

pub fn read_line_chunk(
    line: &IndexedLine,
    relative_offset: u64,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    if relative_offset >= line.byte_len || max_bytes == 0 {
        return Ok(Vec::new());
    }
    let remaining = line.byte_len - relative_offset;
    let len = remaining.min(max_bytes as u64);
    read_range(&line.path, line.byte_offset + relative_offset, len)
}

#[derive(Debug, Clone, Copy)]
struct FileIdentity {
    device: u64,
    inode: u64,
    size: u64,
    modified_ns: i64,
}

impl FileIdentity {
    fn from_metadata(metadata: &Metadata) -> Result<Self> {
        Ok(Self {
            device: metadata_device(metadata),
            inode: metadata_inode(metadata),
            size: metadata.len(),
            modified_ns: system_time_ns(metadata.modified()?),
        })
    }
}

#[derive(Debug)]
struct StoredFile {
    id: i64,
    identity: FileIdentity,
    scanned_offset: u64,
    prefix_len: u64,
    prefix_hash: Vec<u8>,
    boundary_len: u64,
    boundary_hash: Vec<u8>,
    line_count: u64,
    session_id: Option<String>,
    title: Option<String>,
    working_dir: Option<String>,
    git_branch: Option<String>,
    model: Option<String>,
    parent_session_id: Option<String>,
    is_sidechain: bool,
    previous_assistant: Option<PreviousAssistant>,
}

#[derive(Debug, Clone)]
struct PreviousAssistant {
    hash: Vec<u8>,
    source: String,
}

#[derive(Debug)]
struct ScanState {
    offset: u64,
    line_count: u64,
    session_id: Option<String>,
    title: Option<String>,
    working_dir: Option<String>,
    git_branch: Option<String>,
    model: Option<String>,
    parent_session_id: Option<String>,
    is_sidechain: bool,
    previous_assistant: Option<PreviousAssistant>,
}

impl ScanState {
    fn apply_descriptor(&mut self, descriptor: &LineDescriptor) {
        replace_if_some(&mut self.session_id, descriptor.session_id.clone());
        replace_if_some(&mut self.title, descriptor.title.clone());
        replace_if_some(&mut self.working_dir, descriptor.working_dir.clone());
        replace_if_some(&mut self.git_branch, descriptor.git_branch.clone());
        replace_if_some(&mut self.model, descriptor.model.clone());
        replace_if_some(
            &mut self.parent_session_id,
            descriptor.parent_session_id.clone(),
        );
        self.is_sidechain |= descriptor.is_sidechain;
    }
}

fn scan_mode(
    path: &Path,
    current: FileIdentity,
    stored: Option<&StoredFile>,
) -> Result<ScanStatus> {
    let Some(stored) = stored else {
        return Ok(ScanStatus::Rebuilt);
    };
    if stored.identity.device != current.device
        || stored.identity.inode != current.inode
        || current.size < stored.scanned_offset
        || !fingerprints_match(path, stored)?
    {
        return Ok(ScanStatus::Rebuilt);
    }

    let pending_bytes = current.size.saturating_sub(stored.scanned_offset);
    if pending_bytes == 0
        || (current.size == stored.identity.size
            && current.modified_ns == stored.identity.modified_ns)
    {
        Ok(ScanStatus::Unchanged)
    } else {
        Ok(ScanStatus::Appended)
    }
}

fn ensure_file_row(
    transaction: &Transaction<'_>,
    source: SessionSource,
    path: &Path,
    identity: FileIdentity,
) -> Result<i64> {
    let existing = transaction
        .query_row(
            "SELECT id FROM files WHERE source = ?1 AND path = ?2",
            params![source_name(source), path.to_string_lossy().as_ref()],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(id) = existing {
        return Ok(id);
    }

    if source == SessionSource::Grok {
        let alternate = path.with_file_name(
            if path.file_name().and_then(|name| name.to_str()) == Some(crate::grok::UPDATES) {
                crate::grok::HISTORY
            } else {
                crate::grok::UPDATES
            },
        );
        let previous: Option<i64> = transaction
            .query_row(
                "SELECT id FROM files WHERE source = 'grok' AND path = ?1",
                [alternate.to_string_lossy().as_ref()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(id) = previous {
            transaction.execute(
                "UPDATE files SET path = ?2 WHERE id = ?1",
                params![id, path.to_string_lossy().as_ref()],
            )?;
            return Ok(id);
        }
    }

    transaction.execute(
        r#"
        INSERT INTO files (
            source, path, device, inode, file_size, modified_ns, scanned_offset,
            prefix_len, prefix_hash, boundary_len, boundary_hash, indexed_at_ns
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, 0, X'', 0, X'', ?7)
        "#,
        params![
            source_name(source),
            path.to_string_lossy().as_ref(),
            sql_i64(identity.device)?,
            sql_i64(identity.inode)?,
            sql_i64(identity.size)?,
            identity.modified_ns,
            now_ns(),
        ],
    )?;
    Ok(transaction.last_insert_rowid())
}

fn prepare_scan(
    transaction: &Transaction<'_>,
    file_id: i64,
    mode: ScanStatus,
    stored: Option<&StoredFile>,
) -> Result<ScanState> {
    if mode == ScanStatus::Rebuilt {
        transaction.execute("DELETE FROM lines WHERE file_id = ?1", [file_id])?;
        transaction.execute("DELETE FROM index_errors WHERE file_id = ?1", [file_id])?;
        return Ok(ScanState {
            offset: 0,
            line_count: 0,
            session_id: None,
            title: None,
            working_dir: None,
            git_branch: None,
            model: None,
            parent_session_id: None,
            is_sidechain: false,
            previous_assistant: None,
        });
    }

    let stored = stored.context("append scan requires stored file state")?;
    debug_assert_eq!(stored.id, file_id);
    Ok(ScanState {
        offset: stored.scanned_offset,
        line_count: stored.line_count,
        session_id: stored.session_id.clone(),
        title: stored.title.clone(),
        working_dir: stored.working_dir.clone(),
        git_branch: stored.git_branch.clone(),
        model: stored.model.clone(),
        parent_session_id: stored.parent_session_id.clone(),
        is_sidechain: stored.is_sidechain,
        previous_assistant: stored.previous_assistant.clone(),
    })
}

#[allow(clippy::too_many_arguments)]
fn scan_lines(
    transaction: &Transaction<'_>,
    file_id: i64,
    source: SessionSource,
    path: &Path,
    initial_identity: FileIdentity,
    mut state: ScanState,
    cancellation: &CancellationToken,
    mut on_progress: impl FnMut(ScanProgress),
) -> Result<ScanProgress> {
    if source == SessionSource::Claude {
        let (is_sidechain, parent_session_id) = claude_path_relationship(path);
        state.is_sidechain |= is_sidechain;
        replace_if_some(&mut state.parent_session_id, parent_session_id);
    }
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    file.seek(SeekFrom::Start(state.offset))?;
    let mut reader = BufReader::new(file);
    let mut buffer = Vec::new();
    let mut error_count = transaction.query_row(
        "SELECT COUNT(*) FROM index_errors WHERE file_id = ?1",
        [file_id],
        |row| row.get::<_, u64>(0),
    )?;
    let mut next_progress_offset = state.offset + PROGRESS_BYTE_INTERVAL;
    let mut next_progress_line = state.line_count + PROGRESS_LINE_INTERVAL;

    loop {
        if cancellation.is_cancelled() {
            bail!("session index scan cancelled");
        }

        buffer.clear();
        let line_offset = state.offset;
        let read = reader.read_until(b'\n', &mut buffer)?;
        if read == 0 {
            break;
        }
        if buffer.last() != Some(&b'\n') {
            break;
        }

        state.offset += u64::try_from(read)?;
        state.line_count += 1;
        let line_number = state.line_count;
        let line = trim_line_ending(&buffer);
        if line.iter().all(u8::is_ascii_whitespace) {
            state.previous_assistant = None;
            continue;
        }

        let value: Value = match serde_json::from_slice(line) {
            Ok(value) => value,
            Err(error) => {
                transaction.execute(
                    r#"
                    INSERT OR REPLACE INTO index_errors
                        (file_id, line_number, byte_offset, message)
                    VALUES (?1, ?2, ?3, ?4)
                    "#,
                    params![
                        file_id,
                        sql_i64(line_number)?,
                        sql_i64(line_offset)?,
                        error.to_string(),
                    ],
                )?;
                error_count += 1;
                state.previous_assistant = None;
                continue;
            }
        };

        let descriptor = classify_line(source, &value);
        state.apply_descriptor(&descriptor);
        let suppressed = conversation_is_suppressed(
            transaction,
            file_id,
            source,
            &descriptor,
            state.previous_assistant.as_ref(),
        )?;
        let mut source_offset = line_offset;
        if source == SessionSource::Grok
            && descriptor.conversation_kind.is_some()
            && matches!(
                descriptor.event_type.as_str(),
                "grok.user_message_chunk" | "grok.agent_message_chunk"
            )
            && (descriptor.conversation_kind == Some(ConversationKind::Assistant)
                || descriptor.message_key.is_some())
        {
            let previous: Option<(i64, u64)> = transaction.query_row(
                "SELECT ordinal, byte_offset FROM lines WHERE file_id = ?1 AND ordinal = ?2 AND event_type = ?3 AND conversation_suppressed = 0 AND conversation_kind = ?4 AND message_key IS ?5",
                params![file_id, sql_i64(line_number.saturating_sub(1))?, descriptor.event_type, descriptor.conversation_kind.map(|kind| kind as i64), descriptor.message_key],
                |row| Ok((row.get(0)?, row.get(1)?)),
            ).optional()?;
            if let Some((ordinal, offset)) = previous {
                transaction.execute("UPDATE lines SET conversation_suppressed = 1 WHERE file_id = ?1 AND ordinal = ?2", params![file_id, ordinal])?;
                source_offset = offset;
            }
        }
        insert_line(
            transaction,
            file_id,
            line_number,
            source_offset,
            state.offset - source_offset,
            &descriptor,
            suppressed,
        )?;
        state.previous_assistant = descriptor_previous_assistant(&descriptor);

        if state.offset >= next_progress_offset || state.line_count >= next_progress_line {
            on_progress(ScanProgress {
                bytes_scanned: state.offset,
                total_bytes: initial_identity.size.max(state.offset),
                complete_lines: state.line_count,
                error_count,
            });
            next_progress_offset = state.offset + PROGRESS_BYTE_INTERVAL;
            next_progress_line = state.line_count + PROGRESS_LINE_INTERVAL;
        }
    }

    let final_metadata = fs::metadata(path)?;
    let final_identity = FileIdentity::from_metadata(&final_metadata)?;
    if final_identity.device != initial_identity.device
        || final_identity.inode != initial_identity.inode
    {
        bail!("session file was replaced while it was being indexed");
    }

    let (prefix_len, prefix_hash) = fingerprint_prefix(path, state.offset)?;
    let (boundary_len, boundary_hash) = fingerprint_boundary(path, state.offset)?;
    update_file_summary(
        transaction,
        file_id,
        final_identity,
        &state,
        prefix_len,
        &prefix_hash,
        boundary_len,
        &boundary_hash,
    )?;

    let progress = ScanProgress {
        bytes_scanned: state.offset,
        total_bytes: final_identity.size,
        complete_lines: state.line_count,
        error_count,
    };
    on_progress(progress);
    Ok(progress)
}

fn conversation_is_suppressed(
    transaction: &Transaction<'_>,
    file_id: i64,
    source: SessionSource,
    descriptor: &LineDescriptor,
    previous: Option<&PreviousAssistant>,
) -> Result<bool> {
    if descriptor.conversation_kind != Some(ConversationKind::Assistant) {
        return Ok(false);
    }

    if source == SessionSource::Claude {
        if let Some(message_key) = descriptor.message_key.as_deref() {
            transaction.execute(
                r#"
                UPDATE lines
                SET conversation_suppressed = 1
                WHERE file_id = ?1 AND message_key = ?2
                  AND conversation_kind = ?3 AND conversation_suppressed = 0
                "#,
                params![file_id, message_key, ConversationKind::Assistant as i64],
            )?;
            return Ok(false);
        }
    }

    let Some(previous) = previous else {
        return Ok(false);
    };
    Ok(descriptor
        .message_hash
        .as_deref()
        .zip(descriptor.assistant_source.as_deref())
        .is_some_and(|(hash, assistant_source)| {
            hash == previous.hash && assistant_source != previous.source
        }))
}

fn insert_line(
    transaction: &Transaction<'_>,
    file_id: i64,
    line_number: u64,
    byte_offset: u64,
    byte_len: u64,
    descriptor: &LineDescriptor,
    suppressed: bool,
) -> Result<()> {
    transaction.execute(
        r#"
        INSERT INTO lines (
            file_id, ordinal, line_number, byte_offset, byte_len, event_type,
            timestamp, conversation_kind, conversation_suppressed, process_count,
            message_key, message_hash, assistant_source
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)
        "#,
        params![
            file_id,
            sql_i64(line_number)?,
            sql_i64(line_number)?,
            sql_i64(byte_offset)?,
            sql_i64(byte_len)?,
            descriptor.event_type,
            descriptor.timestamp,
            descriptor.conversation_kind.map(|kind| kind as i64),
            suppressed,
            descriptor.process_count,
            descriptor.message_key,
            descriptor.message_hash,
            descriptor.assistant_source,
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn update_file_summary(
    transaction: &Transaction<'_>,
    file_id: i64,
    identity: FileIdentity,
    state: &ScanState,
    prefix_len: u64,
    prefix_hash: &[u8],
    boundary_len: u64,
    boundary_hash: &[u8],
) -> Result<()> {
    let (user_count, assistant_count, process_count): (u64, u64, u64) = transaction
        .query_row(
            r#"
            SELECT
                COALESCE(SUM(CASE WHEN conversation_kind = 1 AND conversation_suppressed = 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN conversation_kind = 2 AND conversation_suppressed = 0 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(process_count), 0)
            FROM lines WHERE file_id = ?1
            "#,
            [file_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
    let error_count: u64 = transaction.query_row(
        "SELECT COUNT(*) FROM index_errors WHERE file_id = ?1",
        [file_id],
        |row| row.get(0),
    )?;
    let started_at: Option<String> = transaction.query_row(
        "SELECT MIN(timestamp) FROM lines WHERE file_id = ?1 AND timestamp IS NOT NULL",
        [file_id],
        |row| row.get(0),
    )?;
    let event_updated_at: Option<String> = transaction.query_row(
        "SELECT MAX(timestamp) FROM lines WHERE file_id = ?1 AND timestamp IS NOT NULL",
        [file_id],
        |row| row.get(0),
    )?;
    let updated_at = event_updated_at.unwrap_or_else(|| {
        DateTime::<Utc>::from(
            UNIX_EPOCH + std::time::Duration::from_nanos(identity.modified_ns.max(0) as u64),
        )
        .to_rfc3339_opts(SecondsFormat::Millis, true)
    });
    let previous = state.previous_assistant.as_ref();

    transaction.execute(
        r#"
        UPDATE files SET
            device = ?2, inode = ?3, file_size = ?4, modified_ns = ?5,
            scanned_offset = ?6, prefix_len = ?7, prefix_hash = ?8,
            boundary_len = ?9, boundary_hash = ?10, line_count = ?11,
            user_count = ?12, assistant_count = ?13, process_count = ?14,
            error_count = ?15, started_at = ?16, updated_at = ?17,
            session_id = ?18, title = ?19, working_dir = ?20,
            git_branch = ?21, model = ?22, parent_session_id = ?23,
            is_sidechain = ?24, last_record_was_assistant = ?25,
            last_assistant_hash = ?26, last_assistant_source = ?27,
            indexed_at_ns = ?28
        WHERE id = ?1
        "#,
        params![
            file_id,
            sql_i64(identity.device)?,
            sql_i64(identity.inode)?,
            sql_i64(identity.size)?,
            identity.modified_ns,
            sql_i64(state.offset)?,
            sql_i64(prefix_len)?,
            prefix_hash,
            sql_i64(boundary_len)?,
            boundary_hash,
            sql_i64(state.line_count)?,
            sql_i64(user_count)?,
            sql_i64(assistant_count)?,
            sql_i64(process_count)?,
            sql_i64(error_count)?,
            started_at,
            updated_at,
            state.session_id,
            state.title,
            state.working_dir,
            state.git_branch,
            state.model,
            state.parent_session_id,
            state.is_sidechain,
            previous.is_some(),
            previous.map(|previous| previous.hash.as_slice()),
            previous.map(|previous| previous.source.as_str()),
            now_ns(),
        ],
    )?;
    Ok(())
}

fn refresh_grok_metadata(transaction: &Transaction<'_>, file_id: i64, path: &Path) -> Result<()> {
    let metadata = crate::grok::metadata(path);
    transaction.execute(
        "UPDATE files SET session_id = ?2, title = COALESCE(?3, title), working_dir = COALESCE(?4, working_dir), git_branch = COALESCE(?5, git_branch), model = COALESCE(?6, model), started_at = COALESCE(?7, started_at), updated_at = CASE WHEN ?8 IS NULL THEN updated_at WHEN ?9 THEN MAX(updated_at, ?8) ELSE ?8 END WHERE id = ?1",
        params![file_id, metadata.id, metadata.title, metadata.cwd, metadata.branch, metadata.model, metadata.created_at, metadata.updated_at, path.file_name().and_then(|name| name.to_str()) == Some(crate::grok::UPDATES)],
    )?;
    Ok(())
}

fn descriptor_previous_assistant(descriptor: &LineDescriptor) -> Option<PreviousAssistant> {
    if descriptor.conversation_kind != Some(ConversationKind::Assistant) {
        return None;
    }
    Some(PreviousAssistant {
        hash: descriptor.message_hash.clone()?,
        source: descriptor.assistant_source.clone()?,
    })
}

fn fingerprints_match(path: &Path, stored: &StoredFile) -> Result<bool> {
    let prefix_hash = hash_range(path, 0, stored.prefix_len)?;
    let boundary_start = stored.scanned_offset.saturating_sub(stored.boundary_len);
    let boundary_hash = hash_range(path, boundary_start, stored.boundary_len)?;
    Ok(prefix_hash == stored.prefix_hash && boundary_hash == stored.boundary_hash)
}

fn fingerprint_prefix(path: &Path, scanned_offset: u64) -> Result<(u64, Vec<u8>)> {
    let len = scanned_offset.min(FINGERPRINT_BYTES);
    Ok((len, hash_range(path, 0, len)?))
}

fn fingerprint_boundary(path: &Path, scanned_offset: u64) -> Result<(u64, Vec<u8>)> {
    let len = scanned_offset.min(FINGERPRINT_BYTES);
    Ok((
        len,
        hash_range(path, scanned_offset.saturating_sub(len), len)?,
    ))
}

fn hash_range(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    let bytes = read_range(path, offset, len)?;
    Ok(Sha256::digest(bytes).to_vec())
}

fn read_range(path: &Path, offset: u64, len: u64) -> Result<Vec<u8>> {
    let len = usize::try_from(len).context("indexed line is too large for this platform")?;
    let mut file =
        File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    file.seek(SeekFrom::Start(offset))?;
    let mut bytes = vec![0; len];
    file.read_exact(&mut bytes).with_context(|| {
        format!(
            "failed to read bytes {offset}..{} from {}",
            offset + len as u64,
            path.display()
        )
    })?;
    Ok(bytes)
}

fn trim_line_ending(mut bytes: &[u8]) -> &[u8] {
    if bytes.last() == Some(&b'\n') {
        bytes = &bytes[..bytes.len() - 1];
    }
    if bytes.last() == Some(&b'\r') {
        bytes = &bytes[..bytes.len() - 1];
    }
    bytes
}

fn map_indexed_file(row: &rusqlite::Row<'_>) -> rusqlite::Result<IndexedFile> {
    let file_size = sql_u64(row.get(3)?, 3)?;
    let scanned_offset = sql_u64(row.get(4)?, 4)?;
    Ok(IndexedFile {
        id: row.get(0)?,
        source: source_from_name(row.get::<_, String>(1)?.as_str())?,
        path: PathBuf::from(row.get::<_, String>(2)?),
        file_size,
        scanned_offset,
        pending_bytes: file_size.saturating_sub(scanned_offset),
        line_count: sql_u64(row.get(5)?, 5)?,
        user_count: sql_u64(row.get(6)?, 6)?,
        assistant_count: sql_u64(row.get(7)?, 7)?,
        process_count: sql_u64(row.get(8)?, 8)?,
        error_count: sql_u64(row.get(9)?, 9)?,
        started_at: row.get(10)?,
        updated_at: row.get(11)?,
        session_id: row.get(12)?,
        title: row.get(13)?,
        working_dir: row.get(14)?,
        git_branch: row.get(15)?,
        model: row.get(16)?,
        parent_session_id: row.get(17)?,
        is_sidechain: row.get(18)?,
    })
}

fn source_name(source: SessionSource) -> &'static str {
    match source {
        SessionSource::Claude => "claude",
        SessionSource::Codex => "codex",
        SessionSource::Grok => "grok",
    }
}

fn replace_if_some<T>(slot: &mut Option<T>, value: Option<T>) {
    if let Some(value) = value {
        *slot = Some(value);
    }
}

fn claude_path_relationship(path: &Path) -> (bool, Option<String>) {
    let components = path
        .components()
        .filter_map(|component| component.as_os_str().to_str())
        .collect::<Vec<_>>();
    let Some(subagents_index) = components
        .iter()
        .position(|component| *component == "subagents")
    else {
        return (false, None);
    };
    let parent = subagents_index
        .checked_sub(1)
        .and_then(|index| components.get(index))
        .map(|value| (*value).to_string());
    (true, parent)
}

fn source_from_name(value: &str) -> rusqlite::Result<SessionSource> {
    match value {
        "claude" => Ok(SessionSource::Claude),
        "codex" => Ok(SessionSource::Codex),
        "grok" => Ok(SessionSource::Grok),
        _ => Err(rusqlite::Error::FromSqlConversionFailure(
            1,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("unknown session source in index: {value}"),
            )),
        )),
    }
}

fn sql_i64(value: u64) -> Result<i64> {
    i64::try_from(value).context("value exceeds SQLite INTEGER range")
}

fn sql_u64(value: i64, column: usize) -> rusqlite::Result<u64> {
    u64::try_from(value).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            column,
            rusqlite::types::Type::Integer,
            Box::new(error),
        )
    })
}

fn now_ns() -> i64 {
    system_time_ns(SystemTime::now())
}

fn system_time_ns(time: SystemTime) -> i64 {
    time.duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_nanos()).ok())
        .unwrap_or(i64::MAX)
}

#[cfg(unix)]
fn metadata_device(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.dev()
}

#[cfg(not(unix))]
fn metadata_device(_: &Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn metadata_inode(metadata: &Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn metadata_inode(_: &Metadata) -> u64 {
    0
}

pub(super) fn normalize_timestamp(value: &str) -> Option<String> {
    DateTime::parse_from_rfc3339(value).ok().map(|timestamp| {
        timestamp
            .with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    })
}

#[cfg(test)]
mod tests {
    use std::{
        fs::{self, OpenOptions},
        io::Write,
        path::Path,
    };

    use anyhow::Result;
    use serde_json::{json, Value};
    use tempfile::tempdir;

    use super::{
        load_process_line, CancellationToken, ConversationBody, ConversationKind, IndexDatabase,
        ProcessBody, ScanStatus,
    };
    use crate::{SessionEvent, SessionSource};

    #[test]
    fn codex_append_preserves_index_and_deduplicates_alternate_assistant_record() -> Result<()> {
        let directory = tempdir()?;
        let session = directory.path().join("codex.jsonl");
        write_lines(
            &session,
            &[
                json!({
                    "timestamp": "2026-08-28T01:00:00Z",
                    "type": "event_msg",
                    "payload": {"type": "user_message", "message": "question"}
                }),
                json!({
                    "timestamp": "2026-08-28T01:00:01Z",
                    "type": "event_msg",
                    "payload": {"type": "agent_message", "message": "answer"}
                }),
            ],
        )?;

        let mut index = IndexDatabase::open_in_memory()?;
        let first = scan(&mut index, SessionSource::Codex, &session)?;
        assert_eq!(first.status, ScanStatus::Rebuilt);
        assert_eq!(first.file.user_count, 1);
        assert_eq!(first.file.assistant_count, 1);

        append_lines(
            &session,
            &[json!({
                "timestamp": "2026-08-28T01:00:01.001Z",
                "type": "response_item",
                "payload": {
                    "type": "message",
                    "role": "assistant",
                    "content": [{"type": "output_text", "text": "answer"}]
                }
            })],
        )?;
        let second = scan(&mut index, SessionSource::Codex, &session)?;
        assert_eq!(second.status, ScanStatus::Appended);
        assert_eq!(second.file.assistant_count, 1);

        let conversations = index.conversation_lines(second.file.id)?;
        assert_eq!(conversations.len(), 2);
        let body = index
            .load_conversation(&conversations[1], 1024 * 1024)?
            .expect("assistant body should load");
        assert!(matches!(
            body,
            ConversationBody::Loaded(message)
                if message.kind == ConversationKind::Assistant && message.text == "answer"
        ));
        Ok(())
    }

    #[test]
    fn truncate_rebuilds_without_stale_rows() -> Result<()> {
        let directory = tempdir()?;
        let session = directory.path().join("codex.jsonl");
        write_lines(
            &session,
            &[
                codex_user("first", "2026-08-28T01:00:00Z"),
                codex_user("second", "2026-08-28T01:00:01Z"),
            ],
        )?;
        let mut index = IndexDatabase::open_in_memory()?;
        let first = scan(&mut index, SessionSource::Codex, &session)?;
        assert_eq!(first.file.user_count, 2);

        write_lines(
            &session,
            &[codex_user("replacement", "2026-08-28T02:00:00Z")],
        )?;
        let replacement = scan(&mut index, SessionSource::Codex, &session)?;
        assert_eq!(replacement.status, ScanStatus::Rebuilt);
        assert_eq!(replacement.file.user_count, 1);
        assert_eq!(index.conversation_lines(replacement.file.id)?.len(), 1);
        Ok(())
    }

    #[test]
    fn malformed_complete_line_is_skipped_and_incomplete_tail_is_retried() -> Result<()> {
        let directory = tempdir()?;
        let session = directory.path().join("codex.jsonl");
        let first = serde_json::to_string(&codex_user("first", "2026-08-28T01:00:00Z"))?;
        fs::write(&session, format!("{first}\nnot-json\n{{\"type\":"))?;

        let mut index = IndexDatabase::open_in_memory()?;
        let initial = scan(&mut index, SessionSource::Codex, &session)?;
        assert_eq!(initial.file.line_count, 2);
        assert_eq!(initial.file.error_count, 1);
        assert!(initial.file.pending_bytes > 0);
        assert_eq!(initial.file.user_count, 1);

        let mut file = OpenOptions::new().append(true).open(&session)?;
        writeln!(
            file,
            "\"event_msg\",\"payload\":{{\"type\":\"user_message\",\"message\":\"later\"}}}}"
        )?;
        let appended = scan(&mut index, SessionSource::Codex, &session)?;
        assert_eq!(appended.status, ScanStatus::Appended);
        assert_eq!(appended.file.line_count, 3);
        assert_eq!(appended.file.error_count, 1);
        assert_eq!(appended.file.pending_bytes, 0);
        assert_eq!(appended.file.user_count, 2);
        Ok(())
    }

    #[test]
    fn claude_stream_updates_keep_only_latest_assistant_message() -> Result<()> {
        let directory = tempdir()?;
        let session = directory.path().join("claude.jsonl");
        write_lines(
            &session,
            &[
                claude_assistant("message-1", "first"),
                claude_assistant("message-1", "final"),
            ],
        )?;

        let mut index = IndexDatabase::open_in_memory()?;
        let outcome = scan(&mut index, SessionSource::Claude, &session)?;
        assert_eq!(outcome.file.assistant_count, 1);
        let conversations = index.conversation_lines(outcome.file.id)?;
        assert_eq!(conversations.len(), 1);
        let body = index
            .load_conversation(&conversations[0], 1024 * 1024)?
            .expect("assistant body should load");
        assert!(matches!(
            body,
            ConversationBody::Loaded(message) if message.text == "final"
        ));
        Ok(())
    }

    #[test]
    fn cancelled_rebuild_rolls_back_file_and_line_rows() -> Result<()> {
        let directory = tempdir()?;
        let session = directory.path().join("codex.jsonl");
        write_lines(&session, &[codex_user("first", "2026-08-28T01:00:00Z")])?;
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let mut index = IndexDatabase::open_in_memory()?;

        let error = index
            .scan_file(SessionSource::Codex, &session, &cancellation, |_| {})
            .expect_err("cancelled scan should fail");
        assert!(error.to_string().contains("cancelled"));
        assert!(index
            .indexed_file(SessionSource::Codex, &session)?
            .is_none());
        Ok(())
    }

    #[test]
    fn corrupt_database_is_archived_and_rebuilt() -> Result<()> {
        let directory = tempdir()?;
        let database = directory.path().join("index.sqlite");
        fs::write(&database, b"not a sqlite database")?;

        let index = IndexDatabase::open(&database)?;
        assert!(index.list_files(SessionSource::Codex)?.is_empty());
        let archived = fs::read_dir(directory.path())?
            .filter_map(Result::ok)
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .any(|name| name.starts_with("index.corrupt-") && name.ends_with(".sqlite"));
        assert!(archived);
        Ok(())
    }

    #[test]
    fn metadata_and_process_payloads_are_loaded_without_storing_body_text() -> Result<()> {
        let directory = tempdir()?;
        let session = directory.path().join("codex.jsonl");
        write_lines(
            &session,
            &[
                json!({
                    "timestamp": "2026-08-28T01:00:00Z",
                    "type": "session_meta",
                    "payload": {
                        "type": "session_meta",
                        "id": "session-id",
                        "cwd": "/tmp/workspace",
                        "model": "gpt-test"
                    }
                }),
                json!({
                    "timestamp": "2026-08-28T01:00:01Z",
                    "type": "response_item",
                    "payload": {
                        "type": "function_call",
                        "call_id": "call-1",
                        "name": "exec_command",
                        "arguments": "{\"cmd\":\"pwd\"}"
                    }
                }),
            ],
        )?;
        let mut index = IndexDatabase::open_in_memory()?;
        let outcome = scan(&mut index, SessionSource::Codex, &session)?;
        assert_eq!(outcome.file.session_id.as_deref(), Some("session-id"));
        assert_eq!(outcome.file.working_dir.as_deref(), Some("/tmp/workspace"));
        assert_eq!(outcome.file.model.as_deref(), Some("gpt-test"));

        let process = index.process_lines(outcome.file.id)?;
        assert_eq!(process.len(), 1);
        let body = load_process_line(&process[0], 1024 * 1024)?;
        assert!(matches!(
            body,
            ProcessBody::Loaded(events)
                if matches!(events.as_slice(), [SessionEvent::ToolCall { name, .. }] if name == "exec_command")
        ));
        Ok(())
    }

    fn scan(
        index: &mut IndexDatabase,
        source: SessionSource,
        path: &Path,
    ) -> Result<super::ScanOutcome> {
        index.scan_file(source, path, &CancellationToken::default(), |_| {})
    }

    fn write_lines(path: &Path, lines: &[Value]) -> Result<()> {
        let mut file = fs::File::create(path)?;
        for line in lines {
            serde_json::to_writer(&mut file, line)?;
            writeln!(file)?;
        }
        Ok(())
    }

    fn append_lines(path: &Path, lines: &[Value]) -> Result<()> {
        let mut file = OpenOptions::new().append(true).open(path)?;
        for line in lines {
            serde_json::to_writer(&mut file, line)?;
            writeln!(file)?;
        }
        Ok(())
    }

    fn codex_user(message: &str, timestamp: &str) -> Value {
        json!({
            "timestamp": timestamp,
            "type": "event_msg",
            "payload": {"type": "user_message", "message": message}
        })
    }

    fn claude_assistant(message_id: &str, text: &str) -> Value {
        json!({
            "timestamp": "2026-08-28T01:00:00Z",
            "type": "assistant",
            "message": {
                "id": message_id,
                "model": "claude-test",
                "content": [{"type": "text", "text": text}]
            }
        })
    }
}
