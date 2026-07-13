use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde::Deserialize;
use serde_json::Value;
use walkdir::WalkDir;

use super::model::*;
use super::util::*;

const LIST_LIMIT: usize = 1000;
const RESULT_TRUNCATE_CHARS: usize = 24_000;

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
struct IndexFile {
    #[serde(default)]
    entries: Vec<IndexEntry>,
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
#[allow(dead_code)]
struct IndexEntry {
    session_id: String,
    full_path: Option<String>,
    first_prompt: Option<String>,
    summary: Option<String>,
    message_count: Option<usize>,
    created: Option<String>,
    modified: Option<String>,
    git_branch: Option<String>,
    project_path: Option<String>,
    #[serde(default)]
    is_sidechain: bool,
}

#[derive(Debug, Clone)]
struct PathInfo {
    path: PathBuf,
    is_subagent: bool,
    parent_session_id: Option<String>,
    agent_id: Option<String>,
}

pub fn list_sessions() -> Result<Vec<SessionSummary>> {
    let root = claude_root()?;
    if !root.exists() {
        return Ok(Vec::new());
    }

    let index_by_path = read_indexed_paths(&root)?;
    let mut sessions = Vec::new();
    let mut subagent_counts: HashMap<String, u32> = HashMap::new();

    for info in collect_session_files(&root)? {
        if info.is_subagent {
            if let Some(parent) = &info.parent_session_id {
                *subagent_counts.entry(parent.clone()).or_default() += 1;
            }
        }
        let canonical = canonical_or(&info.path);
        let indexed = index_by_path.get(&canonical);
        match scan_summary(&info, indexed) {
            Ok(summary) => sessions.push(summary),
            Err(_) => continue,
        }
    }

    for summary in sessions.iter_mut() {
        if let Some(count) = subagent_counts.get(&summary.id) {
            summary.subagent_count = *count;
        }
    }

    sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    sessions.truncate(LIST_LIMIT);
    Ok(sessions)
}

pub fn read_session_detail(path: &Path) -> Result<SessionDetail> {
    let info = path_info(path);
    let index_by_path = read_indexed_paths(&claude_root()?).unwrap_or_default();
    let canonical = canonical_or(path);
    let indexed = index_by_path.get(&canonical).cloned();

    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;

    let mut summary = SessionSummary::empty(
        derive_session_id(&info),
        SessionSource::Claude,
        path.to_string_lossy().into_owned(),
    );
    summary.is_sidechain = info.is_subagent;
    summary.parent_session_id = info.parent_session_id.clone();

    let mut stats = SessionStats::new(
        path.to_string_lossy().into_owned(),
        metadata_timestamp(path)?,
    );

    let mut events = Vec::new();
    let mut first_prompt: Option<String> = None;
    let mut ai_title: Option<String> = None;
    let mut current_cwd: Option<String> = None;
    let mut current_git_branch: Option<String> = None;
    let mut current_model: Option<String> = None;
    let mut assistant_slots: HashMap<String, usize> = HashMap::new();

    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
            if !cwd.is_empty() {
                current_cwd = Some(cwd.to_string());
            }
        }
        if let Some(branch) = value.get("gitBranch").and_then(Value::as_str) {
            if !branch.is_empty() {
                current_git_branch = Some(branch.to_string());
            }
        }
        if stats.started_at.is_none() {
            stats.started_at = timestamp.clone();
        }
        if summary.parent_session_id.is_none() {
            if let Some(parent) = value.get("parentSessionId").and_then(Value::as_str) {
                summary.parent_session_id = Some(parent.to_string());
            }
        }
        if let Some(sid) = value.get("sessionId").and_then(Value::as_str) {
            if info.is_subagent {
                summary.parent_session_id.get_or_insert(sid.to_string());
            } else if summary.id == file_stem_id(path) {
                // prefer the embedded sessionId over the file stem when they differ
                summary.id = sid.to_string();
            }
        }

