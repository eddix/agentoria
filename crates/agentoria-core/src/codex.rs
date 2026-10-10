use std::{
    collections::HashMap,
    fs::File,
    io::{BufRead, BufReader},
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use serde_json::Value;
use walkdir::WalkDir;

use super::model::*;
use super::util::*;

const LIST_LIMIT: usize = 1000;
const RESULT_TRUNCATE_CHARS: usize = 24_000;

#[derive(Debug, Default, Clone)]
struct IndexEntry {
    title: Option<String>,
    updated_at: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AssistantRecordSource {
    AgentMessage,
    ResponseItem,
}

#[derive(Debug)]
struct AssistantRecord {
    source: AssistantRecordSource,
    text: String,
    id: Option<String>,
    stop_reason: Option<String>,
}

impl AssistantRecord {
    fn source_type(&self) -> &'static str {
        match self.source {
            AssistantRecordSource::AgentMessage => "agent_message",
            AssistantRecordSource::ResponseItem => "response_item.message",
        }
    }

    fn duplicates(&self, previous: &Self) -> bool {
        self.source != previous.source && self.text == previous.text
    }
}

pub fn list_sessions() -> Result<Vec<SessionSummary>> {
    let index = read_index()?;
    let sessions_root = codex_root()?.join("sessions");

    if !sessions_root.exists() {
        return Ok(Vec::new());
    }

    let path_index = build_path_index(&sessions_root);

    // Hybrid scan: always walk every jsonl on disk, then enrich from the index
    // when present. Codex evicts older sessions from `session_index.jsonl`, and
    // an index-only loop would hide those historical files entirely.
    let mut sessions: Vec<SessionSummary> = Vec::with_capacity(path_index.len());
    for path in path_index.values() {
        let id = file_stem_id(path);
        let indexed = index.get(&id);
        if let Ok(summary) = scan_summary(path, &index, indexed) {
            sessions.push(summary);
        }
    }

    sessions.sort_by(|left, right| right.updated_at.cmp(&left.updated_at));
    sessions.truncate(LIST_LIMIT);
    Ok(sessions)
}

pub fn read_session_detail(path: &Path) -> Result<SessionDetail> {
    let index = read_index()?;
    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let file_updated_at = metadata_timestamp(path).ok();
    let indexed_updated_at = index
        .get(&file_stem_id(path))
        .and_then(|entry| entry.updated_at.as_deref());

    let mut summary = SessionSummary::empty(
        file_stem_id(path),
        SessionSource::Codex,
        path.to_string_lossy().into_owned(),
    );
    let mut stats = SessionStats::new(
        path.to_string_lossy().into_owned(),
        resolve_session_updated_at(None, file_updated_at.clone(), indexed_updated_at),
    );

    let mut events = Vec::new();
    let mut first_prompt: Option<String> = None;
    let mut last_prompt: Option<String> = None;
    let mut last_event_at: Option<String> = None;
    let mut current_model: Option<String> = None;
    let mut current_cwd: Option<String> = None;
    let mut previous_assistant: Option<AssistantRecord> = None;

    for (line_no, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        if line.trim().is_empty() {
            previous_assistant = None;
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                previous_assistant = None;
                continue;
            }
        };

        let top_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let top_timestamp = value
            .get("timestamp")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
        let payload = value.get("payload");
        update_latest_timestamp(
            &mut last_event_at,
            top_timestamp.as_deref().or_else(|| {
                payload
                    .and_then(|item| item.get("timestamp"))
                    .and_then(Value::as_str)
            }),
        );
        let payload_type = payload
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");

        if let Some(record) = extract_assistant_record(&value) {
            let is_duplicate = previous_assistant
                .as_ref()
                .is_some_and(|previous| record.duplicates(previous));

            if !is_duplicate {
                stats.assistant_count += 1;
                stats.content_chars += record.text.chars().count();
                events.push(SessionEvent::AssistantMessage {
                    meta: EventMeta::new(
                        record
                            .id
                            .clone()
                            .unwrap_or_else(|| format!("codex-agent-{line_no}")),
                        record.source_type(),
                    )
                    .with_timestamp(top_timestamp.clone()),
                    text: record.text.clone(),
                    model: current_model.clone(),
                    stop_reason: record.stop_reason.clone(),
                });
            }

            previous_assistant = Some(record);
            continue;
        }
        previous_assistant = None;

        match top_type {
            "session_meta" => {
                if let Some(p) = payload {
                    apply_session_meta(p, &mut summary, &mut current_model, &mut current_cwd);
                    if stats.started_at.is_none() {
                        stats.started_at = p
                            .get("timestamp")
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned)
                            .or_else(|| top_timestamp.clone());
                    }
                }
            }
            "turn_context" => {
                if let Some(p) = payload {
                    let cwd = p.get("cwd").and_then(Value::as_str).map(ToOwned::to_owned);
                    let model = p
                        .get("model")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    let sandbox = p
                        .pointer("/sandbox_policy/type")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    let approval = p
                        .get("approval_policy")
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);

                    if let Some(c) = &cwd {
                        current_cwd = Some(c.clone());
                    }
                    if let Some(m) = &model {
                        current_model = Some(m.clone());
                    }

                    let summary_text = format_turn_context(&cwd, &model, &sandbox, &approval);

                    events.push(SessionEvent::TurnContext {
                        meta: EventMeta::new(format!("codex-turn-{line_no}"), "turn_context")
                            .with_timestamp(top_timestamp.clone()),
                        cwd,
                        model,
                        sandbox,
                        approval,
                        summary: summary_text,
                    });
                }
            }
            "compacted" | "event_msg"
                if payload_type == "context_compacted" || top_type == "compacted" =>
            {
                let summary_text = payload
                    .and_then(|p| p.get("summary"))
                    .or_else(|| value.get("summary"))
                    .and_then(extract_codex_text)
                    .map(|s| clean_preview(&s))
                    .filter(|s| !s.is_empty());
                events.push(SessionEvent::Compaction {
                    meta: EventMeta::new(
                        format!("codex-compact-{line_no}"),
                        if top_type == "compacted" {
                            "compacted"
                        } else {
                            "context_compacted"
                        },
                    )
                    .with_timestamp(top_timestamp.clone()),
                    summary: summary_text,
                });
            }
            "event_msg" => match payload_type {
                "user_message" => {
                    let text = payload
                        .and_then(|p| p.get("message"))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if text.is_empty() {
                        continue;
                    }
                    if first_prompt.is_none() {
                        first_prompt = Some(text.clone());
                    }
                    if is_meaningful_prompt(&text) {
                        last_prompt = Some(text.clone());
                    }
                    stats.user_count += 1;
                    stats.content_chars += text.chars().count();
                    events.push(SessionEvent::User {
                        meta: EventMeta::new(format!("codex-user-{line_no}"), "user_message")
                            .with_timestamp(top_timestamp.clone()),
                        text,
                    });
                }
                "agent_message" => {}
                "agent_reasoning" => {
                    let text = payload
                        .and_then(|p| p.get("text").or_else(|| p.get("message")))
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string();
                    if text.is_empty() {
                        continue;
                    }
                    stats.reasoning_count += 1;
                    stats.content_chars += text.chars().count();
                    events.push(SessionEvent::Reasoning {
                        meta: EventMeta::new(format!("codex-areason-{line_no}"), "agent_reasoning")
                            .with_timestamp(top_timestamp.clone()),
                        text,
                        encrypted: false,
                    });
                }
                "task_started" | "task_complete" | "turn_aborted" => {
                    let text = payload
                        .and_then(|p| {
                            p.get("text")
                                .or_else(|| p.get("message"))
                                .or_else(|| p.get("reason"))
                        })
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    events.push(SessionEvent::Lifecycle {
                        meta: EventMeta::new(
                            format!("codex-life-{line_no}"),
                            payload_type.to_string(),
                        )
                        .with_timestamp(top_timestamp.clone()),
                        event: payload_type.to_string(),
                        text,
                    });
                }
                "token_count" => {
                    let info = payload.and_then(|p| p.get("info"));
                    let input_tokens = info
                        .and_then(|i| i.get("input_tokens"))
                        .and_then(Value::as_u64);
                    let output_tokens = info
                        .and_then(|i| i.get("output_tokens"))
                        .and_then(Value::as_u64);
                    let cache_read = info
                        .and_then(|i| i.get("cached_input_tokens"))
                        .and_then(Value::as_u64);
                    let cache_creation = info
                        .and_then(|i| i.get("cache_creation_input_tokens"))
                        .and_then(Value::as_u64);

                    stats.total_input_tokens += input_tokens.unwrap_or(0);
                    stats.total_output_tokens += output_tokens.unwrap_or(0);
                    stats.cache_read_tokens += cache_read.unwrap_or(0);
                    stats.cache_creation_tokens += cache_creation.unwrap_or(0);

                    events.push(SessionEvent::TokenUsage {
                        meta: EventMeta::new(format!("codex-tokens-{line_no}"), "token_count")
                            .with_timestamp(top_timestamp.clone()),
                        input_tokens,
                        output_tokens,
                        cache_read_tokens: cache_read,
                        cache_creation_tokens: cache_creation,
                        rate_limit: payload.and_then(|p| p.get("rate_limits")).cloned(),
                    });
                }
                "guardian_assessment" => {
                    let text = payload.map(pretty_json);
                    events.push(SessionEvent::SystemNotice {
                        meta: EventMeta::new(
                            format!("codex-guardian-{line_no}"),
                            "guardian_assessment",
                        )
                        .with_timestamp(top_timestamp.clone()),
                        subtype: Some("guardian_assessment".to_string()),
                        level: payload
                            .and_then(|p| p.get("level"))
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        content: text,
                        hook_count: None,
                    });
                }
                "patch_apply_end" | "exec_command_end" => {
                    // duplicate of response_item.function_call_output — skip
                }
                _ => {
                    events.push(SessionEvent::Unknown {
                        meta: EventMeta::new(format!("codex-unk-{line_no}"), payload_type)
                            .with_timestamp(top_timestamp.clone()),
                        raw_type: format!("event_msg.{payload_type}"),
                    });
                }
            },
            "response_item" => match payload_type {
                "message" => {}
                "reasoning" => {
                    let summary_text = payload
                        .and_then(|p| p.get("summary"))
                        .and_then(extract_codex_text)
                        .filter(|s| !s.is_empty());
                    let content_text = payload
                        .and_then(|p| p.get("content"))
                        .and_then(extract_codex_text)
                        .filter(|s| !s.is_empty());
                    let has_encrypted = payload
                        .and_then(|p| p.get("encrypted_content"))
                        .and_then(Value::as_str)
                        .map(|s| !s.is_empty())
                        .unwrap_or(false);

                    let text = content_text.or(summary_text).unwrap_or_else(|| {
                        if has_encrypted {
                            "(encrypted reasoning)".to_string()
                        } else {
                            String::new()
                        }
                    });
                    if text.is_empty() {
                        continue;
                    }
                    stats.reasoning_count += 1;
                    stats.content_chars += text.chars().count();
                    events.push(SessionEvent::Reasoning {
                        meta: EventMeta::new(
                            format!("codex-reason-{line_no}"),
                            "response_item.reasoning",
                        )
                        .with_timestamp(top_timestamp.clone()),
                        text,
                        encrypted: has_encrypted,
                    });
                }
                "function_call" | "custom_tool_call" => {
                    stats.tool_call_count += 1;
                    let name = payload
                        .and_then(|p| p.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or("tool")
                        .to_string();
                    let call_id = payload
                        .and_then(|p| p.get("call_id"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    let raw_args = payload
                        .and_then(|p| p.get("arguments").or_else(|| p.get("input")))
                        .and_then(Value::as_str);
                    let parsed_args = raw_args
                        .and_then(|raw| serde_json::from_str::<Value>(raw).ok())
                        .unwrap_or_else(|| {
                            payload
                                .and_then(|p| p.get("arguments").or_else(|| p.get("input")))
                                .cloned()
                                .unwrap_or(Value::Null)
                        });
                    let summary_text = summarize_tool_call(&name, &parsed_args);
                    events.push(SessionEvent::ToolCall {
                        meta: EventMeta::new(
                            format!("codex-tool-{line_no}"),
                            format!("response_item.{payload_type}"),
                        )
                        .with_timestamp(top_timestamp.clone()),
                        call_id,
                        name,
                        input: parsed_args,
                        summary: summary_text,
                    });
                }
                "function_call_output" | "custom_tool_call_output" => {
                    let call_id = payload
                        .and_then(|p| p.get("call_id"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned);
                    let output_raw = payload
                        .and_then(|p| p.get("output"))
                        .and_then(|v| {
                            v.as_str()
                                .map(ToOwned::to_owned)
                                .or_else(|| Some(pretty_json(v)))
                        })
                        .unwrap_or_default();
                    let (content, _truncated) = truncate_chars(&output_raw, RESULT_TRUNCATE_CHARS);
                    let is_error = payload
                        .and_then(|p| p.get("is_error").or_else(|| p.get("error")))
                        .and_then(Value::as_bool)
                        .unwrap_or(false);
                    stats.content_chars += content.chars().count();
                    events.push(SessionEvent::ToolResult {
                        meta: EventMeta::new(
                            format!("codex-toolres-{line_no}"),
                            format!("response_item.{payload_type}"),
                        )
                        .with_timestamp(top_timestamp.clone()),
                        call_id,
                        content,
                        is_error,
                    });
                }
                "web_search_call" => {
                    stats.tool_call_count += 1;
                    let query = payload
                        .and_then(|p| p.get("action").and_then(|a| a.get("query")))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                        .or_else(|| {
                            payload
                                .and_then(|p| p.get("query"))
                                .and_then(Value::as_str)
                                .map(ToOwned::to_owned)
                        });
                    events.push(SessionEvent::ToolCall {
                        meta: EventMeta::new(
                            format!("codex-search-{line_no}"),
                            "response_item.web_search_call",
                        )
                        .with_timestamp(top_timestamp.clone()),
                        call_id: payload
                            .and_then(|p| p.get("id"))
                            .and_then(Value::as_str)
                            .map(ToOwned::to_owned),
                        name: "web_search".to_string(),
                        input: payload.cloned().unwrap_or(Value::Null),
                        summary: query.map(|q| format!("web_search\nquery: {q}")),
                    });
                }
                _ => {
                    events.push(SessionEvent::Unknown {
                        meta: EventMeta::new(format!("codex-unk-{line_no}"), payload_type)
                            .with_timestamp(top_timestamp.clone()),
                        raw_type: format!("response_item.{payload_type}"),
                    });
                }
            },
            _ => {
                if !top_type.is_empty() {
                    events.push(SessionEvent::Unknown {
                        meta: EventMeta::new(format!("codex-unk-{line_no}"), top_type)
                            .with_timestamp(top_timestamp.clone()),
                        raw_type: top_type.to_string(),
                    });
                }
            }
        }
    }

    stats.event_count = events.len();
    summary.message_count = stats.user_count + stats.assistant_count;
    summary.user_count = stats.user_count;
    summary.assistant_count = stats.assistant_count;
    summary.tool_call_count = stats.tool_call_count;
    summary.working_dir = current_cwd.clone();
    if summary.model.is_none() {
        summary.model = current_model.clone();
    }

    let indexed = index.get(&summary.id).cloned().unwrap_or_default();
    let recap = indexed.title.clone();
    summary.title = clean_title(recap.as_deref())
        .or_else(|| clean_title(last_prompt.as_deref()))
        .or_else(|| clean_title(first_prompt.as_deref()))
        .unwrap_or_else(|| summary.id.clone());
    summary.preview = last_prompt
        .as_deref()
        .or(recap.as_deref())
        .or(first_prompt.as_deref())
        .map(clean_preview)
        .unwrap_or_default();
    summary.updated_at = resolve_session_updated_at(
        last_event_at,
        file_updated_at,
        indexed.updated_at.as_deref(),
    );
    stats.updated_at = summary.updated_at.clone();
    summary.is_partial = false;

    Ok(SessionDetail {
        summary,
        events,
        stats,
    })
}

fn scan_summary(
    path: &Path,
    index: &HashMap<String, IndexEntry>,
    indexed: Option<&IndexEntry>,
) -> Result<SessionSummary> {
    let mut summary = SessionSummary::empty(
        file_stem_id(path),
        SessionSource::Codex,
        path.to_string_lossy().into_owned(),
    );
    let file_updated_at = metadata_timestamp(path).ok();

    let file = File::open(path).with_context(|| format!("failed to open {}", path.display()))?;
    let mut first_prompt: Option<String> = None;
    let mut last_prompt: Option<String> = None;
    let mut last_event_at: Option<String> = None;
    let mut user_count = 0usize;
    let mut assistant_count = 0usize;
    let mut tool_call_count = 0usize;
    let mut current_model: Option<String> = None;
    let mut current_cwd: Option<String> = None;
    let mut previous_assistant_source: Option<AssistantRecordSource> = None;

    for line in BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            previous_assistant_source = None;
            continue;
        }
        let value: Value = match serde_json::from_str(&line) {
            Ok(value) => value,
            Err(_) => {
                previous_assistant_source = None;
                continue;
            }
        };

        let top_type = value.get("type").and_then(Value::as_str).unwrap_or("");
        let payload = value.get("payload");
        update_latest_timestamp(
            &mut last_event_at,
            value.get("timestamp").and_then(Value::as_str).or_else(|| {
                payload
                    .and_then(|item| item.get("timestamp"))
                    .and_then(Value::as_str)
            }),
        );
        let payload_type = payload
            .and_then(|p| p.get("type"))
            .and_then(Value::as_str)
            .unwrap_or("");

        if let Some(source) = assistant_record_source(&value) {
            let is_duplicate = previous_assistant_source.is_some_and(|previous| source != previous);
            if !is_duplicate {
                assistant_count += 1;
            }
            previous_assistant_source = Some(source);
            continue;
        }
        previous_assistant_source = None;

        match top_type {
            "session_meta" => {
                if let Some(p) = payload {
                    apply_session_meta(p, &mut summary, &mut current_model, &mut current_cwd);
                }
            }
            "turn_context" => {
                if let Some(p) = payload {
                    if let Some(c) = p.get("cwd").and_then(Value::as_str) {
                        current_cwd = Some(c.to_string());
                    }
                    if let Some(m) = p.get("model").and_then(Value::as_str) {
                        current_model = Some(m.to_string());
                    }
                }
            }
            "event_msg" => match payload_type {
                "user_message" => {
                    user_count += 1;
                    let message = payload
                        .and_then(|p| p.get("message"))
                        .and_then(Value::as_str);
                    if let Some(text) = message {
                        if first_prompt.is_none() {
                            first_prompt = Some(text.to_string());
                        }
                        if is_meaningful_prompt(text) {
                            last_prompt = Some(text.to_string());
                        }
                    }
                }
                "agent_message" => {}
                _ => {}
            },
            "response_item" => match payload_type {
                "message" => {}
                "function_call" | "custom_tool_call" | "web_search_call" => {
                    tool_call_count += 1;
                }
                _ => {}
            },
            _ => {}
        }
    }

    let indexed = indexed
        .cloned()
        .or_else(|| index.get(&summary.id).cloned())
        .unwrap_or_default();
    let recap = indexed.title.clone();
    summary.title = clean_title(recap.as_deref())
        .or_else(|| clean_title(last_prompt.as_deref()))
        .or_else(|| clean_title(first_prompt.as_deref()))
        .unwrap_or_else(|| summary.id.clone());
    summary.preview = last_prompt
        .as_deref()
        .or(recap.as_deref())
        .or(first_prompt.as_deref())
        .map(clean_preview)
        .unwrap_or_default();
    summary.updated_at = resolve_session_updated_at(
        last_event_at,
        file_updated_at,
        indexed.updated_at.as_deref(),
    );
    summary.working_dir = current_cwd;
    if summary.model.is_none() {
        summary.model = current_model;
    }
    summary.message_count = user_count + assistant_count;
    summary.user_count = user_count;
    summary.assistant_count = assistant_count;
    summary.tool_call_count = tool_call_count;
    summary.is_partial = false;
    Ok(summary)
}

