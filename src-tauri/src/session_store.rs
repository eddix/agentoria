use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tauri::async_runtime;
use walkdir::WalkDir;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionSource {
    Claude,
    Codex,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    id: String,
    source: SessionSource,
    title: String,
    preview: String,
    is_partial: bool,
    updated_at: String,
    path: String,
    working_dir: Option<String>,
    project_path: Option<String>,
    message_count: usize,
    tool_call_count: usize,
    user_count: usize,
    assistant_count: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetail {
    summary: SessionSummary,
    messages: Vec<SessionMessage>,
    stats: SessionStats,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionMessage {
    id: String,
    role: String,
    kind: String,
    timestamp: Option<String>,
    phase: Option<String>,
    label: Option<String>,
    text: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    file_path: String,
    updated_at: String,
    started_at: Option<String>,
    message_count: usize,
    user_count: usize,
    assistant_count: usize,
    tool_call_count: usize,
    content_chars: usize,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeIndexFile {
    entries: Vec<ClaudeIndexEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ClaudeIndexEntry {
    session_id: String,
    first_prompt: Option<String>,
    summary: Option<String>,
    message_count: Option<usize>,
    created: Option<String>,
    modified: Option<String>,
    project_path: Option<String>,
}

#[derive(Debug, Default, Clone)]
struct CodexIndexEntry {
    title: Option<String>,
    updated_at: Option<String>,
}

#[tauri::command]
pub async fn list_sessions(source: String) -> Result<Vec<SessionSummary>, String> {
    let source = parse_source(&source).map_err(|error| error.to_string())?;

    async_runtime::spawn_blocking(move || match source {
        SessionSource::Codex => list_codex_sessions(),
        SessionSource::Claude => list_claude_sessions(),
    })
    .await
    .map_err(join_error_to_string)?
    .map_err(|error| error.to_string())
}

#[tauri::command]
pub async fn get_session_detail(source: String, path: String) -> Result<SessionDetail, String> {
    let source = parse_source(&source).map_err(|error| error.to_string())?;
    let path = validate_session_path(&source, &path).map_err(|error| error.to_string())?;

    async_runtime::spawn_blocking(move || match source {
        SessionSource::Codex => read_codex_session_detail(&path),
        SessionSource::Claude => read_claude_session_detail(&path),
    })
    .await
    .map_err(join_error_to_string)?
    .map_err(|error| error.to_string())
}

fn parse_source(raw: &str) -> Result<SessionSource> {
    match raw {
        "codex" => Ok(SessionSource::Codex),
        "claude" => Ok(SessionSource::Claude),
        _ => bail!("unknown source: {raw}"),
    }
}

fn home_dir() -> Result<PathBuf> {
    dirs::home_dir().context("failed to resolve home directory")
}

fn codex_root() -> Result<PathBuf> {
    Ok(home_dir()?.join(".codex"))
}

fn claude_root() -> Result<PathBuf> {
    Ok(home_dir()?.join(".claude").join("projects"))
}

fn validate_session_path(source: &SessionSource, raw_path: &str) -> Result<PathBuf> {
    let candidate = PathBuf::from(raw_path);
    let base = match source {
        SessionSource::Codex => codex_root()?.join("sessions"),
        SessionSource::Claude => claude_root()?,
    };

    if !candidate.starts_with(&base) {
        bail!("session path is outside of the expected source root");
    }

    if !candidate.exists() {
        bail!("session file does not exist");
    }

    Ok(candidate)
}

fn list_codex_sessions() -> Result<Vec<SessionSummary>> {
    let index = read_codex_index()?;

    if !index.is_empty() {
        return list_codex_sessions_from_index(&index);
    }

    let sessions_root = codex_root()?.join("sessions");
    let mut sessions = Vec::new();

    for entry in WalkDir::new(&sessions_root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }

        if let Ok(summary) = summarize_codex_session(path, &index) {
            sessions.push(summary);
        }
    }

    sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    sessions.truncate(300);
    Ok(sessions)
}

fn list_codex_sessions_from_index(
    index: &HashMap<String, CodexIndexEntry>,
) -> Result<Vec<SessionSummary>> {
    let sessions_root = codex_root()?.join("sessions");
    let path_index = build_codex_path_index(&sessions_root);
    let mut sessions = Vec::new();

    for (id, entry) in index {
        let Some(path) = path_index.get(id) else {
            continue;
        };

        let fallback_updated_at = metadata_timestamp(path)?;
        sessions.push(SessionSummary {
            id: id.clone(),
            source: SessionSource::Codex,
            title: entry.title.clone().unwrap_or_else(|| id.clone()),
            preview: String::new(),
            is_partial: true,
            updated_at: entry
                .updated_at
                .clone()
                .unwrap_or_else(|| fallback_updated_at.clone()),
            path: path.to_string_lossy().into_owned(),
            working_dir: None,
            project_path: None,
            message_count: 0,
            tool_call_count: 0,
            user_count: 0,
            assistant_count: 0,
        });
    }

    sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    sessions.truncate(300);

    for session in &mut sessions {
        session.working_dir = read_codex_working_dir(Path::new(&session.path))?;
    }

    Ok(sessions)
}

fn read_codex_working_dir(path: &Path) -> Result<Option<String>> {
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;

    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if value.get("type").and_then(Value::as_str) != Some("session_meta") {
            continue;
        }

        return Ok(value
            .pointer("/payload/cwd")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned));
    }

    Ok(None)
}

fn list_claude_sessions() -> Result<Vec<SessionSummary>> {
    let index = read_claude_index()?;
    let mut sessions = Vec::new();
    let mut seen_ids = HashSet::new();

    for path in collect_claude_session_files()? {
        let session_id = file_stem_id(&path);
        if !seen_ids.insert(session_id.clone()) {
            continue;
        }

        let summary = summarize_claude_session(&path, index.get(&session_id))?;
        sessions.push(summary);
    }

    sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    sessions.truncate(300);
    Ok(sessions)
}

fn read_codex_index() -> Result<HashMap<String, CodexIndexEntry>> {
    let index_path = codex_root()?.join("session_index.jsonl");
    if !index_path.exists() {
        return Ok(HashMap::new());
    }

    let mut index = HashMap::new();
    let file = File::open(&index_path)
        .with_context(|| format!("failed to open {}", index_path.display()))?;

    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        let Some(id) = value.get("id").and_then(Value::as_str) else {
            continue;
        };

        let slot = index
            .entry(id.to_string())
            .or_insert_with(CodexIndexEntry::default);
        slot.title = clean_title(value.get("thread_name").and_then(Value::as_str));
        slot.updated_at = value
            .get("updated_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }

    Ok(index)
}

fn read_claude_index() -> Result<HashMap<String, ClaudeIndexEntry>> {
    let mut index_entries = HashMap::new();

    for index_path in WalkDir::new(claude_root()?)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
    {
        if index_path.file_name().to_str() != Some("sessions-index.json") {
            continue;
        }

        let file = File::open(index_path.path())
            .with_context(|| format!("failed to open {}", index_path.path().display()))?;
        let index: ClaudeIndexFile = serde_json::from_reader(file)
            .with_context(|| format!("failed to parse {}", index_path.path().display()))?;

        for entry in index.entries {
            index_entries.insert(entry.session_id.clone(), entry);
        }
    }

    Ok(index_entries)
}

fn summarize_codex_session(
    path: &Path,
    index: &HashMap<String, CodexIndexEntry>,
) -> Result<SessionSummary> {
    let fallback_updated_at = metadata_timestamp(path)?;
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;

    let mut session_id = file_stem_id(path);
    let mut working_dir = None;
    let mut first_prompt = None;
    let mut user_count = 0;
    let mut assistant_count = 0;
    let mut tool_call_count = 0;

    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                if let Some(id) = value.pointer("/payload/id").and_then(Value::as_str) {
                    session_id = id.to_string();
                }
                if let Some(cwd) = value.pointer("/payload/cwd").and_then(Value::as_str) {
                    working_dir = Some(cwd.to_string());
                }
            }
            Some("event_msg") => {
                if value.pointer("/payload/type").and_then(Value::as_str) == Some("user_message") {
                    user_count += 1;
                    if first_prompt.is_none() {
                        first_prompt = value
                            .pointer("/payload/message")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned);
                    }
                }
            }
            Some("response_item") => match value.pointer("/payload/type").and_then(Value::as_str) {
                Some("message") => {
                    if value.pointer("/payload/role").and_then(Value::as_str) == Some("assistant") {
                        assistant_count += 1;
                    }
                }
                Some("function_call") => {
                    tool_call_count += 1;
                }
                _ => {}
            },
            _ => {}
        }
    }

    let indexed = index.get(&session_id).cloned().unwrap_or_default();
    let title = indexed
        .title
        .or_else(|| clean_title(first_prompt.as_deref()))
        .unwrap_or_else(|| session_id.clone());
    let preview = clean_preview(first_prompt.as_deref().unwrap_or(""));
    let updated_at = indexed.updated_at.unwrap_or(fallback_updated_at);

    Ok(SessionSummary {
        id: session_id,
        source: SessionSource::Codex,
        title,
        preview,
        is_partial: false,
        updated_at,
        path: path.to_string_lossy().into_owned(),
        working_dir,
        project_path: None,
        message_count: user_count + assistant_count,
        tool_call_count,
        user_count,
        assistant_count,
    })
}

