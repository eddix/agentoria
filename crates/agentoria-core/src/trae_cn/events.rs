use crate::{index::IndexedLine, EventMeta, SessionEvent};
use serde_json::Value;

pub(super) fn text(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter(|part| part.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

pub(super) fn process_events(value: &Value, line: &IndexedLine) -> Vec<SessionEvent> {
    let mut events = Vec::new();
    let messages = value.get("raw_messages").and_then(Value::as_array);
    for (index, message) in messages.into_iter().flatten().enumerate() {
        let meta = || {
            EventMeta::new(
                format!("trae-{}-{index}", line.byte_offset),
                line.event_type.clone(),
            )
            .with_timestamp(line.timestamp.clone())
        };
        let body = text(message.get("content"));
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or("unknown");
        if role == "tool" {
            events.push(SessionEvent::ToolResult {
                meta: meta(),
                call_id: message
                    .get("tool_call_id")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                content: body,
                is_error: message.get("is_error").and_then(Value::as_bool) == Some(true),
            });
            continue;
        }
        if !body.is_empty() {
            events.push(SessionEvent::SystemNotice {
                meta: meta(),
                subtype: Some(
                    if role == "user" {
                        "model_context"
                    } else {
                        "assistant_trace"
                    }
                    .into(),
                ),
                level: None,
                content: Some(body),
                hook_count: None,
            });
        }
        if let Some(parts) = message.get("content").and_then(Value::as_array) {
            for part in parts {
                if matches!(
                    part.get("type").and_then(Value::as_str),
                    Some("thinking" | "reasoning" | "reasoning_content")
                ) {
                    if let Some(text) = part
                        .get("text")
                        .or_else(|| part.get("thinking"))
                        .and_then(Value::as_str)
                    {
                        events.push(SessionEvent::Reasoning {
                            meta: meta(),
                            text: text.into(),
                            encrypted: false,
                        });
                    }
                }
            }
        }
        if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
            for call in calls {
                let function = call
                    .get("function_call")
                    .or_else(|| call.get("function"))
                    .unwrap_or(call);
                let input = match function.get("arguments") {
                    Some(Value::String(text)) => {
                        serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone()))
                    }
                    Some(value) => value.clone(),
                    None => Value::Null,
                };
                events.push(SessionEvent::ToolCall {
                    meta: meta(),
                    call_id: call.get("id").and_then(Value::as_str).map(str::to_string),
                    name: function
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("Tool")
                        .into(),
                    input,
                    summary: None,
                });
            }
        }
    }
    if events.is_empty() {
        events.push(SessionEvent::SystemNotice {
            meta: EventMeta::new(
                format!("trae-{}", line.byte_offset),
                line.event_type.clone(),
            )
            .with_timestamp(line.timestamp.clone()),
            subtype: Some("history".into()),
            level: None,
            content: Some(serde_json::to_string_pretty(value).unwrap_or_default()),
            hook_count: None,
        });
    }
    events
}