fn apply_session_meta(
    payload: &Value,
    summary: &mut SessionSummary,
    current_model: &mut Option<String>,
    current_cwd: &mut Option<String>,
) {
    if let Some(id) = payload.get("id").and_then(Value::as_str) {
        summary.id = id.to_string();
    }
    if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
        *current_cwd = Some(cwd.to_string());
        summary.working_dir = Some(cwd.to_string());
    }
    if let Some(role) = payload.get("agent_role").and_then(Value::as_str) {
        summary.agent_role = Some(role.to_string());
    }
    if let Some(nick) = payload.get("agent_nickname").and_then(Value::as_str) {
        summary.agent_nickname = Some(nick.to_string());
    }
    if let Some(model) = payload.get("model").and_then(Value::as_str) {
        *current_model = Some(model.to_string());
        summary.model = Some(model.to_string());
    }
    if let Some(parent) = payload
        .pointer("/source/subagent/thread_spawn/parent_thread_id")
        .and_then(Value::as_str)
    {
        summary.parent_session_id = Some(parent.to_string());
        summary.is_sidechain = true;
    } else if let Some(forked) = payload.get("forked_from_id").and_then(Value::as_str) {
        summary.parent_session_id = Some(forked.to_string());
    }
}

fn extract_assistant_record(value: &Value) -> Option<AssistantRecord> {
    let payload = value.get("payload")?;
    match assistant_record_source(value)? {
        AssistantRecordSource::AgentMessage => {
            let text = payload.get("message").and_then(Value::as_str)?;
            if text.is_empty() {
                return None;
            }
            Some(AssistantRecord {
                source: AssistantRecordSource::AgentMessage,
                text: text.to_string(),
                id: None,
                stop_reason: payload
                    .get("phase")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
        }
        AssistantRecordSource::ResponseItem => {
            let text = extract_codex_text(payload.get("content").unwrap_or(&Value::Null))?;
            Some(AssistantRecord {
                source: AssistantRecordSource::ResponseItem,
                text,
                id: payload
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                stop_reason: None,
            })
        }
    }
}

fn assistant_record_source(value: &Value) -> Option<AssistantRecordSource> {
    let top_type = value.get("type").and_then(Value::as_str)?;
    let payload = value.get("payload")?;
    let payload_type = payload.get("type").and_then(Value::as_str)?;

    match (top_type, payload_type) {
        ("event_msg", "agent_message") => Some(AssistantRecordSource::AgentMessage),
        ("response_item", "message")
            if payload.get("role").and_then(Value::as_str) == Some("assistant") =>
        {
            Some(AssistantRecordSource::ResponseItem)
        }
        _ => None,
    }
}

fn extract_codex_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => {
            let trimmed = text.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Value::Array(items) => {
            let mut parts = Vec::new();
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("output_text") | Some("input_text") | Some("summary_text")
                    | Some("text") => {
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                parts.push(trimmed.to_string());
                            }
                        }
                    }
                    _ => {
                        // some reasoning content items omit a type
                        if let Some(text) = item.get("text").and_then(Value::as_str) {
                            let trimmed = text.trim();
                            if !trimmed.is_empty() {
                                parts.push(trimmed.to_string());
                            }
                        }
                    }
                }
            }
            if parts.is_empty() {
                None
            } else {
                Some(parts.join("\n\n"))
            }
        }
        _ => None,
    }
}

