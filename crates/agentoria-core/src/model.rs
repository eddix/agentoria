use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum SessionSource {
    Claude,
    Codex,
    Grok,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionSummary {
    pub id: String,
    pub source: SessionSource,
    pub title: String,
    pub preview: String,
    pub is_partial: bool,
    pub updated_at: String,
    pub path: String,
    pub working_dir: Option<String>,
    pub project_path: Option<String>,
    pub git_branch: Option<String>,
    pub model: Option<String>,
    pub agent_role: Option<String>,
    pub agent_nickname: Option<String>,
    pub parent_session_id: Option<String>,
    pub is_sidechain: bool,
    pub subagent_count: u32,
    pub message_count: usize,
    pub tool_call_count: usize,
    pub user_count: usize,
    pub assistant_count: usize,
}

impl SessionSummary {
    pub fn empty(id: String, source: SessionSource, path: String) -> Self {
        Self {
            id: id.clone(),
            source,
            title: id,
            preview: String::new(),
            is_partial: true,
            updated_at: String::new(),
            path,
            working_dir: None,
            project_path: None,
            git_branch: None,
            model: None,
            agent_role: None,
            agent_nickname: None,
            parent_session_id: None,
            is_sidechain: false,
            subagent_count: 0,
            message_count: 0,
            tool_call_count: 0,
            user_count: 0,
            assistant_count: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionDetail {
    pub summary: SessionSummary,
    pub events: Vec<SessionEvent>,
    pub stats: SessionStats,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionStats {
    pub file_path: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub event_count: usize,
    pub user_count: usize,
    pub assistant_count: usize,
    pub tool_call_count: usize,
    pub reasoning_count: usize,
    pub content_chars: usize,
    pub total_input_tokens: u64,
    pub total_output_tokens: u64,
    pub cache_read_tokens: u64,
    pub cache_creation_tokens: u64,
}

impl SessionStats {
    pub fn new(file_path: String, updated_at: String) -> Self {
        Self {
            file_path,
            updated_at,
            started_at: None,
            event_count: 0,
            user_count: 0,
            assistant_count: 0,
            tool_call_count: 0,
            reasoning_count: 0,
            content_chars: 0,
            total_input_tokens: 0,
            total_output_tokens: 0,
            cache_read_tokens: 0,
            cache_creation_tokens: 0,
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EventMeta {
    pub id: String,
    pub timestamp: Option<String>,
    pub source_type: String,
}

impl EventMeta {
    pub fn new(id: impl Into<String>, source_type: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            timestamp: None,
            source_type: source_type.into(),
        }
    }

    pub fn with_timestamp(mut self, timestamp: Option<String>) -> Self {
        self.timestamp = timestamp;
        self
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionEvent {
    User {
        #[serde(flatten)]
        meta: EventMeta,
        text: String,
    },
    AssistantMessage {
        #[serde(flatten)]
        meta: EventMeta,
        text: String,
        model: Option<String>,
        stop_reason: Option<String>,
    },
    Reasoning {
        #[serde(flatten)]
        meta: EventMeta,
        text: String,
        encrypted: bool,
    },
    ToolCall {
        #[serde(flatten)]
        meta: EventMeta,
        call_id: Option<String>,
        name: String,
        input: Value,
        summary: Option<String>,
    },
    ToolResult {
        #[serde(flatten)]
        meta: EventMeta,
        call_id: Option<String>,
        content: String,
        is_error: bool,
    },
    TurnContext {
        #[serde(flatten)]
        meta: EventMeta,
        cwd: Option<String>,
        model: Option<String>,
        sandbox: Option<String>,
        approval: Option<String>,
        summary: Option<String>,
    },
    TokenUsage {
        #[serde(flatten)]
        meta: EventMeta,
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cache_read_tokens: Option<u64>,
        cache_creation_tokens: Option<u64>,
        rate_limit: Option<Value>,
    },
    Compaction {
        #[serde(flatten)]
        meta: EventMeta,
        summary: Option<String>,
    },
    Lifecycle {
        #[serde(flatten)]
        meta: EventMeta,
        event: String,
        text: Option<String>,
    },
    SystemNotice {
        #[serde(flatten)]
        meta: EventMeta,
        subtype: Option<String>,
        level: Option<String>,
        content: Option<String>,
        hook_count: Option<u32>,
    },
    PermissionMode {
        #[serde(flatten)]
        meta: EventMeta,
        mode: String,
    },
    Attachment {
        #[serde(flatten)]
        meta: EventMeta,
        name: Option<String>,
        mime: Option<String>,
        preview: Option<String>,
    },
    FileSnapshot {
        #[serde(flatten)]
        meta: EventMeta,
        file_count: Option<u32>,
    },
    Unknown {
        #[serde(flatten)]
        meta: EventMeta,
        raw_type: String,
    },
}