        match event_type {
            "user" => {
                if value.get("isMeta").and_then(Value::as_bool) == Some(true) {
                    continue;
                }
                let content = value.pointer("/message/content");
                let Some(content) = content else { continue };
                handle_user_content(
                    line_no,
                    content,
                    timestamp.clone(),
                    &mut events,
                    &mut stats,
                    &mut first_prompt,
                );
            }
            "assistant" => {
                handle_assistant(
                    line_no,
                    &value,
                    timestamp.clone(),
                    &mut events,
                    &mut stats,
                    &mut assistant_slots,
                    &mut current_model,
                );
            }
            "system" => {
                let subtype = value
                    .get("subtype")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let level = value
                    .get("level")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let hook_count = value
                    .get("hookCount")
                    .and_then(Value::as_u64)
                    .map(|v| v as u32);
                let content = value
                    .get("hookInfos")
                    .map(pretty_json)
                    .or_else(|| value.get("content").map(pretty_json));
                events.push(SessionEvent::SystemNotice {
                    meta: EventMeta::new(format!("claude-system-{line_no}"), "system")
                        .with_timestamp(timestamp.clone()),
                    subtype,
                    level,
                    content,
                    hook_count,
                });
            }
            "permission-mode" => {
                let mode = value
                    .get("permissionMode")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                if mode.is_empty() {
                    continue;
                }
                events.push(SessionEvent::PermissionMode {
                    meta: EventMeta::new(
                        format!("claude-perm-{line_no}"),
                        "permission-mode",
                    )
                    .with_timestamp(timestamp.clone()),
                    mode,
                });
            }
            "last-prompt" => {
                let text = value
                    .get("lastPrompt")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                if first_prompt.is_none() {
                    if let Some(t) = &text {
                        first_prompt = Some(t.clone());
                    }
                }
                events.push(SessionEvent::Lifecycle {
                    meta: EventMeta::new(format!("claude-last-{line_no}"), "last-prompt")
                        .with_timestamp(timestamp.clone()),
                    event: "last-prompt".to_string(),
                    text,
                });
            }
            "ai-title" => {
                let title = value
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("aiTitle").and_then(Value::as_str))
                    .map(ToOwned::to_owned);
                if let Some(t) = title.clone() {
                    if !t.trim().is_empty() {
                        ai_title = Some(t);
                    }
                }
                events.push(SessionEvent::Lifecycle {
                    meta: EventMeta::new(format!("claude-title-{line_no}"), "ai-title")
                        .with_timestamp(timestamp.clone()),
                    event: "ai-title".to_string(),
                    text: title,
                });
            }
            "attachment" => {
                let attachment = value.get("attachment");
                let name = attachment
                    .and_then(|a| a.get("hookName").or_else(|| a.get("name")))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let mime = attachment
                    .and_then(|a| a.get("mime").or_else(|| a.get("type")))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let preview = attachment
                    .and_then(|a| {
                        a.get("stdout")
                            .or_else(|| a.get("content"))
                            .or_else(|| a.get("stderr"))
                    })
                    .and_then(|v| match v {
                        Value::String(s) => Some(s.to_string()),
                        Value::Array(_) | Value::Object(_) => Some(pretty_json(v)),
                        _ => None,
                    })
                    .map(|s| truncate_chars(&s, 480).0);
                events.push(SessionEvent::Attachment {
                    meta: EventMeta::new(format!("claude-attach-{line_no}"), "attachment")
                        .with_timestamp(timestamp.clone()),
                    name,
                    mime,
                    preview,
                });
            }
            "file-history-snapshot" => {
                let file_count = value
                    .pointer("/snapshot/trackedFileBackups")
                    .and_then(Value::as_object)
                    .map(|obj| obj.len() as u32);
                events.push(SessionEvent::FileSnapshot {
                    meta: EventMeta::new(
                        format!("claude-files-{line_no}"),
                        "file-history-snapshot",
                    )
                    .with_timestamp(timestamp.clone()),
                    file_count,
                });
            }
            "queue-operation" => {
                let text = value.get("content").and_then(Value::as_str).map(|s| {
                    let (truncated, _) = truncate_chars(s, RESULT_TRUNCATE_CHARS);
                    truncated
                });
                events.push(SessionEvent::Lifecycle {
                    meta: EventMeta::new(
                        format!("claude-queue-{line_no}"),
                        "queue-operation",
                    )
                    .with_timestamp(timestamp.clone()),
                    event: value
                        .get("operation")
                        .and_then(Value::as_str)
                        .unwrap_or("queue-operation")
                        .to_string(),
                    text,
                });
            }
            other if !other.is_empty() => {
                events.push(SessionEvent::Unknown {
                    meta: EventMeta::new(format!("claude-unk-{line_no}"), other)
                        .with_timestamp(timestamp.clone()),
                    raw_type: other.to_string(),
                });
            }
            _ => {}
        }
    }

    stats.event_count = events.len();
    summary.message_count = stats.user_count + stats.assistant_count;
    summary.user_count = stats.user_count;
    summary.assistant_count = stats.assistant_count;
    summary.tool_call_count = stats.tool_call_count;
    summary.working_dir = current_cwd.clone();
    summary.git_branch = current_git_branch.clone();
    summary.model = current_model.clone();
    summary.project_path = indexed
        .as_ref()
        .and_then(|entry| entry.project_path.clone())
        .or_else(|| project_path_from_dir(path));

    if !info.is_subagent {
        if let Some(title) = ai_title.clone() {
            summary.title = clean_title(Some(&title)).unwrap_or(title);
        } else if let Some(entry) = &indexed {
            summary.title = clean_title(entry.summary.as_deref())
                .or_else(|| clean_title(entry.first_prompt.as_deref()))
                .or_else(|| clean_title(first_prompt.as_deref()))
                .unwrap_or_else(|| summary.id.clone());
        } else {
            summary.title = clean_title(first_prompt.as_deref())
                .unwrap_or_else(|| summary.id.clone());
        }
    } else {
        let prefix = "subagent";
        let agent_label = info
            .agent_id
            .clone()
            .unwrap_or_else(|| "agent".to_string());
        summary.title = clean_title(first_prompt.as_deref())
            .map(|t| format!("{prefix} {agent_label} · {t}"))
            .unwrap_or_else(|| format!("{prefix} {agent_label}"));
        summary.agent_nickname = Some(agent_label);
    }

    summary.preview = clean_preview(first_prompt.as_deref().unwrap_or(""));
    summary.updated_at = indexed
        .as_ref()
        .and_then(|entry| entry.modified.clone().or_else(|| entry.created.clone()))
        .unwrap_or_else(|| stats.updated_at.clone());
    summary.is_partial = false;

    Ok(SessionDetail {
        summary,
        events,
        stats,
    })
}