fn summarize_tool_call(name: &str, args: &Value) -> Option<String> {
    match name {
        "exec_command" => args
            .get("cmd")
            .and_then(Value::as_str)
            .map(|cmd| format!("run command\n{cmd}")),
        "update_plan" => args.get("plan").and_then(Value::as_array).map(|steps| {
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
        }),
        "apply_patch" => Some("apply patch".to_string()),
        _ => None,
    }
}

fn format_turn_context(
    cwd: &Option<String>,
    model: &Option<String>,
    sandbox: &Option<String>,
    approval: &Option<String>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(m) = model {
        parts.push(format!("model: {m}"));
    }
    if let Some(c) = cwd {
        parts.push(format!("cwd: {c}"));
    }
    if let Some(s) = sandbox {
        parts.push(format!("sandbox: {s}"));
    }
    if let Some(a) = approval {
        parts.push(format!("approval: {a}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("  ·  "))
    }
}

fn read_index() -> Result<HashMap<String, IndexEntry>> {
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
            .or_insert_with(IndexEntry::default);
        slot.title = clean_title(value.get("thread_name").and_then(Value::as_str));
        slot.updated_at = value
            .get("updated_at")
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);
    }
    Ok(index)
}

pub(crate) fn session_titles() -> Result<HashMap<PathBuf, String>> {
    let index = read_index()?;
    let sessions_root = codex_root()?.join("sessions");
    if !sessions_root.exists() {
        return Ok(HashMap::new());
    }
    let paths = build_path_index(&sessions_root);
    Ok(paths
        .into_iter()
        .filter_map(|(id, path)| {
            let title = index.get(&id)?.title.clone()?;
            let canonical = path.canonicalize().unwrap_or(path);
            Some((canonical, title))
        })
        .collect())
}

fn build_path_index(root: &Path) -> HashMap<String, PathBuf> {
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

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::extract_assistant_record;

    #[test]
    fn assistant_record_deduplicates_adjacent_alternate_sources() {
        let event_message = extract_assistant_record(&json!({
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "message": "same answer"
            }
        }))
        .expect("agent message should parse");
        let response_item = extract_assistant_record(&json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "same answer"}]
            }
        }))
        .expect("response item should parse");

        assert!(response_item.duplicates(&event_message));
    }

    #[test]
    fn assistant_record_keeps_different_answers() {
        let event_message = extract_assistant_record(&json!({
            "type": "event_msg",
            "payload": {
                "type": "agent_message",
                "message": "first answer"
            }
        }))
        .expect("agent message should parse");
        let response_item = extract_assistant_record(&json!({
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "second answer"}]
            }
        }))
        .expect("response item should parse");

        assert!(!response_item.duplicates(&event_message));
    }
}