fn read_codex_session_detail(path: &Path) -> Result<SessionDetail> {
    let index = read_codex_index()?;
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut messages = Vec::new();
    let mut content_chars = 0;
    let mut started_at = None;
    let mut session_id = file_stem_id(path);
    let mut working_dir = None;
    let mut first_prompt = None;
    let mut user_count = 0;
    let mut assistant_count = 0;
    let mut tool_call_count = 0;

    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        match value.get("type").and_then(Value::as_str) {
            Some("session_meta") => {
                started_at = value
                    .pointer("/payload/timestamp")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                if let Some(id) = value.pointer("/payload/id").and_then(Value::as_str) {
                    session_id = id.to_string();
                }
                if let Some(cwd) = value.pointer("/payload/cwd").and_then(Value::as_str) {
                    working_dir = Some(cwd.to_string());
                }
            }
            Some("event_msg") => {
                if value.pointer("/payload/type").and_then(Value::as_str) == Some("user_message") {
                    let text = value
                        .pointer("/payload/message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if !text.is_empty() {
                        user_count += 1;
                        if first_prompt.is_none() {
                            first_prompt = Some(text.clone());
                        }
                        content_chars += text.chars().count();
                        messages.push(SessionMessage {
                            id: format!("codex-user-{line_no}"),
                            role: "user".to_string(),
                            kind: "message".to_string(),
                            timestamp: value
                                .get("timestamp")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            phase: None,
                            label: None,
                            text,
                        });
                    }
                }
            }
            Some("response_item") => match value.pointer("/payload/type").and_then(Value::as_str) {
                Some("message") => {
                    if value.pointer("/payload/role").and_then(Value::as_str) != Some("assistant") {
                        continue;
                    }

                    let text = extract_codex_message_text(value.pointer("/payload/content"));
                    if text.is_empty() {
                        continue;
                    }

                    assistant_count += 1;
                    content_chars += text.chars().count();
                    messages.push(SessionMessage {
                        id: format!("codex-assistant-{line_no}"),
                        role: "assistant".to_string(),
                        kind: "message".to_string(),
                        timestamp: value
                            .get("timestamp")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        phase: value
                            .pointer("/payload/phase")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        label: None,
                        text,
                    });
                }
                Some("function_call") => {
                    tool_call_count += 1;
                    let name = value
                        .pointer("/payload/name")
                        .and_then(Value::as_str)
                        .unwrap_or("tool");
                    let text = describe_codex_tool_call(
                        name,
                        value.pointer("/payload/arguments").and_then(Value::as_str),
                    );
                    content_chars += text.chars().count();
                    messages.push(SessionMessage {
                        id: format!("codex-tool-{line_no}"),
                        role: "tool".to_string(),
                        kind: "tool".to_string(),
                        timestamp: value
                            .get("timestamp")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        phase: None,
                        label: Some(name.to_string()),
                        text,
                    });
                }
                _ => {}
            },
            _ => {}
        }
    }

    let indexed = index.get(&session_id).cloned().unwrap_or_default();
    let fallback_updated_at = metadata_timestamp(path)?;
    let summary = SessionSummary {
        id: session_id.clone(),
        source: SessionSource::Codex,
        title: indexed
            .title
            .or_else(|| clean_title(first_prompt.as_deref()))
            .unwrap_or(session_id),
        preview: clean_preview(first_prompt.as_deref().unwrap_or("")),
        is_partial: false,
        updated_at: indexed.updated_at.unwrap_or(fallback_updated_at),
        path: path.to_string_lossy().into_owned(),
        working_dir,
        project_path: None,
        message_count: user_count + assistant_count,
        tool_call_count,
        user_count,
        assistant_count,
    };

    let stats = SessionStats {
        file_path: summary.path.clone(),
        updated_at: summary.updated_at.clone(),
        started_at,
        message_count: summary.message_count,
        user_count: summary.user_count,
        assistant_count: summary.assistant_count,
        tool_call_count: summary.tool_call_count,
        content_chars,
    };

    Ok(SessionDetail {
        summary,
        messages,
        stats,
    })
}