fn handle_user_content(
    line_no: usize,
    content: &Value,
    timestamp: Option<String>,
    events: &mut Vec<SessionEvent>,
    stats: &mut SessionStats,
    first_prompt: &mut Option<String>,
) {
    match content {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                return;
            }
            let text = trimmed.to_string();
            if first_prompt.is_none() {
                *first_prompt = Some(text.clone());
            }
            stats.user_count += 1;
            stats.content_chars += text.chars().count();
            events.push(SessionEvent::User {
                meta: EventMeta::new(format!("claude-user-{line_no}"), "user")
                    .with_timestamp(timestamp),
                text,
            });
        }
        Value::Array(items) => {
            let mut text_parts = Vec::new();
            let mut tool_results = Vec::new();
            for item in items {
                let item_type = item.get("type").and_then(Value::as_str);
                match item_type {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                text_parts.push(trimmed.to_string());
                            }
                        }
                    }
                    Some("tool_result") => {
                        tool_results.push(item.clone());
                    }
                    Some("image") => {
                        text_parts.push("[image]".to_string());
                    }
                    _ => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                text_parts.push(trimmed.to_string());
                            }
                        }
                    }
                }
            }

            if !text_parts.is_empty() {
                let text = text_parts.join("\n\n");
                if first_prompt.is_none() {
                    *first_prompt = Some(text.clone());
                }
                stats.user_count += 1;
                stats.content_chars += text.chars().count();
                events.push(SessionEvent::User {
                    meta: EventMeta::new(format!("claude-user-{line_no}"), "user")
                        .with_timestamp(timestamp.clone()),
                    text,
                });
            }

            for (idx, item) in tool_results.iter().enumerate() {
                let call_id = item
                    .get("tool_use_id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let is_error = item
                    .get("is_error")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                let content_text = extract_tool_result_content(item.get("content"));
                let (content_text, _) = truncate_chars(&content_text, RESULT_TRUNCATE_CHARS);
                stats.content_chars += content_text.chars().count();
                events.push(SessionEvent::ToolResult {
                    meta: EventMeta::new(
                        format!("claude-toolres-{line_no}-{idx}"),
                        "tool_result",
                    )
                    .with_timestamp(timestamp.clone()),
                    call_id,
                    content: content_text,
                    is_error,
                });
            }
        }
        _ => {}
    }
}

