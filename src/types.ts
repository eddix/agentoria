export type SessionSource = "codex" | "claude";

export interface SessionSummary {
  id: string;
  source: SessionSource;
  title: string;
  preview: string;
  isPartial: boolean;
  updatedAt: string;
  path: string;
  workingDir?: string | null;
  projectPath?: string | null;
  gitBranch?: string | null;
  model?: string | null;
  agentRole?: string | null;
  agentNickname?: string | null;
  parentSessionId?: string | null;
  isSidechain: boolean;
  subagentCount: number;
  messageCount: number;
  toolCallCount: number;
  userCount: number;
  assistantCount: number;
}

interface EventBase {
  id: string;
  timestamp?: string | null;
  sourceType: string;
}

export type SessionEvent =
  | (EventBase & { kind: "user"; text: string })
  | (EventBase & {
      kind: "assistant_message";
      text: string;
      model?: string | null;
      stopReason?: string | null;
    })
  | (EventBase & { kind: "reasoning"; text: string; encrypted: boolean })
  | (EventBase & {
      kind: "tool_call";
      callId?: string | null;
      name: string;
      input: unknown;
      summary?: string | null;
    })
  | (EventBase & {
      kind: "tool_result";
      callId?: string | null;
      content: string;
      isError: boolean;
    })
  | (EventBase & {
      kind: "turn_context";
      cwd?: string | null;
      model?: string | null;
      sandbox?: string | null;
      approval?: string | null;
      summary?: string | null;
    })
  | (EventBase & {
      kind: "token_usage";
      inputTokens?: number | null;
      outputTokens?: number | null;
      cacheReadTokens?: number | null;
      cacheCreationTokens?: number | null;
      rateLimit?: unknown;
    })
  | (EventBase & { kind: "compaction"; summary?: string | null })
  | (EventBase & { kind: "lifecycle"; event: string; text?: string | null })
  | (EventBase & {
      kind: "system_notice";
      subtype?: string | null;
      level?: string | null;
      content?: string | null;
      hookCount?: number | null;
    })
  | (EventBase & { kind: "permission_mode"; mode: string })
  | (EventBase & {
      kind: "attachment";
      name?: string | null;
      mime?: string | null;
      preview?: string | null;
    })
  | (EventBase & { kind: "file_snapshot"; fileCount?: number | null })
  | (EventBase & { kind: "unknown"; rawType: string });

export type EventKind = SessionEvent["kind"];

export interface SessionStats {
  filePath: string;
  updatedAt: string;
  startedAt?: string | null;
  eventCount: number;
  userCount: number;
  assistantCount: number;
  toolCallCount: number;
  reasoningCount: number;
  contentChars: number;
  totalInputTokens: number;
  totalOutputTokens: number;
  cacheReadTokens: number;
  cacheCreationTokens: number;
}

export interface SessionDetail {
  summary: SessionSummary;
  events: SessionEvent[];
  stats: SessionStats;
}
