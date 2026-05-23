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
  messageCount: number;
  toolCallCount: number;
  userCount: number;
  assistantCount: number;
}

export interface SessionMessage {
  id: string;
  role: "user" | "assistant" | "tool";
  kind: string;
  timestamp?: string | null;
  phase?: string | null;
  label?: string | null;
  text: string;
}

export interface SessionStats {
  filePath: string;
  updatedAt: string;
  startedAt?: string | null;
  messageCount: number;
  userCount: number;
  assistantCount: number;
  toolCallCount: number;
  contentChars: number;
}

export interface SessionDetail {
  summary: SessionSummary;
  messages: SessionMessage[];
  stats: SessionStats;
}
