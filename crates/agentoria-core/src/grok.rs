use std::{
    fs::File,
    io::{BufRead, BufReader, Read},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use serde_json::Value;

use crate::{
    index::ConversationKind, util, EventMeta, SessionDetail, SessionEvent, SessionSource,
    SessionStats, SessionSummary,
};

const METADATA_LIMIT: u64 = 64 * 1024;
pub(crate) const UPDATES: &str = "updates.jsonl";
pub(crate) const HISTORY: &str = "chat_history.jsonl";

#[derive(Default)]
pub(crate) struct Metadata {
    pub id: String,
    pub title: Option<String>,
    pub cwd: Option<String>,
    pub branch: Option<String>,
    pub model: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

fn bounded_text(path: &Path) -> Option<String> {
    let mut bytes = Vec::new();
    File::open(path)
        .ok()?
        .take(METADATA_LIMIT + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() as u64 <= METADATA_LIMIT)
        .then(|| String::from_utf8(bytes).ok())
        .flatten()
}

fn string(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)?
        .as_str()
        .filter(|text| !text.is_empty())
        .map(str::to_owned)
}

fn iso(value: Option<&str>) -> Option<String> {
    DateTime::parse_from_rfc3339(value?).ok().map(|time| {
        time.with_timezone(&Utc)
            .to_rfc3339_opts(SecondsFormat::Millis, true)
    })
}

fn decode_cwd(name: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(name.len());
    let mut input = name.as_bytes().iter().copied();
    while let Some(byte) = input.next() {
        bytes.push(if byte == b'%' {
            let hi = (input.next()? as char).to_digit(16)?;
            let lo = (input.next()? as char).to_digit(16)?;
            (hi * 16 + lo) as u8
        } else {
            byte
        });
    }
    let decoded = String::from_utf8(bytes).ok()?;
    Path::new(&decoded).is_absolute().then_some(decoded)
}

pub(crate) fn metadata(path: &Path) -> Metadata {
    let directory = path.parent().unwrap_or(path);
    let value = bounded_text(&directory.join("summary.json"))
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .unwrap_or(Value::Null);
    let group = directory.parent();
    Metadata {
        id: value
            .pointer("/info/id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map(str::to_owned)
            .unwrap_or_else(|| {
                directory
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()
            }),
        title: util::clean_title(value.get("session_summary").and_then(Value::as_str))
            .or_else(|| util::clean_title(value.get("generated_title").and_then(Value::as_str))),
        cwd: value
            .pointer("/info/cwd")
            .and_then(Value::as_str)
            .filter(|cwd| !cwd.is_empty())
            .map(str::to_owned)
            .or_else(|| {
                group
                    .and_then(|group| bounded_text(&group.join(".cwd")))
                    .map(|text| text.trim().to_string())
                    .filter(|cwd| Path::new(cwd).is_absolute())
            })
            .or_else(|| {
                group
                    .and_then(|group| group.file_name()?.to_str())
                    .and_then(decode_cwd)
            }),
        branch: string(&value, "head_branch"),
        model: string(&value, "current_model_id"),
        created_at: iso(value.get("created_at").and_then(Value::as_str)),
        updated_at: iso(value.get("updated_at").and_then(Value::as_str)),
    }
}

pub(crate) fn preferred_transcript(directory: &Path) -> Option<PathBuf> {
    [UPDATES, HISTORY]
        .into_iter()
        .map(|name| directory.join(name))
        .find(|path| path.is_file())
}

pub(crate) fn is_transcript(path: &Path) -> bool {
    path.parent()
        .and_then(preferred_transcript)
        .is_some_and(|preferred| preferred == path)
}

pub(crate) fn kind(value: &Value) -> &str {
    update(value)
        .get("sessionUpdate")
        .or_else(|| value.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("")
}

pub(crate) fn message_key(value: &Value) -> Option<String> {
    (kind(value) == "user_message_chunk")
        .then(|| update(value).pointer("/_meta/promptIndex"))
        .flatten()
        .map(|index| format!("grok.user.{index}"))
}

fn update(value: &Value) -> &Value {
    value.pointer("/params/update").unwrap_or(value)
}

pub(crate) fn timestamp(value: &Value) -> Option<String> {
    value
        .get("timestamp")
        .and_then(Value::as_i64)
        .and_then(DateTime::<Utc>::from_timestamp_millis)
        .map(|time| time.to_rfc3339_opts(SecondsFormat::Millis, true))
        .or_else(|| iso(value.get("timestamp").and_then(Value::as_str)))
}

fn text(content: &Value) -> String {
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    if let Some(text) = content.get("text").and_then(Value::as_str) {
        return text.to_string();
    }
    content
        .as_array()
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("text").and_then(Value::as_str))
                .collect::<Vec<_>>()
                .join("\n\n")
        })
        .unwrap_or_default()
}