fn handle_assistant(
    line_no: usize,
    value: &Value,
    timestamp: Option<String>,
    events: &mut Vec<SessionEvent>,
    stats: &mut SessionStats,
    assistant_slots: &mut HashMap<String, usize>,
    current_model: &mut Option<String>,
) {
    let message = match value.get("message") {
        Some(m) => m,
        None => return,
    };
    let message_id = message
        .get("id")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let model = message
        .get("model")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let stop_reason = message
        .get("stop_reason")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    if let Some(m) = &model {
        *current_model = Some(m.clone());
    }

    if let Some(usage) = message.get("usage") {
        let input_tokens = usage.get("input_tokens").and_then(Value::as_u64);
        let output_tokens = usage.get("output_tokens").and_then(Value::as_u64);
        let cache_read = usage.get("cache_read_input_tokens").and_then(Value::as_u64);
        let cache_creation = usage
            .get("cache_creation_input_tokens")
            .and_then(Value::as_u64);

        stats.total_input_tokens += input_tokens.unwrap_or(0);
        stats.total_output_tokens += output_tokens.unwrap_or(0);
        stats.cache_read_tokens += cache_read.unwrap_or(0);
        stats.cache_creation_tokens += cache_creation.unwrap_or(0);

        if input_tokens.is_some()
            || output_tokens.is_some()
            || cache_read.is_some()
            || cache_creation.is_some()
        {
            events.push(SessionEvent::TokenUsage {
                meta: EventMeta::new(format!("claude-tokens-{line_no}"), "usage")
                    .with_timestamp(timestamp.clone()),
                input_tokens,
                output_tokens,
                cache_read_tokens: cache_read,
                cache_creation_tokens: cache_creation,
                rate_limit: usage.get("rate_limits").cloned(),
            });
        }
    }

    let items = match message.get("content").and_then(Value::as_array) {
        Some(items) => items,
        None => return,
    };

    let mut text_parts = Vec::new();
    let mut reasoning_parts = Vec::new();
    let mut encrypted_reasoning = false;

    for item in items {
        let item_type = item.get("type").and_then(Value::as_str);
        match item_type {
            Some("text") => {
                if let Some(text) = item.get("text").and_then(Value::as_str) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        text_parts.push(trimmed.to_string());
                    }
                }
            }
            Some("thinking") => {
                if let Some(text) = item.get("thinking").and_then(Value::as_str) {
                    let trimmed = text.trim();
                    if !trimmed.is_empty() {
                        reasoning_parts.push(trimmed.to_string());
                    } else if item
                        .get("signature")
                        .and_then(Value::as_str)
                        .map(|s| !s.is_empty())
                        .unwrap_or(false)
                    {
                        encrypted_reasoning = true;
                    }
                }
            }
            Some("tool_use") => {
                stats.tool_call_count += 1;
                let name = item
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string();
                let call_id = item
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
                let input = item.get("input").cloned().unwrap_or(Value::Null);
                let summary = summarize_tool_use(&name, &input);
                events.push(SessionEvent::ToolCall {
                    meta: EventMeta::new(format!("claude-tool-{line_no}"), "tool_use")
                        .with_timestamp(timestamp.clone()),
                    call_id,
                    name,
                    input,
                    summary,
                });
            }
            _ => {}
        }
    }

    if !reasoning_parts.is_empty() || encrypted_reasoning {
        let text = if reasoning_parts.is_empty() {
            "(encrypted reasoning)".to_string()
        } else {
            reasoning_parts.join("\n\n")
        };
        stats.reasoning_count += 1;
        stats.content_chars += text.chars().count();
        events.push(SessionEvent::Reasoning {
            meta: EventMeta::new(format!("claude-thinking-{line_no}"), "thinking")
                .with_timestamp(timestamp.clone()),
            text,
            encrypted: encrypted_reasoning && reasoning_parts.is_empty(),
        });
    }

    if text_parts.is_empty() {
        return;
    }

    let text = text_parts.join("\n\n");
    stats.content_chars += text.chars().count();

    if let Some(id) = &message_id {
        if let Some(slot) = assistant_slots.get(id).copied() {
            if let Some(SessionEvent::AssistantMessage {
                text: ref mut existing,
                ..
            }) = events.get_mut(slot)
            {
                if !existing.is_empty() {
                    existing.push_str("\n\n");
                }
                existing.push_str(&text);
            }
            return;
        }
    }

    let id_meta = message_id
        .clone()
        .unwrap_or_else(|| format!("claude-assistant-{line_no}"));
    stats.assistant_count += 1;
    let slot = events.len();
    events.push(SessionEvent::AssistantMessage {
        meta: EventMeta::new(id_meta, "assistant").with_timestamp(timestamp),
        text,
        model,
        stop_reason,
    });
    if let Some(id) = message_id {
        assistant_slots.insert(id, slot);
    }
}

