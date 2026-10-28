use serde_json::Value;
use sha2::{Digest, Sha256};

use super::ConversationKind;
use crate::{EventMeta, SessionEvent, SessionSource};

#[derive(Debug)]
pub(super) struct LineDescriptor {
    pub event_type: String,
    pub timestamp: Option<String>,
    pub conversation_kind: Option<ConversationKind>,
    pub process_count: u32,
    pub message_key: Option<String>,
    pub message_hash: Option<Vec<u8>>,
    pub assistant_source: Option<String>,
    pub session_id: Option<String>,
    pub title: Option<String>,
    pub working_dir: Option<String>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub parent_session_id: Option<String>,
    pub is_sidechain: bool,
}

pub(super) fn classify_line(source: SessionSource, value: &Value) -> LineDescriptor {
    match source {
        SessionSource::Claude => classify_claude(value),
        SessionSource::Codex => classify_codex(value),
        SessionSource::Grok => classify_grok(value),
    }
}

pub(super) fn extract_conversation_text(
    source: SessionSource,
    value: &Value,
) -> Option<(ConversationKind, String, Option<String>, Option<String>)> {
    match source {
        SessionSource::Claude => extract_claude_conversation(value),
        SessionSource::Codex => extract_codex_conversation(value),
        SessionSource::Grok => crate::grok::conversation(value),
    }
}

pub(super) fn extract_process_events(
    source: SessionSource,
    value: &Value,
    line_number: u64,
) -> Vec<SessionEvent> {
    match source {
        SessionSource::Claude => extract_claude_process(value, line_number),
        SessionSource::Codex => extract_codex_process(value, line_number),
        SessionSource::Grok => crate::grok::process_events(value, line_number),
    }
}

fn classify_grok(value: &Value) -> LineDescriptor {
    let conversation = crate::grok::conversation(value);
    LineDescriptor {
        event_type: format!("grok.{}", crate::grok::kind(value)),
        timestamp: crate::grok::timestamp(value),
        conversation_kind: conversation.as_ref().map(|(kind, ..)| *kind),
        process_count: crate::grok::process_count(value),
        message_key: crate::grok::message_key(value),
        message_hash: conversation.as_ref().map(|(_, text, ..)| hash_text(text)),
        assistant_source: conversation
            .as_ref()
            .filter(|(kind, ..)| *kind == ConversationKind::Assistant)
            .map(|_| "grok.assistant".to_string()),
        session_id: value
            .pointer("/params/sessionId")
            .and_then(Value::as_str)
            .map(str::to_owned),
        title: conversation
            .as_ref()
            .filter(|(kind, ..)| *kind == ConversationKind::User)
            .and_then(|(_, text, ..)| crate::util::clean_title(Some(text))),
        working_dir: None,
        git_branch: None,
        model: conversation.and_then(|(_, _, model, _)| model),
        parent_session_id: None,
        is_sidechain: false,
    }
}