fn read_claude_session_detail(path: &Path) -> Result<SessionDetail> {
    let index = read_claude_index()?;
    let session_id = file_stem_id(path);
    let indexed = index.get(&session_id).cloned();
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut messages = Vec::new();
    let mut assistant_slots: HashMap<String, usize> = HashMap::new();
    let mut user_count = 0;
    let mut assistant_count = 0;
    let mut tool_call_count = 0;
    let mut content_chars = 0;
    let mut started_at = None;

    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if started_at.is_none() {
            started_at = value
                .get("timestamp")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }

        match value.get("type").and_then(Value::as_str) {
            Some("user") => {
                if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
                    continue;
                }

                let Some(content) = value.pointer("/message/content") else {
                    continue;
                };

                match content {
                    Value::String(text) => {
                        let text = text.trim().to_string();
                        if text.is_empty() {
                            continue;
                        }
                        user_count += 1;
                        content_chars += text.chars().count();
                        messages.push(SessionMessage {
                            id: format!("claude-user-{line_no}"),
                            role: "user".to_string(),
                            kind: "message".to_string(),
                            timestamp: value
                                .get("timestamp")
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned),
                            phase: None,
                            label: None,
                            text,
                        });
                    }
                    Value::Array(items) => {
                        if let Some(text) = extract_claude_tool_result(items) {
                            content_chars += text.chars().count();
                            messages.push(SessionMessage {
                                id: format!("claude-tool-result-{line_no}"),
                                role: "tool".to_string(),
                                kind: "tool_result".to_string(),
                                timestamp: value
                                    .get("timestamp")
                                    .and_then(Value::as_str)
                                    .map(ToOwned::to_owned),
                                phase: None,
                                label: Some("tool_result".to_string()),
                                text,
                            });
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                let message_id = value
                    .pointer("/message/id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| format!("assistant-{line_no}"));
                let timestamp = value
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let Some(items) = value.pointer("/message/content").and_then(Value::as_array)
                else {
                    continue;
                };

                let mut text_parts = Vec::new();
                for item in items {
                    match item.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if let Some(text) = item.get("text").and_then(Value::as_str) {
                                let trimmed = text.trim();
                                if !trimmed.is_empty() {
                                    text_parts.push(trimmed.to_string());
                                }
                            }
                        }
                        Some("tool_use") => {
                            tool_call_count += 1;
                            let name = item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool_use")
                                .to_string();
                            let text = describe_claude_tool_use(item);
                            content_chars += text.chars().count();
                            messages.push(SessionMessage {
                                id: format!("claude-tool-{line_no}-{tool_call_count}"),
                                role: "tool".to_string(),
                                kind: "tool".to_string(),
                                timestamp: timestamp.clone(),
                                phase: None,
                                label: Some(name),
                                text,
                            });
                        }
                        _ => {}
                    }
                }

                if !text_parts.is_empty() {
                    let text = text_parts.join("\n\n");
                    content_chars += text.chars().count();
                    if let Some(slot) = assistant_slots.get(&message_id).copied() {
                        messages[slot].text.push_str("\n\n");
                        messages[slot].text.push_str(&text);
                    } else {
                        assistant_count += 1;
                        let index = messages.len();
                        messages.push(SessionMessage {
                            id: message_id.clone(),
                            role: "assistant".to_string(),
                            kind: "message".to_string(),
                            timestamp,
                            phase: None,
                            label: None,
                            text,
                        });
                        assistant_slots.insert(message_id, index);
                    }
                }
            }
            _ => {}
        }
    }

    let mut summary = summarize_claude_session(path, indexed.as_ref())?;
    summary.message_count = user_count + assistant_count;
    summary.user_count = user_count;
    summary.assistant_count = assistant_count;
    summary.tool_call_count = tool_call_count;

    let stats = SessionStats {
        file_path: summary.path.clone(),
        updated_at: summary.updated_at.clone(),
        started_at,
        message_count: summary.message_count,
        user_count,
        assistant_count,
        tool_call_count,
        content_chars,
    };

    Ok(SessionDetail {
        summary,
        messages,
        stats,
    })
}