fn summarize_tool_use(name: &str, input: &Value) -> Option<String> {
    match name {
        "Bash" => input
            .get("command")
            .and_then(Value::as_str)
            .map(|cmd| format!("$ {cmd}")),
        "Read" => input
            .get("file_path")
            .and_then(Value::as_str)
            .map(|p| format!("read {p}")),
        "Write" => input
            .get("file_path")
            .and_then(Value::as_str)
            .map(|p| format!("write {p}")),
        "Edit" | "MultiEdit" => input
            .get("file_path")
            .and_then(Value::as_str)
            .map(|p| format!("edit {p}")),
        "Glob" => input
            .get("pattern")
            .and_then(Value::as_str)
            .map(|p| format!("glob {p}")),
        "Grep" => input
            .get("pattern")
            .and_then(Value::as_str)
            .map(|p| format!("grep {p}")),
        "TodoWrite" => input
            .get("todos")
            .and_then(Value::as_array)
            .map(|todos| {
                let listed = todos
                    .iter()
                    .filter_map(|t| t.get("content").and_then(Value::as_str))
                    .take(8)
                    .collect::<Vec<_>>()
                    .join("\n- ");
                if listed.is_empty() {
                    "update todos".to_string()
                } else {
                    format!("todos\n- {listed}")
                }
            }),
        "Task" | "Agent" => input
            .get("prompt")
            .and_then(Value::as_str)
            .map(|p| {
                let (preview, _) = truncate_chars(p, 200);
                format!("delegate task\n{preview}")
            })
            .or_else(|| {
                input
                    .get("description")
                    .and_then(Value::as_str)
                    .map(|d| format!("delegate task\n{d}"))
            }),
        "WebFetch" => input
            .get("url")
            .and_then(Value::as_str)
            .map(|u| format!("fetch {u}")),
        "WebSearch" => input
            .get("query")
            .and_then(Value::as_str)
            .map(|q| format!("web search\n{q}")),
        "Skill" => input
            .get("skill")
            .and_then(Value::as_str)
            .map(|s| format!("invoke skill: {s}")),
        _ => None,
    }
}

fn extract_tool_result_content(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    match content {
        Value::String(text) => text.trim().to_string(),
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                parts.push(trimmed.to_string());
                            }
                        }
                    }
                    Some("image") => parts.push("[image]".to_string()),
                    _ => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                parts.push(trimmed.to_string());
                            }
                        }
                    }
                }
            }
            parts.join("\n\n")
        }
        Value::Object(_) => pretty_json(content),
        _ => String::new(),
    }
}