fn classify_claude(value: &Value) -> LineDescriptor {
    let event_type = value
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string();
    let timestamp = timestamp(value);
    let mut descriptor = LineDescriptor {
        event_type: event_type.clone(),
        timestamp,
        conversation_kind: None,
        process_count: 0,
        message_key: None,
        message_hash: None,
        assistant_source: None,
        session_id: string_field(value, "sessionId"),
        title: None,
        working_dir: string_field(value, "cwd"),
        git_branch: string_field(value, "gitBranch"),
        model: None,
        parent_session_id: string_field(value, "parentSessionId"),
        is_sidechain: false,
    };

    match event_type.as_str() {
        "user" if value.get("isMeta").and_then(Value::as_bool) != Some(true) => {
            if let Some(content) = value.pointer("/message/content") {
                let (text, tool_results) = claude_user_content(content);
                descriptor.process_count = tool_results;
                if let Some(text) = text {
                    descriptor.conversation_kind = Some(ConversationKind::User);
                    descriptor.message_hash = Some(hash_text(&text));
                }
            }
        }
        "assistant" => {
            let Some(message) = value.get("message") else {
                return descriptor;
            };
            descriptor.model = string_field(message, "model");
            descriptor.process_count += u32::from(claude_usage_has_tokens(message.get("usage")));
            descriptor.message_key = message
                .get("id")
                .and_then(Value::as_str)
                .filter(|id| !id.is_empty())
                .map(ToOwned::to_owned);
            descriptor.assistant_source = Some("claude.assistant".to_string());

            let mut text_parts = Vec::new();
            let mut has_reasoning = false;
            if let Some(items) = message.get("content").and_then(Value::as_array) {
                for item in items {
                    match item.get("type").and_then(Value::as_str) {
                        Some("text") => push_trimmed(&mut text_parts, item.get("text")),
                        Some("thinking") => has_reasoning = true,
                        Some("tool_use") => descriptor.process_count += 1,
                        _ => {}
                    }
                }
            }
            descriptor.process_count += u32::from(has_reasoning);
            if !text_parts.is_empty() {
                let text = text_parts.join("\n\n");
                descriptor.conversation_kind = Some(ConversationKind::Assistant);
                descriptor.message_hash = Some(hash_text(&text));
            }
        }
        "ai-title" => {
            descriptor.title =
                string_field(value, "title").or_else(|| string_field(value, "aiTitle"));
            descriptor.process_count = 1;
        }
        "" => {}
        _ => descriptor.process_count = 1,
    }

    descriptor
}