pub(crate) fn conversation(
    value: &Value,
) -> Option<(ConversationKind, String, Option<String>, Option<String>)> {
    let message = update(value);
    let is_chunk = matches!(kind(value), "user_message_chunk" | "agent_message_chunk");
    if is_chunk && message.pointer("/content/type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    let role = match kind(value) {
        "user_message_chunk" => ConversationKind::User,
        "agent_message_chunk" => ConversationKind::Assistant,
        "user" if value.get("synthetic_reason").is_none_or(Value::is_null) => {
            ConversationKind::User
        }
        "assistant" => ConversationKind::Assistant,
        _ => return None,
    };
    let mut content = text(message.get("content")?);
    if role == ConversationKind::User && kind(value) == "user" {
        if let Some((_, query)) = content.split_once("<user_query>") {
            if let Some((query, _)) = query.rsplit_once("</user_query>") {
                content = query.trim().to_string();
            }
        } else if content.trim_start().starts_with("<user_info>") {
            // Old Grok versions stored harness bootstrap context as an untagged user message.
            return None;
        }
    }
    if !is_chunk && content.trim().is_empty() {
        return None;
    }
    Some((
        role,
        content,
        string(value, "model_id").or_else(|| {
            message
                .pointer("/_meta/modelId")
                .and_then(Value::as_str)
                .map(str::to_owned)
        }),
        None,
    ))
}

pub(crate) fn process_count(value: &Value) -> u32 {
    match kind(value) {
        "user_message_chunk" | "agent_message_chunk" => u32::from(
            update(value)
                .pointer("/content/type")
                .and_then(Value::as_str)
                != Some("text"),
        ),
        "assistant" => value
            .get("tool_calls")
            .and_then(Value::as_array)
            .map_or(0, |calls| calls.len().min(u32::MAX as usize) as u32),
        "user" => u32::from(conversation(value).is_none()),
        "" => 0,
        _ => 1,
    }
}