fn scan_summary(info: &PathInfo, indexed: Option<&IndexEntry>) -> Result<SessionSummary> {
    let path = &info.path;
    let mut summary = SessionSummary::empty(
        derive_session_id(info),
        SessionSource::Claude,
        path.to_string_lossy().into_owned(),
    );
    summary.updated_at = metadata_timestamp(path)?;
    summary.is_sidechain = info.is_subagent;
    summary.parent_session_id = info.parent_session_id.clone();
    summary.project_path = indexed
        .and_then(|entry| entry.project_path.clone())
        .or_else(|| project_path_from_dir(path));

    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut first_prompt: Option<String> = None;
    let mut ai_title: Option<String> = None;
    let mut user_count = 0usize;
    let mut assistant_count = 0usize;
    let mut tool_call_count = 0usize;
    let mut current_cwd: Option<String> = None;
    let mut current_git_branch: Option<String> = None;
    let mut current_model: Option<String> = None;
    let mut seen_assistant_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if let Some(cwd) = value.get("cwd").and_then(Value::as_str) {
            if !cwd.is_empty() {
                current_cwd = Some(cwd.to_string());
            }
        }
        if let Some(branch) = value.get("gitBranch").and_then(Value::as_str) {
            if !branch.is_empty() {
                current_git_branch = Some(branch.to_string());
            }
        }
        if let Some(sid) = value.get("sessionId").and_then(Value::as_str) {
            if !info.is_subagent && summary.id == file_stem_id(path) {
                summary.id = sid.to_string();
            }
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
                        let trimmed = text.trim();
                        if trimmed.is_empty() {
                            continue;
                        }
                        user_count += 1;
                        if first_prompt.is_none() {
                            first_prompt = Some(trimmed.to_string());
                        }
                    }
                    Value::Array(items) => {
                        let text_parts: Vec<&str> = items
                            .iter()
                            .filter(|item| {
                                item.get("type").and_then(Value::as_str) == Some("text")
                            })
                            .filter_map(|item| item.get("text").and_then(Value::as_str))
                            .map(str::trim)
                            .filter(|t| !t.is_empty())
                            .collect();
                        if !text_parts.is_empty() {
                            user_count += 1;
                            if first_prompt.is_none() {
                                first_prompt = Some(text_parts.join("\n\n"));
                            }
                        }
                    }
                    _ => {}
                }
            }
            Some("assistant") => {
                let message = value.get("message");
                if let Some(model) = message
                    .and_then(|m| m.get("model"))
                    .and_then(Value::as_str)
                {
                    current_model = Some(model.to_string());
                }
                let items = message
                    .and_then(|m| m.get("content"))
                    .and_then(Value::as_array);
                let Some(items) = items else { continue };

                let mut has_text = false;
                for item in items {
                    match item.get("type").and_then(Value::as_str) {
                        Some("text") => {
                            if item
                                .get("text")
                                .and_then(Value::as_str)
                                .map(|t| !t.trim().is_empty())
                                .unwrap_or(false)
                            {
                                has_text = true;
                            }
                        }
                        Some("tool_use") => tool_call_count += 1,
                        _ => {}
                    }
                }

                if has_text {
                    let message_id = message
                        .and_then(|m| m.get("id"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    let counted = match message_id {
                        Some(id) => seen_assistant_ids.insert(id),
                        None => true,
                    };
                    if counted {
                        assistant_count += 1;
                    }
                }
            }
            Some("ai-title") => {
                let title = value
                    .get("title")
                    .and_then(Value::as_str)
                    .or_else(|| value.get("aiTitle").and_then(Value::as_str));
                if let Some(t) = title {
                    if !t.trim().is_empty() {
                        ai_title = Some(t.to_string());
                    }
                }
            }
            Some("last-prompt") => {
                if first_prompt.is_none() {
                    first_prompt = value
                        .get("lastPrompt")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                }
            }
            _ => {}
        }
    }

    summary.working_dir = current_cwd;
    summary.git_branch = current_git_branch;
    summary.model = current_model;
    summary.message_count = user_count + assistant_count;
    summary.user_count = user_count;
    summary.assistant_count = assistant_count;
    summary.tool_call_count = tool_call_count;
    summary.is_partial = false;

    if !info.is_subagent {
        if let Some(title) = ai_title {
            summary.title = clean_title(Some(&title)).unwrap_or(title);
        } else if let Some(entry) = indexed {
            summary.title = clean_title(entry.summary.as_deref())
                .or_else(|| clean_title(entry.first_prompt.as_deref()))
                .or_else(|| clean_title(first_prompt.as_deref()))
                .unwrap_or_else(|| summary.id.clone());
        } else {
            summary.title = clean_title(first_prompt.as_deref())
                .unwrap_or_else(|| summary.id.clone());
        }
    } else {
        let agent_label = info
            .agent_id
            .clone()
            .unwrap_or_else(|| "agent".to_string());
        summary.title = clean_title(first_prompt.as_deref())
            .map(|t| format!("subagent {agent_label} · {t}"))
            .unwrap_or_else(|| format!("subagent {agent_label}"));
        summary.agent_nickname = Some(agent_label);
    }

    summary.preview = clean_preview(first_prompt.as_deref().unwrap_or(""));
    summary.updated_at = indexed
        .and_then(|entry| entry.modified.clone().or_else(|| entry.created.clone()))
        .unwrap_or(summary.updated_at);
    Ok(summary)
}