fn extract_codex_message_text(content: Option<&Value>) -> String {
    let Some(Value::Array(items)) = content else {
        return String::new();
    };

    let mut parts = Vec::new();
    for item in items {
        match item.get("type").and_then(Value::as_str) {
            Some("output_text") | Some("input_text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        parts.push(trimmed.to_string());
                    }
                }
            }
            _ => {}
        }
    }

    parts.join("\n\n")
}

fn describe_codex_tool_call(name: &str, raw_arguments: Option<&str>) -> String {
    let parsed = raw_arguments.and_then(|raw| serde_json::from_str::<Value>(raw).ok());

    match name {
        "exec_command" => parsed
            .as_ref()
            .and_then(|value| value.get("cmd"))
            .and_then(Value::as_str)
            .map(|command| format!("run command\n{command}"))
            .unwrap_or_else(|| "run command".to_string()),
        "update_plan" => parsed
            .as_ref()
            .and_then(|value| value.get("plan"))
            .and_then(Value::as_array)
            .map(|steps| {
                let listed = steps
                    .iter()
                    .filter_map(|step| step.get("step").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n- ");
                if listed.is_empty() {
                    "update plan".to_string()
                } else {
                    format!("update plan\n- {listed}")
                }
            })
            .unwrap_or_else(|| "update plan".to_string()),
        "apply_patch" => "apply patch".to_string(),
        other => {
            let snippet = parsed
                .as_ref()
                .map(compact_json)
                .unwrap_or_else(|| raw_arguments.unwrap_or("").to_string());
            if snippet.is_empty() {
                other.to_string()
            } else {
                format!("{other}\n{snippet}")
            }
        }
    }
}

fn describe_claude_tool_use(item: &Value) -> String {
    let name = item.get("name").and_then(Value::as_str).unwrap_or("tool");
    let input = item.get("input").map(compact_json).unwrap_or_default();

    if input.is_empty() {
        name.to_string()
    } else {
        format!("{name}\n{input}")
    }
}

fn extract_claude_tool_result(items: &[Value]) -> Option<String> {
    for item in items {
        if item.get("type").and_then(Value::as_str) != Some("tool_result") {
            continue;
        }

        let content = item.get("content")?;
        return match content {
            Value::String(text) => Some(clean_preview(text)),
            Value::Array(parts) => {
                let text = parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|part| !part.is_empty())
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.is_empty() {
                    None
                } else {
                    Some(text)
                }
            }
            _ => None,
        };
    }

    None
}