pub(crate) fn process_events(value: &Value, line: u64) -> Vec<SessionEvent> {
    let message = update(value);
    let kind = kind(value);
    let meta = || EventMeta::new(format!("grok-{line}"), kind).with_timestamp(timestamp(value));
    let content = || message.get("content").map(text).unwrap_or_default();
    let event = match kind {
        "assistant" => {
            return value
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .enumerate()
                .map(|(index, call)| {
                    let input = call.get("arguments").cloned().unwrap_or(Value::Null);
                    let input = input
                        .as_str()
                        .and_then(|raw| serde_json::from_str(raw).ok())
                        .unwrap_or(input);
                    SessionEvent::ToolCall {
                        meta: EventMeta::new(format!("grok-tool-{line}-{index}"), "tool_call"),
                        call_id: string(call, "id"),
                        name: string(call, "name").unwrap_or_else(|| "tool".into()),
                        input,
                        summary: None,
                    }
                })
                .collect()
        }
        "user_message_chunk" | "agent_message_chunk" if process_count(value) == 0 => {
            return Vec::new()
        }
        "user" if conversation(value).is_some() => return Vec::new(),
        "tool_call" => SessionEvent::ToolCall {
            meta: meta(),
            call_id: string(message, "toolCallId"),
            name: message
                .pointer("/_meta/x.ai~1tool")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| string(message, "title"))
                .unwrap_or_else(|| "tool".into()),
            input: message.get("rawInput").cloned().unwrap_or(Value::Null),
            summary: string(message, "title"),
        },
        "tool_result" => SessionEvent::ToolResult {
            meta: meta(),
            call_id: string(value, "tool_call_id"),
            content: content(),
            is_error: value
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        },
        "tool_call_update" => SessionEvent::ToolResult {
            meta: meta(),
            call_id: string(message, "toolCallId"),
            content: message
                .get("content")
                .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
                .or_else(|| {
                    message
                        .get("rawOutput")
                        .map(|value| serde_json::to_string_pretty(value).unwrap_or_default())
                })
                .unwrap_or_else(|| serde_json::to_string_pretty(message).unwrap_or_default()),
            is_error: message.get("status").and_then(Value::as_str) == Some("failed"),
        },
        "reasoning" | "agent_thought_chunk" => SessionEvent::Reasoning {
            meta: meta(),
            text: if kind == "reasoning" {
                value.get("summary").map(text).unwrap_or_default()
            } else {
                content()
            },
            encrypted: value
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_some_and(|text| !text.is_empty()),
        },
        "turn_completed" => {
            let usage = message.get("usage").unwrap_or(&Value::Null);
            SessionEvent::TokenUsage {
                meta: meta(),
                input_tokens: usage.get("inputTokens").and_then(Value::as_u64),
                output_tokens: usage.get("outputTokens").and_then(Value::as_u64),
                cache_read_tokens: usage.get("cachedReadTokens").and_then(Value::as_u64),
                cache_creation_tokens: usage.get("cacheCreationTokens").and_then(Value::as_u64),
                rate_limit: None,
            }
        }
        "auto_compact_started" | "auto_compact_completed" | "compaction_checkpoint" => {
            SessionEvent::Compaction {
                meta: meta(),
                summary: string(message, "summary_preview").or_else(|| string(message, "reason")),
            }
        }
        "user"
            if value.get("synthetic_reason").and_then(Value::as_str) == Some("compaction_meta") =>
        {
            SessionEvent::Compaction {
                meta: meta(),
                summary: Some(content()),
            }
        }
        "system" | "user" | "hook_execution" | "session_recap" => SessionEvent::SystemNotice {
            meta: meta(),
            subtype: string(value, "synthetic_reason").or_else(|| string(message, "event_name")),
            level: None,
            content: Some(if kind == "session_recap" {
                string(message, "summary").unwrap_or_default()
            } else if kind == "hook_execution" {
                serde_json::to_string_pretty(message).unwrap_or_default()
            } else {
                content()
            }),
            hook_count: None,
        },
        "user_message_chunk" | "agent_message_chunk" => SessionEvent::Attachment {
            meta: meta(),
            name: None,
            mime: message
                .pointer("/content/mimeType")
                .and_then(Value::as_str)
                .map(str::to_owned),
            preview: None,
        },
        _ => SessionEvent::Unknown {
            meta: meta(),
            raw_type: kind.to_string(),
        },
    };
    vec![event]
}

pub(crate) fn list_sessions() -> Result<Vec<SessionSummary>> {
    let mut summaries = Vec::new();
    for file in crate::discover_session_files(SessionSource::Grok)? {
        if let Ok(detail) = read_session(&file.path, false) {
            summaries.push(detail.summary);
        }
    }
    summaries.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    Ok(summaries)
}

pub(crate) fn read_session_detail(path: &Path) -> Result<SessionDetail> {
    read_session(path, true)
}

