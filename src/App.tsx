import { startTransition, useDeferredValue, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  Bot,
  ChevronDown,
  Clock3,
  FileText,
  FolderRoot,
  Hammer,
  MessageSquareQuote,
  MoonStar,
  RefreshCcw,
  SunMedium,
  UserRound,
  type LucideIcon,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from "@/components/ui/dropdown-menu";
import { ScrollArea } from "@/components/ui/scroll-area";
import { Skeleton } from "@/components/ui/skeleton";
import { applyTheme, persistTheme, resolveInitialTheme, type ThemeMode } from "@/lib/theme";
import { cn } from "@/lib/utils";
import type {
  SessionDetail,
  SessionMessage,
  SessionSource,
  SessionStats,
  SessionSummary,
} from "@/types";

const sourceMeta: Record<
  SessionSource,
  { label: string; strap: string; hint: string }
> = {
  codex: {
    label: "Codex",
    strap: "OpenAI coding session logs",
    hint: "~/.codex/sessions",
  },
  claude: {
    label: "Claude",
    strap: "Anthropic CLI conversation files",
    hint: "~/.claude/projects",
  },
};

export default function App() {
  const [theme, setTheme] = useState<ThemeMode>(() => resolveInitialTheme());
  const [source, setSource] = useState<SessionSource>("codex");
  const [sessions, setSessions] = useState<SessionSummary[]>([]);
  const [listLoading, setListLoading] = useState(true);
  const [detailLoading, setDetailLoading] = useState(false);
  const [listError, setListError] = useState<string | null>(null);
  const [detailError, setDetailError] = useState<string | null>(null);
  const [selectedPath, setSelectedPath] = useState<string | null>(null);
  const [detail, setDetail] = useState<SessionDetail | null>(null);
  const [reloadTick, setReloadTick] = useState(0);
  const deferredSelection = useDeferredValue(selectedPath);

  useEffect(() => {
    applyTheme(theme);
    persistTheme(theme);
  }, [theme]);

  useEffect(() => {
    let cancelled = false;

    async function loadSessions() {
      setListLoading(true);
      setListError(null);
      setDetail(null);
      setDetailError(null);

      try {
        const nextSessions = await invoke<SessionSummary[]>("list_sessions", {
          source,
        });

        if (cancelled) {
          return;
        }

        setSessions(nextSessions);
        setSelectedPath((current) =>
          current && nextSessions.some((session) => session.path === current)
            ? current
            : null,
        );
      } catch (error) {
        if (!cancelled) {
          setSessions([]);
          setSelectedPath(null);
          setListError(normalizeError(error));
        }
      } finally {
        if (!cancelled) {
          setListLoading(false);
        }
      }
    }

    void loadSessions();

    return () => {
      cancelled = true;
    };
  }, [source, reloadTick]);

  useEffect(() => {
    if (!deferredSelection || !isPathCompatibleWithSource(source, deferredSelection)) {
      setDetail(null);
      setDetailLoading(false);
      setDetailError(null);
      return;
    }

    let cancelled = false;

    async function loadDetail() {
      setDetailLoading(true);
      setDetailError(null);

      try {
        const nextDetail = await invoke<SessionDetail>("get_session_detail", {
          source,
          path: deferredSelection,
        });

        if (!cancelled) {
          setDetail(nextDetail);
        }
      } catch (error) {
        if (!cancelled) {
          setDetail(null);
          setDetailError(normalizeError(error));
        }
      } finally {
        if (!cancelled) {
          setDetailLoading(false);
        }
      }
    }

    void loadDetail();

    return () => {
      cancelled = true;
    };
  }, [source, deferredSelection, reloadTick]);

  const currentSummary =
    detail?.summary ?? sessions.find((session) => session.path === selectedPath) ?? null;
  const stats = detail?.stats ?? summarizeFallback(currentSummary);
  const statusLine = buildStatusLine(source, currentSummary, stats);
  const loadingStatus = buildLoadingStatus({
    source,
    listLoading,
    detailLoading,
    listError,
    detailError,
    currentSummary,
  });

  return (
    <div className="h-screen bg-background text-foreground">
      <div className="pointer-events-none fixed inset-0 bg-[radial-gradient(circle_at_top,rgba(37,99,235,0.05),transparent_30%)] dark:bg-[radial-gradient(circle_at_top,rgba(96,165,250,0.12),transparent_35%)]" />

      <main className="relative flex h-screen min-w-0 flex-col">
        <header className="flex h-14 shrink-0 items-center justify-between gap-4 border-b border-border bg-background/90 px-5 backdrop-blur">
          <div className="flex min-w-0 items-center gap-3">
            <LogoMark />
            <div className="min-w-0">
              <div className="font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground">
                Sessionary
              </div>
              <div className="flex min-w-0 items-center gap-3">
                <h1 className="truncate font-display text-xl font-semibold tracking-tight">
                  Local Session Console
                </h1>
                <span className="hidden rounded-md border border-border bg-muted/40 px-2.5 py-1 text-xs text-muted-foreground md:inline-flex">
                  {sourceMeta[source].strap}
                </span>
              </div>
            </div>
          </div>

          <div className="flex items-center gap-2">
            <Button
              size="icon"
              variant="outline"
              onClick={() =>
                setTheme((current) => (current === "dark" ? "light" : "dark"))
              }
              title={theme === "dark" ? "切换到浅色模式" : "切换到暗色模式"}
              aria-label={theme === "dark" ? "切换到浅色模式" : "切换到暗色模式"}
            >
              {theme === "dark" ? (
                <SunMedium className="h-4 w-4" />
              ) : (
                <MoonStar className="h-4 w-4" />
              )}
            </Button>

            <Button
              size="icon"
              variant="outline"
              onClick={() => setReloadTick((current) => current + 1)}
              disabled={listLoading}
              title="刷新当前来源"
            >
              <RefreshCcw className={cn("h-4 w-4", listLoading && "animate-spin")} />
            </Button>

            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="outline"
                  className="h-10 min-w-[220px] justify-between px-3"
                >
                  <div className="text-left">
                    <div className="font-mono text-[10px] uppercase tracking-[0.18em] text-muted-foreground">
                      Source
                    </div>
                    <div className="mt-0.5 text-sm font-medium">
                      {sourceMeta[source].label}
                    </div>
                  </div>
                  <ChevronDown className="h-4 w-4 text-muted-foreground" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuLabel>Session Source</DropdownMenuLabel>
                <DropdownMenuSeparator />
                <DropdownMenuRadioGroup
                  value={source}
                  onValueChange={(value) => setSource(value as SessionSource)}
                >
                  {(Object.keys(sourceMeta) as SessionSource[]).map((item) => (
                    <DropdownMenuRadioItem key={item} value={item}>
                      <div className="space-y-0.5">
                        <div className="font-medium">{sourceMeta[item].label}</div>
                        <div className="text-xs text-muted-foreground">
                          {sourceMeta[item].hint}
                        </div>
                      </div>
                    </DropdownMenuRadioItem>
                  ))}
                </DropdownMenuRadioGroup>
              </DropdownMenuContent>
            </DropdownMenu>
          </div>
        </header>

        <section className="grid min-h-0 flex-1 grid-cols-[360px_minmax(0,1fr)]">
          <aside className="flex min-h-0 flex-col border-r border-border bg-background/55">
            <div className="border-b border-border px-5 py-4">
              <div className="flex items-start justify-between gap-3">
                <div>
                  <div className="font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground">
                    Sessions
                  </div>
                  <h2 className="mt-1 font-display text-lg font-semibold tracking-tight">
                    {sourceMeta[source].label}
                  </h2>
                </div>
                <Badge variant="outline">{sessions.length} total</Badge>
              </div>
              <p className="mt-3 text-sm text-muted-foreground">
                {sourceMeta[source].hint}
              </p>
            </div>

            <ScrollArea className="min-h-0 flex-1">
              <div className="divide-y divide-border">
                {listLoading && (
                  <>
                    {Array.from({ length: 6 }).map((_, index) => (
                      <SessionListSkeleton key={index} />
                    ))}
                  </>
                )}

                {!listLoading && listError && (
                  <EmptyState title="列表没拉下来" description={listError} />
                )}

                {!listLoading && !listError && sessions.length === 0 && (
                  <EmptyState
                    title="没找到 session"
                    description={`当前来源会去扫 ${sourceMeta[source].hint}`}
                  />
                )}

                {!listLoading &&
                  !listError &&
                  sessions.map((session, index) => {
                    const active = selectedPath === session.path;
                    const workspacePath = getWorkspacePath(session) ?? session.path;

                    return (
                      <button
                        key={session.path}
                        type="button"
                        className={cn(
                          "relative w-full px-5 py-4 text-left transition-colors duration-200 animate-floatIn",
                          active
                            ? "bg-primary/[0.05]"
                            : "bg-transparent hover:bg-muted/40",
                        )}
                        style={{ animationDelay: `${index * 28}ms` }}
                        onClick={() =>
                          startTransition(() => {
                            setSelectedPath(session.path);
                          })
                        }
                      >
                        {active && (
                          <span className="absolute inset-y-0 left-0 w-0.5 bg-primary" />
                        )}

                        <div className="flex items-start justify-between gap-3">
                          <div className="min-w-0">
                            <div className="block max-w-full truncate text-sm font-semibold text-foreground">
                              {session.title}
                            </div>
                            <div className="mt-1 font-mono text-[11px] uppercase tracking-[0.16em] text-muted-foreground">
                              {formatDateShort(session.updatedAt)}
                            </div>
                          </div>
                          <Badge
                            variant={active ? "default" : "outline"}
                            className="shrink-0"
                          >
                            {session.source}
                          </Badge>
                        </div>

                        {!!session.preview && (
                          <div className="mt-2 overflow-hidden break-words text-sm leading-5 text-muted-foreground [display:-webkit-box] [-webkit-box-orient:vertical] [-webkit-line-clamp:2]">
                            {session.preview}
                          </div>
                        )}

                        {!session.isPartial && (
                          <div className="mt-3 flex flex-wrap gap-4 text-xs text-muted-foreground">
                            <MetaInline
                              icon={MessageSquareQuote}
                              text={`${session.messageCount} msgs`}
                            />
                            <MetaInline icon={Hammer} text={`${session.toolCallCount} tools`} />
                          </div>
                        )}

                        <div className="mt-3 truncate font-mono text-[11px] text-muted-foreground">
                          {formatDisplayPath(workspacePath)}
                        </div>
                      </button>
                    );
                  })}
              </div>
            </ScrollArea>
          </aside>

          <section className="flex min-h-0 flex-col">
            <div className="border-b border-border px-6 py-5">
              <div className="flex flex-wrap items-start justify-between gap-4">
                <div className="min-w-0">
                  <div className="font-mono text-[11px] uppercase tracking-[0.18em] text-muted-foreground">
                    Transcript
                  </div>
                  <h2 className="mt-1 truncate font-display text-[1.65rem] font-semibold tracking-tight">
                    {currentSummary?.title ?? "Select a session"}
                  </h2>
                  <p className="mt-2 text-sm leading-6 text-muted-foreground">
                    {currentSummary?.preview ||
                      "右侧会显示当前 session 的主要对话和工具轨迹。"}
                  </p>
                </div>

                {currentSummary && (
                  <div className="flex flex-wrap gap-2">
                    <Badge variant="default">{sourceMeta[source].label}</Badge>
                    {stats ? (
                      <>
                        <Badge variant="outline">{stats.messageCount} items</Badge>
                        <Badge variant="outline">{stats.toolCallCount} tools</Badge>
                      </>
                    ) : (
                      <Badge variant="outline">detail on demand</Badge>
                    )}
                  </div>
                )}
              </div>

              {currentSummary && (
                <div className="mt-4 flex flex-wrap gap-x-5 gap-y-2 border-t border-border pt-3">
                  <DetailInline
                    icon={Clock3}
                    label="Updated"
                    value={formatDateLong(currentSummary.updatedAt)}
                  />
                  {stats?.startedAt && (
                    <DetailInline
                      icon={Clock3}
                      label="Started"
                      value={formatDateLong(stats.startedAt)}
                    />
                  )}
                  <DetailInline
                    icon={FolderRoot}
                    label="Workspace"
                    value={formatDisplayPath(getWorkspacePath(currentSummary))}
                  />
                  <DetailInline
                    icon={FileText}
                    label="File"
                    value={formatDisplayPath(stats?.filePath ?? currentSummary.path)}
                  />
                  <DetailInline
                    icon={MessageSquareQuote}
                    label="Messages"
                    value={stats ? String(stats.messageCount) : "On demand"}
                  />
                  <DetailInline
                    icon={Hammer}
                    label="Tools"
                    value={stats ? String(stats.toolCallCount) : "On demand"}
                  />
                  <DetailInline
                    icon={UserRound}
                    label="User"
                    value={stats ? String(stats.userCount) : "N/A"}
                  />
                  <DetailInline
                    icon={Bot}
                    label="Assistant"
                    value={stats ? String(stats.assistantCount) : "N/A"}
                  />
                </div>
              )}
            </div>

            <ScrollArea className="min-h-0 flex-1">
              <div className="divide-y divide-border">
                {detailLoading && (
                  <>
                    {Array.from({ length: 5 }).map((_, index) => (
                      <MessageSkeleton key={index} />
                    ))}
                  </>
                )}

                {!detailLoading && detailError && (
                  <EmptyState title="详情没拉下来" description={detailError} />
                )}

                {!detailLoading && !detailError && detail && detail.messages.length === 0 && (
                  <EmptyState
                    title="这个 session 还没有可展示内容"
                    description="当前解析器只会展示主要的用户、助手和工具轨迹。"
                  />
                )}

                {!detailLoading &&
                  !detailError &&
                  detail?.messages.map((message, index) => (
                    <MessageBubble key={`${message.id}-${index}`} message={message} />
                  ))}

                {!detailLoading && !detailError && !detail && (
                  <EmptyState
                    title="先选一个 session"
                    description="左边点一个条目，右边就会按需加载完整内容。"
                  />
                )}
              </div>
            </ScrollArea>
          </section>
        </section>

        <footer className="flex h-9 shrink-0 items-center justify-between gap-4 border-t border-border bg-background/90 px-4 backdrop-blur">
          <div className="flex min-w-0 items-center gap-3">
            <span
              className={cn(
                "font-mono text-[11px]",
                loadingStatus.tone === "error"
                  ? "text-destructive"
                  : loadingStatus.active
                    ? "text-foreground"
                    : "text-muted-foreground",
              )}
            >
              {loadingStatus.label}
            </span>
            <div className="hidden h-1.5 w-28 overflow-hidden rounded-full bg-muted md:block">
              <div
                className={cn(
                  "h-full rounded-full transition-all duration-300",
                  loadingStatus.active ? "animate-pulse bg-primary" : "bg-primary/35",
                  loadingStatus.tone === "error" && "bg-destructive",
                )}
                style={{ width: `${loadingStatus.progress}%` }}
              />
            </div>
          </div>

          <span className="min-w-0 truncate font-mono text-[11px] text-muted-foreground">
            {statusLine}
          </span>
        </footer>
      </main>
    </div>
  );
}

function LogoMark() {
  return (
    <div className="flex h-10 w-10 items-center justify-center rounded-xl border border-primary/15 bg-primary/5">
      <div className="flex items-center gap-1.5">
        <span className="h-5 w-1 rounded-full bg-primary" />
        <div className="space-y-1">
          <div className="h-1.5 w-4 rounded-full bg-primary" />
          <div className="h-1.5 w-3 rounded-full bg-foreground/80" />
          <div className="h-1.5 w-2 rounded-full bg-foreground/35" />
        </div>
      </div>
    </div>
  );
}

function MetaInline({
  icon: Icon,
  text,
}: {
  icon: LucideIcon;
  text: string;
}) {
  return (
    <span className="inline-flex items-center gap-1.5">
      <Icon className="h-3.5 w-3.5" />
      {text}
    </span>
  );
}

function DetailInline({
  icon: Icon,
  label,
  value,
}: {
  icon: LucideIcon;
  label: string;
  value: string;
}) {
  return (
    <div className="flex min-w-0 items-center gap-2 text-sm text-muted-foreground">
      <div className="flex shrink-0 items-center gap-2 font-mono text-[10px] uppercase tracking-[0.16em] text-muted-foreground">
        <Icon className="h-3.5 w-3.5" />
        {label}
      </div>
      <div className="min-w-0 truncate font-medium text-foreground">{value}</div>
    </div>
  );
}

function EmptyState({
  title,
  description,
}: {
  title: string;
  description: string;
}) {
  return (
    <div className="rounded-xl border border-dashed border-border bg-background px-6 py-8 text-center">
      <div className="font-display text-xl font-semibold tracking-tight">{title}</div>
      <div className="mt-2 text-sm leading-6 text-muted-foreground">
        {description}
      </div>
    </div>
  );
}

function SessionListSkeleton() {
  return (
    <div className="px-5 py-4">
      <div className="flex items-center justify-between gap-3">
        <Skeleton className="h-5 w-36 rounded-md" />
        <Skeleton className="h-5 w-14 rounded-md" />
      </div>
      <Skeleton className="mt-3 h-4 w-24" />
      <Skeleton className="mt-3 h-4 w-full" />
      <Skeleton className="mt-2 h-4 w-5/6" />
      <Skeleton className="mt-4 h-4 w-1/2" />
    </div>
  );
}

function MessageSkeleton() {
  return (
    <div className="px-5 py-4">
      <div className="flex items-center justify-between">
        <Skeleton className="h-5 w-24 rounded-md" />
        <Skeleton className="h-4 w-28" />
      </div>
      <div className="mt-4">
        <Skeleton className="h-4 w-full" />
        <Skeleton className="mt-2 h-4 w-11/12" />
        <Skeleton className="mt-2 h-4 w-3/4" />
      </div>
    </div>
  );
}

function MessageBubble({ message }: { message: SessionMessage }) {
  const meta = messageVisuals[message.role];

  return (
    <article className={cn("animate-floatIn border-l-2 px-5 py-4", meta.shellClass)}>
      <div className="flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <span className={cn("h-2 w-2 rounded-full", meta.dotClass)} />
          <span className="text-sm font-medium text-foreground">{meta.label}</span>
          {message.phase && <Badge variant="outline">{phaseLabel(message.phase)}</Badge>}
          {message.label && message.role === "tool" && (
            <Badge variant="outline">{message.label}</Badge>
          )}
        </div>
        <span className="font-mono text-[11px] text-muted-foreground">
          {message.timestamp ? formatDateLong(message.timestamp) : "timestamp n/a"}
        </span>
      </div>
      <div
        className={cn(
          "whitespace-pre-wrap break-words pt-3 text-sm leading-6 text-foreground",
          message.role === "tool" && "font-mono text-[12px] leading-5 text-foreground/90",
        )}
      >
        {message.text}
      </div>
    </article>
  );
}

const messageVisuals = {
  user: {
    label: "User",
    shellClass: "border-l-primary bg-primary/[0.02]",
    dotClass: "bg-primary",
  },
  assistant: {
    label: "Assistant",
    shellClass: "border-l-border bg-background",
    dotClass: "bg-foreground/70",
  },
  tool: {
    label: "Tool",
    shellClass: "border-l-border bg-muted/35",
    dotClass: "bg-muted-foreground/70",
  },
};

function buildStatusLine(
  source: SessionSource,
  summary: SessionSummary | null,
  stats: SessionStats | null,
) {
  const pieces = [
    sourceMeta[source].label,
    summary?.title ?? "No session selected",
    stats ? `${stats.userCount} user` : null,
    stats ? `${stats.assistantCount} assistant` : null,
    stats ? `${stats.toolCallCount} tools` : null,
    formatDisplayPath(getWorkspacePath(summary) ?? summary?.path ?? null),
  ];

  return pieces.filter(Boolean).join(" / ");
}

function buildLoadingStatus({
  source,
  listLoading,
  detailLoading,
  listError,
  detailError,
  currentSummary,
}: {
  source: SessionSource;
  listLoading: boolean;
  detailLoading: boolean;
  listError: string | null;
  detailError: string | null;
  currentSummary: SessionSummary | null;
}) {
  if (listLoading) {
    return {
      active: true,
      tone: "loading" as const,
      progress: 50,
      label: `Loading ${sourceMeta[source].label} sessions (1/2)`,
    };
  }

  if (detailLoading) {
    return {
      active: true,
      tone: "loading" as const,
      progress: 100,
      label: `Loading transcript${currentSummary ? `: ${currentSummary.title}` : ""} (2/2)`,
    };
  }

  if (listError) {
    return {
      active: false,
      tone: "error" as const,
      progress: 100,
      label: `List load failed`,
    };
  }

  if (detailError) {
    return {
      active: false,
      tone: "error" as const,
      progress: 100,
      label: `Transcript load failed`,
    };
  }

  return {
    active: false,
    tone: "idle" as const,
    progress: 100,
    label: "Ready",
  };
}

function summarizeFallback(summary: SessionSummary | null): SessionStats | null {
  if (!summary || summary.isPartial) {
    return null;
  }

  return {
    filePath: summary.path,
    updatedAt: summary.updatedAt,
    startedAt: summary.updatedAt,
    messageCount: summary.messageCount,
    userCount: summary.userCount,
    assistantCount: summary.assistantCount,
    toolCallCount: summary.toolCallCount,
    contentChars: 0,
  };
}

function getWorkspacePath(summary?: SessionSummary | null) {
  return summary?.workingDir || summary?.projectPath || null;
}

function formatDisplayPath(value?: string | null) {
  if (!value) {
    return "N/A";
  }

  return value
    .replace(/^\/Users\/[^/]+/, "~")
    .replace(/^\/home\/[^/]+/, "~");
}

function formatDateShort(value: string) {
  return new Intl.DateTimeFormat("zh-CN", {
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

function formatDateLong(value: string) {
  return new Intl.DateTimeFormat("zh-CN", {
    year: "numeric",
    month: "2-digit",
    day: "2-digit",
    hour: "2-digit",
    minute: "2-digit",
  }).format(new Date(value));
}

function phaseLabel(phase: string) {
  switch (phase) {
    case "commentary":
      return "Commentary";
    case "final":
      return "Final";
    default:
      return phase;
  }
}

function normalizeError(error: unknown) {
  if (typeof error === "string") {
    return error;
  }

  if (error instanceof Error) {
    return error.message;
  }

  return "未知错误";
}

function isPathCompatibleWithSource(source: SessionSource, path: string) {
  if (source === "codex") {
    return path.includes("/.codex/sessions/");
  }

  return path.includes("/.claude/projects/");
}