fn metadata_timestamp(path: &Path) -> Result<String> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to read metadata for {}", path.display()))?;
    let modified = metadata
        .modified()
        .context("failed to read modified time")?;
    Ok(DateTime::<Utc>::from(modified).to_rfc3339())
}

fn collect_claude_session_files() -> Result<Vec<PathBuf>> {
    let root = claude_root()?;
    let mut files = Vec::new();

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }
        if path
            .components()
            .any(|component| component.as_os_str() == "subagents")
        {
            continue;
        }

        files.push(path.to_path_buf());
    }

    Ok(files)
}

fn build_codex_path_index(root: &Path) -> HashMap<String, PathBuf> {
    let mut entries = HashMap::new();

    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("jsonl") {
            continue;
        }

        entries.insert(file_stem_id(path), path.to_path_buf());
    }

    entries
}

fn summarize_claude_session(
    path: &Path,
    indexed: Option<&ClaudeIndexEntry>,
) -> Result<SessionSummary> {
    let fallback_updated_at = metadata_timestamp(path)?;
    let session_id = file_stem_id(path);

    let title = indexed
        .and_then(|entry| clean_title(entry.summary.as_deref()))
        .or_else(|| indexed.and_then(|entry| clean_title(entry.first_prompt.as_deref())))
        .unwrap_or_else(|| session_id.clone());
    let preview = clean_preview(
        indexed
            .and_then(|entry| entry.first_prompt.as_deref().or(entry.summary.as_deref()))
            .unwrap_or(""),
    );
    let updated_at = indexed
        .and_then(|entry| entry.modified.clone().or(entry.created.clone()))
        .unwrap_or(fallback_updated_at);

    Ok(SessionSummary {
        id: session_id,
        source: SessionSource::Claude,
        title,
        preview,
        is_partial: false,
        updated_at,
        path: path.to_string_lossy().into_owned(),
        working_dir: indexed.and_then(|entry| entry.project_path.clone()),
        project_path: indexed.and_then(|entry| entry.project_path.clone()),
        message_count: indexed.and_then(|entry| entry.message_count).unwrap_or(0),
        tool_call_count: 0,
        user_count: 0,
        assistant_count: 0,
    })
}

fn join_error_to_string(error: impl std::fmt::Display) -> String {
    format!("background task failed: {error}")
}

fn clean_title(raw: Option<&str>) -> Option<String> {
    raw.and_then(|text| {
        let cleaned = text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .unwrap_or("")
            .chars()
            .take(72)
            .collect::<String>();
        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned)
        }
    })
}

fn clean_preview(raw: &str) -> String {
    raw.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(180)
        .collect()
}

fn file_stem_id(path: &Path) -> String {
    let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
        return "session".to_string();
    };

    if let Some(candidate) = stem.get(stem.len().saturating_sub(36)..) {
        let bytes = candidate.as_bytes();
        let looks_like_uuid = bytes.len() == 36
            && matches!(bytes.get(8), Some(b'-'))
            && matches!(bytes.get(13), Some(b'-'))
            && matches!(bytes.get(18), Some(b'-'))
            && matches!(bytes.get(23), Some(b'-'))
            && bytes.iter().enumerate().all(|(index, byte)| match index {
                8 | 13 | 18 | 23 => *byte == b'-',
                _ => byte.is_ascii_hexdigit(),
            });

        if looks_like_uuid {
            return candidate.to_string();
        }
    }

    stem.to_string()
}

fn compact_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}