fn collect_session_files(root: &Path) -> Result<Vec<PathInfo>> {
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
        files.push(path_info(path));
    }
    Ok(files)
}

fn path_info(path: &Path) -> PathInfo {
    let components: Vec<_> = path
        .components()
        .filter_map(|c| c.as_os_str().to_str().map(|s| s.to_string()))
        .collect();
    let is_subagent = components.iter().any(|c| c == "subagents");
    let parent_session_id = if is_subagent {
        components
            .iter()
            .rev()
            .skip_while(|c| c.as_str() != "subagents")
            .nth(1)
            .cloned()
    } else {
        None
    };
    let agent_id = if is_subagent {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.trim_start_matches("agent-").to_string())
    } else {
        None
    };
    PathInfo {
        path: path.to_path_buf(),
        is_subagent,
        parent_session_id,
        agent_id,
    }
}

fn derive_session_id(info: &PathInfo) -> String {
    if info.is_subagent {
        let agent = info.agent_id.clone().unwrap_or_else(|| "agent".to_string());
        match &info.parent_session_id {
            Some(parent) => format!("{parent}::agent-{agent}"),
            None => format!("agent-{agent}"),
        }
    } else {
        file_stem_id(&info.path)
    }
}

fn project_path_from_dir(path: &Path) -> Option<String> {
    let mut comps = path.components();
    while let Some(c) = comps.next() {
        if c.as_os_str() == "projects" {
            if let Some(slug) = comps.next() {
                return Some(decode_project_slug(slug.as_os_str().to_str()?));
            }
        }
    }
    None
}

fn decode_project_slug(slug: &str) -> String {
    if slug.starts_with('-') {
        slug.replace('-', "/")
    } else {
        slug.to_string()
    }
}

fn canonical_or(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn read_indexed_paths(root: &Path) -> Result<HashMap<PathBuf, IndexEntry>> {
    let mut by_path = HashMap::new();
    if !root.exists() {
        return Ok(by_path);
    }
    for entry in WalkDir::new(root)
        .into_iter()
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_type().is_file())
    {
        if entry.file_name().to_str() != Some("sessions-index.json") {
            continue;
        }
        let file = match File::open(entry.path()) {
            Ok(f) => f,
            Err(_) => continue,
        };
        let parsed: IndexFile = match serde_json::from_reader(file) {
            Ok(p) => p,
            Err(_) => continue,
        };
        for record in parsed.entries {
            if let Some(full) = record.full_path.clone() {
                let key = canonical_or(Path::new(&full));
                by_path.insert(key, record);
            }
        }
    }
    Ok(by_path)
}