fn read_session(path: &Path, retain_events: bool) -> Result<SessionDetail> {
    let metadata = metadata(path);
    let mut summary = SessionSummary::empty(
        metadata.id,
        SessionSource::Grok,
        path.to_string_lossy().into_owned(),
    );
    let mut needs_title = metadata.title.is_none();
    summary.title = metadata.title.unwrap_or_else(|| summary.id.clone());
    summary.working_dir = metadata.cwd.clone();
    summary.project_path = metadata.cwd;
    summary.git_branch = metadata.branch;
    summary.model = metadata.model;
    summary.updated_at = metadata
        .updated_at
        .or_else(|| util::metadata_timestamp(path).ok())
        .unwrap_or_default();
    summary.is_partial = false;
    let mut stats = SessionStats::new(summary.path.clone(), summary.updated_at.clone());
    stats.started_at = metadata.created_at;
    let file = File::open(path).with_context(|| format!("failed to read {}", path.display()))?;
    let mut events = Vec::new();
    let mut previous_chunk = None;
    for (line, raw) in BufReader::new(file).lines().enumerate() {
        let raw = raw?;
        let Ok(value) = serde_json::from_str::<Value>(&raw) else {
            previous_chunk = None;
            continue;
        };
        if let Some((role, text, model, stop_reason)) = conversation(&value) {
            let chunk = matches!(kind(&value), "user_message_chunk" | "agent_message_chunk");
            let key = message_key(&value);
            let chunk = chunk && (role == ConversationKind::Assistant || key.is_some());
            let merged = chunk && previous_chunk == Some((role, key.clone()));
            if !merged {
                match role {
                    ConversationKind::User => stats.user_count += 1,
                    ConversationKind::Assistant => stats.assistant_count += 1,
                }
            }
            stats.content_chars += text.chars().count();
            let meta = EventMeta::new(format!("grok-{line}"), kind(&value))
                .with_timestamp(timestamp(&value));
            if role == ConversationKind::User {
                summary.preview = util::clean_preview(&text);
                if needs_title {
                    if let Some(title) = util::clean_title(Some(&text)) {
                        summary.title = title;
                        needs_title = false;
                    }
                }
            }
            if summary.model.is_none() {
                summary.model = model.clone();
            }
            if retain_events {
                if merged {
                    if let Some(
                        SessionEvent::User { text: previous, .. }
                        | SessionEvent::AssistantMessage { text: previous, .. },
                    ) = events.last_mut()
                    {
                        previous.push_str(&text);
                    }
                } else {
                    events.push(match role {
                        ConversationKind::User => SessionEvent::User { meta, text },
                        ConversationKind::Assistant => SessionEvent::AssistantMessage {
                            meta,
                            text,
                            model,
                            stop_reason,
                        },
                    });
                }
            }
            previous_chunk = chunk.then_some((role, key));
        } else {
            previous_chunk = None;
        }
        let process = process_events(&value, line as u64);
        for event in &process {
            match event {
                SessionEvent::ToolCall { .. } => stats.tool_call_count += 1,
                SessionEvent::Reasoning { .. } => stats.reasoning_count += 1,
                SessionEvent::TokenUsage {
                    input_tokens,
                    output_tokens,
                    cache_read_tokens,
                    cache_creation_tokens,
                    ..
                } => {
                    stats.total_input_tokens += input_tokens.unwrap_or(0);
                    stats.total_output_tokens += output_tokens.unwrap_or(0);
                    stats.cache_read_tokens += cache_read_tokens.unwrap_or(0);
                    stats.cache_creation_tokens += cache_creation_tokens.unwrap_or(0);
                }
                _ => {}
            }
        }
        stats.event_count += process.len();
        if retain_events {
            events.extend(process);
        }
    }
    stats.event_count += stats.user_count + stats.assistant_count;
    summary.user_count = stats.user_count;
    summary.assistant_count = stats.assistant_count;
    summary.message_count = stats.user_count + stats.assistant_count;
    summary.tool_call_count = stats.tool_call_count;
    Ok(SessionDetail {
        summary,
        events,
        stats,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::{
        search::search_conversations, CancellationToken, ConversationBody, IndexDatabase,
        ScanStatus,
    };
    use serde_json::json;
    use std::{fs, io::Write};

    fn update(kind: &str, fields: Value) -> Value {
        let mut update = fields.as_object().unwrap().clone();
        update.insert("sessionUpdate".into(), kind.into());
        json!({"timestamp":1793145600000i64,"method":"session/update","params":{"sessionId":"session-1","update":update}})
    }
    fn chunk(kind: &str, text: &str) -> Value {
        update(
            kind,
            json!({"content":{"type":"text","text":text},"_meta":{"promptIndex":0,"modelId":"grok-test"}}),
        )
    }
    fn fixture() -> (tempfile::TempDir, PathBuf, IndexDatabase) {
        let dir = tempfile::tempdir().unwrap();
        let session = dir.path().join("sessions/%2Ftmp%2Fa%2Bb/session-1");
        fs::create_dir_all(&session).unwrap();
        fs::write(session.join("summary.json"),json!({"info":{"id":"session-1","cwd":"/tmp/repo"},"session_summary":"Test title","current_model_id":"grok-test","head_branch":"main","created_at":"2026-10-01T00:00:00Z","updated_at":"2026-10-28T12:00:00Z"}).to_string()).unwrap();
        let index = IndexDatabase::open(dir.path().join("index.sqlite")).unwrap();
        (dir, session, index)
    }
    fn write_rows(path: &Path, rows: &[Value]) {
        fs::write(
            path,
            rows.iter()
                .map(|row| format!("{row}\n"))
                .collect::<String>(),
        )
        .unwrap();
    }
    fn loaded_text(index: &IndexDatabase, id: i64) -> Vec<String> {
        index
            .conversation_lines(id)
            .unwrap()
            .iter()
            .map(|line| {
                match crate::index::load_conversation_line(line, 512 * 1024)
                    .unwrap()
                    .unwrap()
                {
                    ConversationBody::Loaded(message) => message.text,
                    _ => panic!("fixture should load inline"),
                }
            })
            .collect()
    }

    #[test]
    fn updates_keep_conversation_and_process_payloads_separate() {
        let (_dir, session, mut index) = fixture();
        let path = session.join(UPDATES);
        write_rows(
            &path,
            &[
                update(
                    "hook_execution",
                    json!({"event_name":"session_start","runs":[]}),
                ),
                chunk("user_message_chunk", "Fix café 中文"),
                chunk("agent_message_chunk", "First **"),
                chunk("agent_message_chunk", "answer** 中文"),
                update(
                    "tool_call",
                    json!({"toolCallId":"call-1","title":"Read","rawInput":{"path":"src/main.rs"}}),
                ),
                update(
                    "tool_call_update",
                    json!({"toolCallId":"call-1","status":"completed","content":[{"type":"content","content":{"type":"text","text":"tool output"}}]}),
                ),
                update(
                    "agent_thought_chunk",
                    json!({"content":{"type":"text","text":"reasoning"}}),
                ),
                update(
                    "turn_completed",
                    json!({"usage":{"inputTokens":12,"outputTokens":5,"cachedReadTokens":3},"stop_reason":"end_turn"}),
                ),
            ],
        );
        let file = index
            .scan_file(
                SessionSource::Grok,
                &path,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap()
            .file;
        assert_eq!(
            (file.user_count, file.assistant_count, file.process_count),
            (1, 1, 5)
        );
        assert_eq!(
            loaded_text(&index, file.id),
            ["Fix café 中文", "First **answer** 中文"]
        );
        assert_eq!(file.title.as_deref(), Some("Test title"));
        assert_eq!(file.working_dir.as_deref(), Some("/tmp/repo"));
        assert_eq!(file.started_at.as_deref(), Some("2026-10-01T00:00:00.000Z"));
        let results = search_conversations(
            &index.conversation_lines(file.id).unwrap(),
            "answer** 中文",
            512 * 1024,
            100,
            &CancellationToken::default(),
        )
        .unwrap();
        assert_eq!(results.total_matches, 1);
        let detail = read_session_detail(&path).unwrap();
        assert_eq!(
            (
                detail.stats.user_count,
                detail.stats.assistant_count,
                detail.stats.tool_call_count
            ),
            (1, 1, 1)
        );
        assert_eq!(
            (
                detail.stats.total_input_tokens,
                detail.stats.total_output_tokens
            ),
            (12, 5)
        );
        assert_eq!(index.process_lines(file.id).unwrap().len(), 5);
    }

    #[test]
    fn appended_chunks_merge_and_incomplete_tail_waits_without_losing_offset() {
        let (_dir, session, mut index) = fixture();
        let path = session.join(UPDATES);
        write_rows(
            &path,
            &[
                chunk("user_message_chunk", "Question"),
                chunk("agent_message_chunk", "Hello"),
                chunk("agent_message_chunk", " "),
            ],
        );
        let first = index
            .scan_file(
                SessionSource::Grok,
                &path,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap()
            .file;
        let extra = chunk("agent_message_chunk", "Grok 中文").to_string();
        let mut file = fs::OpenOptions::new().append(true).open(&path).unwrap();
        write!(file, "{extra}").unwrap();
        let pending = index
            .scan_file(
                SessionSource::Grok,
                &path,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap();
        assert_eq!(pending.file.scanned_offset, first.scanned_offset);
        assert_eq!(loaded_text(&index, first.id), ["Question", "Hello "]);
        writeln!(file).unwrap();
        let appended = index
            .scan_file(
                SessionSource::Grok,
                &path,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap();
        assert_eq!(appended.status, ScanStatus::Appended);
        assert_eq!(
            (appended.file.user_count, appended.file.assistant_count),
            (1, 1)
        );
        assert_eq!(
            loaded_text(&index, first.id),
            ["Question", "Hello Grok 中文"]
        );
        let lines = index.conversation_lines(first.id).unwrap();
        assert!(matches!(
            crate::index::load_conversation_line(&lines[1], 1).unwrap(),
            Some(ConversationBody::Oversized { .. })
        ));
    }

    #[test]
    fn different_user_prompts_do_not_merge_and_summary_only_rename_is_refreshed() {
        let (_dir, session, mut index) = fixture();
        let path = session.join(UPDATES);
        let a = chunk("user_message_chunk", "One");
        let mut b = chunk("user_message_chunk", "Two");
        b["params"]["update"]["_meta"]["promptIndex"] = 1.into();
        write_rows(&path, &[a, b]);
        let first = index
            .scan_file(
                SessionSource::Grok,
                &path,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap()
            .file;
        assert_eq!(first.user_count, 2);
        let metadata_path = session.join("summary.json");
        let mut summary: Value =
            serde_json::from_str(&fs::read_to_string(&metadata_path).unwrap()).unwrap();
        summary["session_summary"] = "Renamed".into();
        fs::write(metadata_path, summary.to_string()).unwrap();
        let next = index
            .scan_file(
                SessionSource::Grok,
                &path,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap();
        assert_eq!(next.status, ScanStatus::Unchanged);
        assert_eq!(next.file.scanned_offset, first.scanned_offset);
        assert_eq!(next.file.title.as_deref(), Some("Renamed"));
        assert_eq!(index.list_files(SessionSource::Grok).unwrap().len(), 1);
    }

    #[test]
    fn old_history_filters_harness_context_and_retains_mixed_assistant_tools() {
        let (_dir, session, mut index) = fixture();
        let path = session.join(HISTORY);
        write_rows(
            &path,
            &[
                json!({"type":"system","content":"system prompt"}),
                json!({"type":"user","content":[{"type":"text","text":"<user_info>bootstrap</user_info>"}]}),
                json!({"type":"user","synthetic_reason":"system_reminder","content":"hidden"}),
                json!({"type":"user","content":[{"type":"text","text":"<user_info>cwd</user_info><user_query>Actual prompt</user_query>"}]}),
                json!({"type":"reasoning","summary":[{"type":"summary_text","text":"think"}],"encrypted_content":"opaque"}),
                json!({"type":"assistant","content":"Response","model_id":"grok-test","tool_calls":[{"id":"x","name":"read","arguments":"{\"path\":\"a\"}"}]}),
                json!({"type":"tool_result","tool_call_id":"x","content":"output"}),
                json!({"type":"user","synthetic_reason":"compaction_meta","content":"compressed context"}),
            ],
        );
        let file = index
            .scan_file(
                SessionSource::Grok,
                &path,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap()
            .file;
        assert_eq!(
            (file.user_count, file.assistant_count, file.process_count),
            (1, 1, 7)
        );
        assert_eq!(loaded_text(&index, file.id), ["Actual prompt", "Response"]);
        let detail = read_session_detail(&path).unwrap();
        assert_eq!(
            (detail.stats.user_count, detail.stats.assistant_count),
            (1, 1)
        );
        assert!(detail
            .events
            .iter()
            .any(|event| matches!(event,SessionEvent::ToolCall{input,..} if input["path"]=="a")));
    }

    #[test]
    fn switching_to_authoritative_updates_reuses_file_identity_and_rebuilds() {
        let (_dir, session, mut index) = fixture();
        let history = session.join(HISTORY);
        write_rows(&history, &[json!({"type":"user","content":"Old context"})]);
        let old = index
            .scan_file(
                SessionSource::Grok,
                &history,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap()
            .file;
        let path = session.join(UPDATES);
        write_rows(&path, &[chunk("user_message_chunk", "Full history")]);
        let new = index
            .scan_file(
                SessionSource::Grok,
                &path,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap();
        assert_eq!(new.status, ScanStatus::Rebuilt);
        assert_eq!(old.id, new.file.id);
        assert_eq!(index.list_files(SessionSource::Grok).unwrap().len(), 1);
        assert_eq!(loaded_text(&index, old.id), ["Full history"]);
    }

    #[test]
    fn cancelled_format_switch_rolls_back_path_and_conversation_rows() {
        let (_dir, session, mut index) = fixture();
        let history = session.join(HISTORY);
        write_rows(
            &history,
            &[json!({"type":"user","content":"Keep old context"})],
        );
        let first = index
            .scan_file(
                SessionSource::Grok,
                &history,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap()
            .file;
        let updates = session.join(UPDATES);
        write_rows(&updates, &[chunk("user_message_chunk", "New history")]);
        let token = CancellationToken::default();
        token.cancel();
        assert!(index
            .scan_file(SessionSource::Grok, &updates, &token, |_| {})
            .is_err());
        assert!(index
            .indexed_file(SessionSource::Grok, updates.canonicalize().unwrap())
            .unwrap()
            .is_none());
        assert_eq!(
            index
                .indexed_file(SessionSource::Grok, &first.path)
                .unwrap()
                .unwrap()
                .id,
            first.id
        );
        assert_eq!(loaded_text(&index, first.id), ["Keep old context"]);
    }

    #[test]
    fn missing_or_bounded_metadata_uses_directory_id_and_encoded_or_hashed_cwd() {
        let (_dir, session, _index) = fixture();
        let path = session.join(HISTORY);
        fs::remove_file(session.join("summary.json")).unwrap();
        let fallback = metadata(&path);
        assert_eq!(fallback.id, "session-1");
        assert_eq!(fallback.cwd.as_deref(), Some("/tmp/a+b"));
        fs::write(
            session.parent().unwrap().join(".cwd"),
            "/tmp/中文 repository",
        )
        .unwrap();
        fs::write(
            session.join("summary.json"),
            "x".repeat(METADATA_LIMIT as usize + 1),
        )
        .unwrap();
        assert_eq!(metadata(&path).cwd.as_deref(), Some("/tmp/中文 repository"));
        assert_eq!(decode_cwd("slug-hash"), None);
    }

    #[test]
    fn long_chunk_stream_keeps_one_lazy_message_and_enforces_its_source_limit() {
        let (_dir, session, mut index) = fixture();
        let path = session.join(UPDATES);
        let mut file = std::io::BufWriter::new(File::create(&path).unwrap());
        for _ in 0..5_000 {
            writeln!(file, "{}", chunk("agent_message_chunk", "text ")).unwrap();
        }
        file.flush().unwrap();
        let indexed = index
            .scan_file(
                SessionSource::Grok,
                &path,
                &CancellationToken::default(),
                |_| {},
            )
            .unwrap()
            .file;
        assert_eq!(indexed.assistant_count, 1);
        let lines = index.conversation_lines(indexed.id).unwrap();
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].byte_offset, 0);
        assert_eq!(lines[0].byte_len, indexed.file_size);
        assert!(matches!(
            crate::index::load_conversation_line(&lines[0], 512 * 1024).unwrap(),
            Some(ConversationBody::Oversized { .. })
        ));
        assert!(
            matches!(crate::index::load_conversation_line(&lines[0],u64::MAX).unwrap(),Some(ConversationBody::Loaded(message)) if message.text.len()==25_000)
        );
    }
}