fn classify_codex(value: &Value) -> LineDescriptor {
    let top_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    let payload = value.get("payload");
    let payload_type = payload
        .and_then(|payload| payload.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let event_type = if payload_type.is_empty() {
        top_type.to_string()
    } else {
        format!("{top_type}.{payload_type}")
    };
    let mut descriptor = LineDescriptor {
        event_type,
        timestamp: timestamp(value),
        conversation_kind: None,
        process_count: 0,
        message_key: None,
        message_hash: None,
        assistant_source: None,
        session_id: None,
        title: None,
        working_dir: None,
        git_branch: None,
        model: None,
        parent_session_id: None,
        is_sidechain: false,
    };

    if top_type == "session_meta" {
        if let Some(payload) = payload {
            descriptor.session_id = string_field(payload, "id");
            descriptor.working_dir = string_field(payload, "cwd");
            descriptor.model = string_field(payload, "model");
            descriptor.parent_session_id = payload
                .pointer("/source/subagent/thread_spawn/parent_thread_id")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| string_field(payload, "forked_from_id"));
            descriptor.is_sidechain = payload
                .pointer("/source/subagent/thread_spawn/parent_thread_id")
                .is_some();
        }
    } else if top_type == "turn_context" {
        if let Some(payload) = payload {
            descriptor.working_dir = string_field(payload, "cwd");
            descriptor.model = string_field(payload, "model");
        }
    }

    match (top_type, payload_type) {
        ("session_meta", _) => {}
        ("event_msg", "user_message") => {
            if let Some(text) = payload
                .and_then(|payload| payload.get("message"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                descriptor.conversation_kind = Some(ConversationKind::User);
                descriptor.message_hash = Some(hash_text(text));
            }
        }
        ("event_msg", "agent_message") => {
            if let Some(text) = payload
                .and_then(|payload| payload.get("message"))
                .and_then(Value::as_str)
                .filter(|text| !text.is_empty())
            {
                descriptor.conversation_kind = Some(ConversationKind::Assistant);
                descriptor.message_hash = Some(hash_text(text));
                descriptor.assistant_source = Some("codex.agent_message".to_string());
            }
        }
        ("response_item", "message")
            if payload
                .and_then(|payload| payload.get("role"))
                .and_then(Value::as_str)
                == Some("assistant") =>
        {
            if let Some(text) = payload
                .and_then(|payload| payload.get("content"))
                .and_then(extract_codex_text)
            {
                descriptor.conversation_kind = Some(ConversationKind::Assistant);
                descriptor.message_hash = Some(hash_text(&text));
                descriptor.assistant_source = Some("codex.response_message".to_string());
                descriptor.message_key = payload
                    .and_then(|payload| payload.get("id"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned);
            }
        }
        ("event_msg", "agent_reasoning") => {
            descriptor.process_count = u32::from(
                payload
                    .and_then(|payload| payload.get("text").or_else(|| payload.get("message")))
                    .and_then(Value::as_str)
                    .is_some_and(|text| !text.is_empty()),
            );
        }
        ("event_msg", "patch_apply_end" | "exec_command_end") => {}
        ("response_item", "message") => {}
        ("response_item", "reasoning") => {
            descriptor.process_count = u32::from(payload.is_some_and(|payload| {
                payload
                    .get("summary")
                    .and_then(extract_codex_text)
                    .is_some_and(|text| !text.is_empty())
                    || payload
                        .get("content")
                        .and_then(extract_codex_text)
                        .is_some_and(|text| !text.is_empty())
                    || payload
                        .get("encrypted_content")
                        .and_then(Value::as_str)
                        .is_some_and(|text| !text.is_empty())
            }));
        }
        ("", "") => {}
        _ => descriptor.process_count = 1,
    }

    descriptor
}

fn extract_claude_conversation(
    value: &Value,
) -> Option<(ConversationKind, String, Option<String>, Option<String>)> {
    match value.get("type").and_then(Value::as_str)? {
        "user" if value.get("isMeta").and_then(Value::as_bool) != Some(true) => {
            let content = value.pointer("/message/content")?;
            let (text, _) = claude_user_content(content);
            Some((ConversationKind::User, text?, None, None))
        }
        "assistant" => {
            let message = value.get("message")?;
            let mut text_parts = Vec::new();
            for item in message.get("content")?.as_array()? {
                if item.get("type").and_then(Value::as_str) == Some("text") {
                    push_trimmed(&mut text_parts, item.get("text"));
                }
            }
            if text_parts.is_empty() {
                return None;
            }
            Some((
                ConversationKind::Assistant,
                text_parts.join("\n\n"),
                message
                    .get("model")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
                message
                    .get("stop_reason")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            ))
        }
        _ => None,
    }
}

fn extract_codex_conversation(
    value: &Value,
) -> Option<(ConversationKind, String, Option<String>, Option<String>)> {
    let top_type = value.get("type").and_then(Value::as_str)?;
    let payload = value.get("payload")?;
    let payload_type = payload.get("type").and_then(Value::as_str)?;

    match (top_type, payload_type) {
        ("event_msg", "user_message") => Some((
            ConversationKind::User,
            payload.get("message")?.as_str()?.trim().to_string(),
            None,
            None,
        )),
        ("event_msg", "agent_message") => Some((
            ConversationKind::Assistant,
            payload.get("message")?.as_str()?.to_string(),
            None,
            payload
                .get("phase")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
        )),
        ("response_item", "message")
            if payload.get("role").and_then(Value::as_str) == Some("assistant") =>
        {
            Some((
                ConversationKind::Assistant,
                extract_codex_text(payload.get("content")?)?,
                None,
                None,
            ))
        }
        _ => None,
    }
}

fn extract_claude_process(value: &Value, line_number: u64) -> Vec<SessionEvent> {
    let event_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let mut events = Vec::new();
    match event_type {
        "user" if value.get("isMeta").and_then(Value::as_bool) != Some(true) => {
            if let Some(items) = value.pointer("/message/content").and_then(Value::as_array) {
                for (index, item) in items.iter().enumerate() {
                    if item.get("type").and_then(Value::as_str) != Some("tool_result") {
                        continue;
                    }
                    events.push(SessionEvent::ToolResult {
                        meta: process_meta(
                            format!("claude-tool-result-{line_number}-{index}"),
                            "tool_result",
                            timestamp.clone(),
                        ),
                        call_id: string_field(item, "tool_use_id"),
                        content: process_content(item.get("content")),
                        is_error: item
                            .get("is_error")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    });
                }
            }
        }
        "assistant" => {
            let Some(message) = value.get("message") else {
                return events;
            };
            if claude_usage_has_tokens(message.get("usage")) {
                let usage = message.get("usage").unwrap_or(&Value::Null);
                events.push(SessionEvent::TokenUsage {
                    meta: process_meta(
                        format!("claude-token-{line_number}"),
                        "usage",
                        timestamp.clone(),
                    ),
                    input_tokens: usage.get("input_tokens").and_then(Value::as_u64),
                    output_tokens: usage.get("output_tokens").and_then(Value::as_u64),
                    cache_read_tokens: usage.get("cache_read_input_tokens").and_then(Value::as_u64),
                    cache_creation_tokens: usage
                        .get("cache_creation_input_tokens")
                        .and_then(Value::as_u64),
                    rate_limit: usage.get("rate_limits").cloned(),
                });
            }
            let mut reasoning = Vec::new();
            let mut encrypted = false;
            if let Some(items) = message.get("content").and_then(Value::as_array) {
                for (index, item) in items.iter().enumerate() {
                    match item.get("type").and_then(Value::as_str) {
                        Some("thinking") => {
                            if let Some(text) = item
                                .get("thinking")
                                .and_then(Value::as_str)
                                .map(str::trim)
                                .filter(|text| !text.is_empty())
                            {
                                reasoning.push(text.to_string());
                            } else {
                                encrypted |= item
                                    .get("signature")
                                    .and_then(Value::as_str)
                                    .is_some_and(|value| !value.is_empty());
                            }
                        }
                        Some("tool_use") => {
                            let input = item.get("input").cloned().unwrap_or(Value::Null);
                            let name = item
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or("tool")
                                .to_string();
                            events.push(SessionEvent::ToolCall {
                                meta: process_meta(
                                    format!("claude-tool-{line_number}-{index}"),
                                    "tool_use",
                                    timestamp.clone(),
                                ),
                                call_id: string_field(item, "id"),
                                name,
                                summary: None,
                                input,
                            });
                        }
                        _ => {}
                    }
                }
            }
            if !reasoning.is_empty() || encrypted {
                events.push(SessionEvent::Reasoning {
                    meta: process_meta(
                        format!("claude-reasoning-{line_number}"),
                        "thinking",
                        timestamp.clone(),
                    ),
                    text: if reasoning.is_empty() {
                        "(encrypted reasoning)".to_string()
                    } else {
                        reasoning.join("\n\n")
                    },
                    encrypted,
                });
            }
        }
        "system" => events.push(SessionEvent::SystemNotice {
            meta: process_meta(
                format!("claude-system-{line_number}"),
                "system",
                timestamp.clone(),
            ),
            subtype: string_field(value, "subtype"),
            level: string_field(value, "level"),
            content: value
                .get("hookInfos")
                .or_else(|| value.get("content"))
                .map(crate::util::pretty_json),
            hook_count: value
                .get("hookCount")
                .and_then(Value::as_u64)
                .and_then(|value| u32::try_from(value).ok()),
        }),
        "permission-mode" => events.push(SessionEvent::PermissionMode {
            meta: process_meta(
                format!("claude-permission-{line_number}"),
                "permission-mode",
                timestamp.clone(),
            ),
            mode: value
                .get("permissionMode")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
        }),
        "attachment" => events.push(SessionEvent::Attachment {
            meta: process_meta(
                format!("claude-attachment-{line_number}"),
                "attachment",
                timestamp.clone(),
            ),
            name: value
                .get("attachment")
                .and_then(|item| item.get("hookName").or_else(|| item.get("name")))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            mime: value
                .get("attachment")
                .and_then(|item| item.get("mime").or_else(|| item.get("type")))
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            preview: value.get("attachment").map(crate::util::pretty_json),
        }),
        "file-history-snapshot" => events.push(SessionEvent::FileSnapshot {
            meta: process_meta(
                format!("claude-files-{line_number}"),
                "file-history-snapshot",
                timestamp.clone(),
            ),
            file_count: value
                .pointer("/snapshot/trackedFileBackups")
                .and_then(Value::as_object)
                .and_then(|value| u32::try_from(value.len()).ok()),
        }),
        "last-prompt" | "ai-title" | "queue-operation" => {
            events.push(SessionEvent::Lifecycle {
                meta: process_meta(
                    format!("claude-life-{line_number}"),
                    event_type,
                    timestamp.clone(),
                ),
                event: event_type.to_string(),
                text: value
                    .get("lastPrompt")
                    .or_else(|| value.get("title"))
                    .or_else(|| value.get("content"))
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            });
        }
        "" => {}
        other => events.push(SessionEvent::Unknown {
            meta: process_meta(format!("claude-unknown-{line_number}"), other, timestamp),
            raw_type: other.to_string(),
        }),
    }
    events
}

fn extract_codex_process(value: &Value, line_number: u64) -> Vec<SessionEvent> {
    let top_type = value.get("type").and_then(Value::as_str).unwrap_or("");
    let payload = value.get("payload");
    let payload_type = payload
        .and_then(|payload| payload.get("type"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let timestamp = value
        .get("timestamp")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let source_type = if payload_type.is_empty() {
        top_type.to_string()
    } else {
        format!("{top_type}.{payload_type}")
    };
    let meta = || {
        process_meta(
            format!("codex-process-{line_number}"),
            source_type.clone(),
            timestamp.clone(),
        )
    };

    let event = match (top_type, payload_type) {
        ("turn_context", _) => payload.map(|payload| SessionEvent::TurnContext {
            meta: meta(),
            cwd: string_field(payload, "cwd"),
            model: string_field(payload, "model"),
            sandbox: payload
                .pointer("/sandbox_policy/type")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned),
            approval: string_field(payload, "approval_policy"),
            summary: None,
        }),
        ("compacted", _) | ("event_msg", "context_compacted") => Some(SessionEvent::Compaction {
            meta: meta(),
            summary: payload
                .and_then(|payload| payload.get("summary"))
                .or_else(|| value.get("summary"))
                .and_then(extract_codex_text),
        }),
        ("event_msg", "agent_reasoning") => Some(SessionEvent::Reasoning {
            meta: meta(),
            text: payload
                .and_then(|payload| payload.get("text").or_else(|| payload.get("message")))
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            encrypted: false,
        }),
        ("event_msg", "token_count") => {
            let info = payload.and_then(|payload| payload.get("info"));
            Some(SessionEvent::TokenUsage {
                meta: meta(),
                input_tokens: info
                    .and_then(|info| info.get("input_tokens"))
                    .and_then(Value::as_u64),
                output_tokens: info
                    .and_then(|info| info.get("output_tokens"))
                    .and_then(Value::as_u64),
                cache_read_tokens: info
                    .and_then(|info| info.get("cached_input_tokens"))
                    .and_then(Value::as_u64),
                cache_creation_tokens: info
                    .and_then(|info| info.get("cache_creation_input_tokens"))
                    .and_then(Value::as_u64),
                rate_limit: payload
                    .and_then(|payload| payload.get("rate_limits"))
                    .cloned(),
            })
        }
        ("event_msg", "task_started" | "task_complete" | "turn_aborted") => {
            Some(SessionEvent::Lifecycle {
                meta: meta(),
                event: payload_type.to_string(),
                text: payload
                    .and_then(|payload| {
                        payload
                            .get("text")
                            .or_else(|| payload.get("message"))
                            .or_else(|| payload.get("reason"))
                    })
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned),
            })
        }
        ("event_msg", "guardian_assessment") => Some(SessionEvent::SystemNotice {
            meta: meta(),
            subtype: Some("guardian_assessment".to_string()),
            level: payload.and_then(|payload| string_field(payload, "level")),
            content: payload.map(crate::util::pretty_json),
            hook_count: None,
        }),
        ("event_msg", "patch_apply_end" | "exec_command_end") => None,
        ("response_item", "reasoning") => Some(SessionEvent::Reasoning {
            meta: meta(),
            text: payload
                .and_then(|payload| payload.get("content").or_else(|| payload.get("summary")))
                .and_then(extract_codex_text)
                .unwrap_or_else(|| "(encrypted reasoning)".to_string()),
            encrypted: payload
                .and_then(|payload| payload.get("encrypted_content"))
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()),
        }),
        ("response_item", "function_call" | "custom_tool_call" | "web_search_call") => {
            let payload = payload.unwrap_or(&Value::Null);
            let name = if payload_type == "web_search_call" {
                "web_search".to_string()
            } else {
                payload
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("tool")
                    .to_string()
            };
            let input = payload
                .get("arguments")
                .or_else(|| payload.get("input"))
                .and_then(|value| {
                    value
                        .as_str()
                        .and_then(|value| serde_json::from_str(value).ok())
                        .or_else(|| Some(value.clone()))
                })
                .unwrap_or_else(|| payload.clone());
            Some(SessionEvent::ToolCall {
                meta: meta(),
                call_id: string_field(payload, "call_id").or_else(|| string_field(payload, "id")),
                name,
                input,
                summary: None,
            })
        }
        ("response_item", "function_call_output" | "custom_tool_call_output") => {
            let payload = payload.unwrap_or(&Value::Null);
            Some(SessionEvent::ToolResult {
                meta: meta(),
                call_id: string_field(payload, "call_id"),
                content: process_content(payload.get("output")),
                is_error: payload
                    .get("is_error")
                    .or_else(|| payload.get("error"))
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        }
        ("session_meta", _)
        | ("event_msg", "user_message" | "agent_message")
        | ("response_item", "message")
        | ("", "") => None,
        _ => Some(SessionEvent::Unknown {
            meta: meta(),
            raw_type: source_type,
        }),
    };
    event.into_iter().collect()
}

fn process_meta(
    id: impl Into<String>,
    source_type: impl Into<String>,
    timestamp: Option<String>,
) -> EventMeta {
    EventMeta::new(id, source_type).with_timestamp(timestamp)
}

fn process_content(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.to_string(),
        Some(value) => crate::util::pretty_json(value),
        None => String::new(),
    }
}

fn claude_user_content(content: &Value) -> (Option<String>, u32) {
    match content {
        Value::String(text) => {
            let text = text.trim();
            ((!text.is_empty()).then(|| text.to_string()), 0)
        }
        Value::Array(items) => {
            let mut parts = Vec::new();
            let mut tool_results = 0;
            for item in items {
                match item.get("type").and_then(Value::as_str) {
                    Some("text") => push_trimmed(&mut parts, item.get("text")),
                    Some("image") => parts.push("[image]".to_string()),
                    Some("tool_result") => tool_results += 1,
                    _ => push_trimmed(&mut parts, item.get("text")),
                }
            }
            (
                (!parts.is_empty()).then(|| parts.join("\n\n")),
                tool_results,
            )
        }
        _ => (None, 0),
    }
}

fn claude_usage_has_tokens(usage: Option<&Value>) -> bool {
    usage.is_some_and(|usage| {
        [
            "input_tokens",
            "output_tokens",
            "cache_read_input_tokens",
            "cache_creation_input_tokens",
        ]
        .into_iter()
        .any(|key| usage.get(key).and_then(Value::as_u64).is_some())
    })
}

fn extract_codex_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => (!text.is_empty()).then(|| text.to_string()),
        Value::Array(items) => {
            let parts = items
                .iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(Value::as_str)
                        .or_else(|| item.get("content").and_then(Value::as_str))
                })
                .filter(|text| !text.is_empty())
                .map(ToOwned::to_owned)
                .collect::<Vec<_>>();
            (!parts.is_empty()).then(|| parts.join("\n\n"))
        }
        _ => None,
    }
}

fn push_trimmed(parts: &mut Vec<String>, value: Option<&Value>) {
    if let Some(text) = value.and_then(Value::as_str).map(str::trim) {
        if !text.is_empty() {
            parts.push(text.to_string());
        }
    }
}

fn timestamp(value: &Value) -> Option<String> {
    value
        .get("timestamp")
        .and_then(Value::as_str)
        .and_then(crate::index::normalize_timestamp)
}

fn hash_text(text: &str) -> Vec<u8> {
    Sha256::digest(text.as_bytes()).to_vec()
}

fn string_field(value: &Value, key: &str) -> Option<String> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}
