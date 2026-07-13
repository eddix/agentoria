import {
  startTransition,
  useDeferredValue,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import { invoke } from "@tauri-apps/api/core";
import { useVirtualizer } from "@tanstack/react-virtual";
import {
  Bot,
  Brain,
  ChevronDown,
  Clock3,
  Compass,
  FileCog,
  FileText,
  Folder,
  FolderRoot,
  GitBranch,
  Hammer,
  History,
  Info,
  KeyRound,
  ListTree,
  MessageSquareQuote,
  MoonStar,
  Network,
  Paperclip,
  RefreshCcw,
  Scissors,
  ShieldAlert,
  Sparkles,
  SunMedium,
  UserRound,
  Wand2,
  type LucideIcon,
} from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { MarkdownBody } from "@/components/markdown-body";
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
  SessionEvent,
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
  const [collapsedGroups, setCollapsedGroups] = useState<Set<string>>(
    () => new Set(),
  );
  const [renderMarkdown, setRenderMarkdown] = useState(true);
  const deferredSelection = useDeferredValue(selectedPath);

  const groupedSessions = useMemo(
    () => groupSessionsByWorkspace(sessions),
    [sessions],
  );

  function toggleGroup(key: string) {
    setCollapsedGroups((current) => {
      const next = new Set(current);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  }

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
              <div>
                {listLoading && (
                  <div className="divide-y divide-border">
                    {Array.from({ length: 6 }).map((_, index) => (
                      <SessionListSkeleton key={index} />
                    ))}
                  </div>
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
                  (() => {
                    let runningIndex = 0;
                    return groupedSessions.map((group) => {
                      const collapsed = collapsedGroups.has(group.key);
                      return (
                        <div
                          key={group.key}
                          className="border-b border-border last:border-b-0"
                        >
                          <button
                            type="button"
                            onClick={() => toggleGroup(group.key)}
                            className="flex w-full items-center justify-between gap-2 bg-muted/30 px-5 py-2 text-left transition-colors hover:bg-muted/50"
                            title={group.fullPath ?? "无关联工作目录"}
                          >
                            <div className="flex min-w-0 items-center gap-2">
                              <ChevronDown
                                className={cn(
                                  "h-3.5 w-3.5 shrink-0 text-muted-foreground transition-transform duration-150",
                                  collapsed && "-rotate-90",
                                )}
                              />
                              <Folder className="h-3.5 w-3.5 shrink-0 text-muted-foreground" />
                              <span className="truncate font-mono text-[11px] uppercase tracking-[0.14em] text-muted-foreground">
                                {group.label}
                              </span>
                            </div>
                            <Badge
                              variant="outline"
                              className="shrink-0 text-[10px]"
                            >
                              {group.sessions.length}
                            </Badge>
                          </button>
                          {!collapsed && (
                            <div className="divide-y divide-border">
                              {group.sessions.map((session) => {
                                const active = selectedPath === session.path;
                                const animationIndex = runningIndex++;
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
                                    style={{
                                      animationDelay: `${animationIndex * 28}ms`,
                                    }}
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
                                      <div className="flex shrink-0 flex-col items-end gap-1">
                                        <Badge variant={active ? "default" : "outline"}>
                                          {session.source}
                                        </Badge>
                                        {session.isSidechain && (
                                          <Badge
                                            variant="outline"
                                            className="text-[10px]"
                                          >
                                            subagent
                                          </Badge>
                                        )}
                                      </div>
                                    </div>

                                    {!!session.preview && (
                                      <div className="mt-2 overflow-hidden break-words text-sm leading-5 text-muted-foreground [display:-webkit-box] [-webkit-box-orient:vertical] [-webkit-line-clamp:2]">
                                        {session.preview}
                                      </div>
                                    )}

                                    {!session.isPartial && (
                                      <div className="mt-3 flex flex-wrap gap-x-3 gap-y-1 text-xs text-muted-foreground">
                                        <MetaInline
                                          icon={MessageSquareQuote}
                                          text={`${session.messageCount} msgs`}
                                        />
                                        <MetaInline
                                          icon={Hammer}
                                          text={`${session.toolCallCount} tools`}
                                        />
                                        {session.subagentCount > 0 && (
                                          <MetaInline
                                            icon={Network}
                                            text={`${session.subagentCount} agents`}
                                          />
                                        )}
                                        {session.gitBranch && (
                                          <MetaInline
                                            icon={GitBranch}
                                            text={session.gitBranch}
                                          />
                                        )}
                                      </div>
                                    )}
                                  </button>
                                );
                              })}
                            </div>
                          )}
                        </div>
                      );
                    });
                  })()}
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
                        <Badge variant="outline">{stats.eventCount} events</Badge>
                        <Badge variant="outline">{stats.toolCallCount} tools</Badge>
                        {stats.totalInputTokens + stats.totalOutputTokens > 0 && (
                          <Badge variant="outline">
                            {formatTokens(stats.totalInputTokens + stats.totalOutputTokens)} tok
                          </Badge>
                        )}
                      </>
                    ) : (
                      <Badge variant="outline">detail on demand</Badge>
                    )}
                    {currentSummary.isSidechain && (
                      <Badge variant="outline">sidechain</Badge>
                    )}
                    <Button
                      type="button"
                      variant="outline"
                      size="sm"
                      className="h-7 px-2 text-xs"
                      onClick={() => setRenderMarkdown((current) => !current)}
                      title={renderMarkdown ? "切换到纯文本" : "切换到 Markdown"}
                    >
                      <FileText className="h-3.5 w-3.5" />
                      {renderMarkdown ? "Markdown" : "Raw"}
                    </Button>
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
                  {currentSummary.gitBranch && (
                    <DetailInline
                      icon={GitBranch}
                      label="Branch"
                      value={currentSummary.gitBranch}
                    />
                  )}
                  {currentSummary.model && (
                    <DetailInline
                      icon={Sparkles}
                      label="Model"
                      value={currentSummary.model}
                    />
                  )}
                  <DetailInline
                    icon={FileText}
                    label="File"
                    value={formatDisplayPath(stats?.filePath ?? currentSummary.path)}
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
                  <DetailInline
                    icon={Hammer}
                    label="Tools"
                    value={stats ? String(stats.toolCallCount) : "On demand"}
                  />
                  {stats && stats.reasoningCount > 0 && (
                    <DetailInline
                      icon={Brain}
                      label="Reasoning"
                      value={String(stats.reasoningCount)}
                    />
                  )}
                  {stats &&
                    (stats.totalInputTokens > 0 || stats.totalOutputTokens > 0) && (
                      <DetailInline
                        icon={KeyRound}
                        label="Tokens"
                        value={formatTokenBreakdown(stats)}
                      />
                    )}
                </div>
              )}
            </div>

            <div className="relative min-h-0 flex-1">
              {detailLoading && (
                <div className="absolute inset-0 z-10 overflow-hidden">
                  {Array.from({ length: 6 }).map((_, index) => (
                    <MessageSkeleton key={index} />
                  ))}
                </div>
              )}

              {!detailLoading && detailError && (
                <div className="p-6">
                  <EmptyState title="详情没拉下来" description={detailError} />
                </div>
              )}

              {!detailLoading && !detailError && detail && detail.events.length === 0 && (
                <div className="p-6">
                  <EmptyState
                    title="这个 session 还没有可展示内容"
                    description="文件存在但解析器没读到任何事件。"
                  />
                </div>
              )}

              {!detailLoading && !detailError && !detail && (
                <div className="p-6">
                  <EmptyState
                    title="先选一个 session"
                    description="左边点一个条目，右边就会按需加载完整内容。"
                  />
                </div>
              )}

              {!detailLoading && !detailError && detail && detail.events.length > 0 && (
                <EventStream
                  events={detail.events}
                  sessionPath={detail.summary.path}
                  renderMarkdown={renderMarkdown}
                />
              )}
            </div>
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

function MetaInline({ icon: Icon, text }: { icon: LucideIcon; text: string }) {
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

function EventStream({
  events,
  sessionPath,
  renderMarkdown,
}: {
  events: SessionEvent[];
  sessionPath: string;
  renderMarkdown: boolean;
}) {
  const parentRef = useRef<HTMLDivElement | null>(null);
  const renderMode = renderMarkdown ? "markdown" : "raw";

  // sessionPath in the key forces a fresh virtualizer when the user switches sessions,
  // so cached measurements from the previous transcript don't bleed in.
  const virtualizer = useVirtualizer({
    count: events.length,
    getScrollElement: () => parentRef.current,
    estimateSize: (index) => estimateEventHeight(events[index]),
    overscan: 6,
    measureElement: (el) => el.getBoundingClientRect().height,
    getItemKey: (index) => `${renderMode}:${sessionPath}:${events[index].id}:${index}`,
  });

  const items = virtualizer.getVirtualItems();

  return (
    <div
      ref={parentRef}
      className="h-full overflow-auto"
      style={{ contain: "strict" }}
    >
      <div
        className="relative w-full"
        style={{ height: `${virtualizer.getTotalSize()}px` }}
      >
        {items.map((virtualRow) => {
          const event = events[virtualRow.index];
          return (
            <div
              key={virtualRow.key}
              data-index={virtualRow.index}
              ref={virtualizer.measureElement}
              className="absolute left-0 right-0 border-b border-border"
              style={{ transform: `translateY(${virtualRow.start}px)` }}
            >
              <EventRow event={event} renderMarkdown={renderMarkdown} />
            </div>
          );
        })}
      </div>
    </div>
  );
}

function estimateEventHeight(event: SessionEvent): number {
  switch (event.kind) {
    case "user":
    case "assistant_message":
      return Math.min(720, 120 + Math.floor(event.text.length / 4));
    case "reasoning":
      return Math.min(520, 100 + Math.floor(event.text.length / 5));
    case "tool_call":
      return event.summary ? 160 : 120;
    case "tool_result":
      return Math.min(620, 110 + Math.floor(event.content.length / 6));
    case "turn_context":
    case "permission_mode":
    case "compaction":
    case "lifecycle":
    case "attachment":
    case "file_snapshot":
    case "system_notice":
    case "token_usage":
    case "unknown":
      return 88;
    default:
      return 96;
  }
}

function EventRow({
  event,
  renderMarkdown,
}: {
  event: SessionEvent;
  renderMarkdown: boolean;
}) {
  switch (event.kind) {
    case "user":
      return (
        <EventShell
          tone="user"
          icon={UserRound}
          label="User"
          timestamp={event.timestamp}
        >
          <TextBody text={event.text} renderMarkdown={renderMarkdown} />
        </EventShell>
      );
    case "assistant_message":
      return (
        <EventShell
          tone="assistant"
          icon={Bot}
          label="Assistant"
          timestamp={event.timestamp}
          badges={[event.model, event.stopReason].filter(Boolean) as string[]}
        >
          <TextBody text={event.text} renderMarkdown={renderMarkdown} />
        </EventShell>
      );
    case "reasoning":
      return (
        <EventShell
          tone="reasoning"
          icon={Brain}
          label="Reasoning"
          timestamp={event.timestamp}
          badges={event.encrypted ? ["encrypted"] : []}
        >
          <TextBody
            text={event.text}
            renderMarkdown={renderMarkdown}
            muted
            italic
          />
        </EventShell>
      );
    case "tool_call":
      return (
        <EventShell
          tone="tool"
          icon={Hammer}
          label="Tool call"
          timestamp={event.timestamp}
          badges={[event.name]}
        >
          {event.summary ? (
            <pre className="overflow-x-auto whitespace-pre-wrap break-words font-mono text-[12px] leading-5 text-foreground/90">
              {event.summary}
            </pre>
          ) : (
            <CollapsibleJson value={event.input} />
          )}
        </EventShell>
      );
    case "tool_result":
      return (
        <EventShell
          tone={event.isError ? "error" : "tool-result"}
          icon={event.isError ? ShieldAlert : ListTree}
          label={event.isError ? "Tool error" : "Tool result"}
          timestamp={event.timestamp}
        >
          <pre className="overflow-x-auto whitespace-pre-wrap break-words font-mono text-[12px] leading-5 text-foreground/90">
            {event.content}
          </pre>
        </EventShell>
      );
    case "turn_context":
      return (
        <EventShell
          tone="meta"
          icon={Compass}
          label="Turn context"
          timestamp={event.timestamp}
        >
          <div className="font-mono text-[11px] text-muted-foreground">
            {event.summary ?? "(no context)"}
          </div>
        </EventShell>
      );
    case "token_usage":
      return (
        <EventShell
          tone="meta"
          icon={KeyRound}
          label="Token usage"
          timestamp={event.timestamp}
        >
          <div className="font-mono text-[11px] text-muted-foreground">
            {formatTokenLine(event)}
          </div>
        </EventShell>
      );
    case "compaction":
      return (
        <EventShell
          tone="meta"
          icon={Scissors}
          label="Compaction"
          timestamp={event.timestamp}
        >
          <TextBody
            text={event.summary ?? "(context compacted)"}
            renderMarkdown={renderMarkdown}
            muted
          />
        </EventShell>
      );
    case "lifecycle":
      return (
        <EventShell
          tone="meta"
          icon={Wand2}
          label={lifecycleLabel(event.event)}
          timestamp={event.timestamp}
        >
          <TextBody
            text={event.text ?? event.event}
            renderMarkdown={renderMarkdown}
            muted
          />
        </EventShell>
      );
    case "system_notice":
      return (
        <EventShell
          tone="meta"
          icon={Info}
          label="System"
          timestamp={event.timestamp}
          badges={
            [event.subtype, event.level].filter(Boolean) as string[]
          }
        >
          {event.content && (
            <pre className="overflow-x-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-5 text-muted-foreground">
              {event.content}
            </pre>
          )}
          {event.hookCount != null && (
            <div className="mt-1 text-xs text-muted-foreground">
              hooks: {event.hookCount}
            </div>
          )}
        </EventShell>
      );
    case "permission_mode":
      return (
        <EventShell
          tone="meta"
          icon={ShieldAlert}
          label="Permission"
          timestamp={event.timestamp}
        >
          <div className="font-mono text-xs text-muted-foreground">
            mode: {event.mode}
          </div>
        </EventShell>
      );
    case "attachment":
      return (
        <EventShell
          tone="meta"
          icon={Paperclip}
          label="Attachment"
          timestamp={event.timestamp}
          badges={[event.mime, event.name].filter(Boolean) as string[]}
        >
          {event.preview && (
            <pre className="overflow-x-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-5 text-muted-foreground">
              {event.preview}
            </pre>
          )}
        </EventShell>
      );
    case "file_snapshot":
      return (
        <EventShell
          tone="meta"
          icon={FileCog}
          label="File snapshot"
          timestamp={event.timestamp}
        >
          <div className="font-mono text-xs text-muted-foreground">
            tracked files: {event.fileCount ?? 0}
          </div>
        </EventShell>
      );
    case "unknown":
      return (
        <EventShell
          tone="meta"
          icon={History}
          label="Unknown"
          timestamp={event.timestamp}
          badges={[event.rawType]}
        >
          <div className="font-mono text-xs text-muted-foreground">
            {event.sourceType}
          </div>
        </EventShell>
      );
    default:
      return null;
  }
}

function TextBody({
  text,
  renderMarkdown,
  muted,
  italic,
}: {
  text: string;
  renderMarkdown: boolean;
  muted?: boolean;
  italic?: boolean;
}) {
  if (renderMarkdown) {
    return (
      <MarkdownBody muted={muted} italic={italic}>
        {text}
      </MarkdownBody>
    );
  }

  return (
    <div
      className={cn(
        "whitespace-pre-wrap break-words text-sm leading-6",
        muted ? "text-muted-foreground" : "text-foreground",
        italic && "italic",
      )}
    >
      {text}
    </div>
  );
}

const toneClass: Record<EventTone, string> = {
  user: "border-l-primary bg-primary/[0.02]",
  assistant: "border-l-border bg-background",
  reasoning: "border-l-purple-400/60 bg-purple-500/[0.04] dark:border-l-purple-300/40",
  tool: "border-l-amber-400/60 bg-amber-500/[0.04] dark:border-l-amber-300/40",
  "tool-result": "border-l-muted-foreground/40 bg-muted/35",
  error: "border-l-destructive bg-destructive/[0.05]",
  meta: "border-l-border bg-background",
};

type EventTone = "user" | "assistant" | "reasoning" | "tool" | "tool-result" | "error" | "meta";

function EventShell({
  tone,
  icon: Icon,
  label,
  timestamp,
  badges,
  children,
}: {
  tone: EventTone;
  icon: LucideIcon;
  label: string;
  timestamp?: string | null;
  badges?: string[];
  children: React.ReactNode;
}) {
  return (
    <article className={cn("border-l-2 px-5 py-3", toneClass[tone])}>
      <div className="flex flex-wrap items-center justify-between gap-2">
        <div className="flex flex-wrap items-center gap-2">
          <Icon className="h-4 w-4 text-muted-foreground" />
          <span className="text-sm font-medium text-foreground">{label}</span>
          {badges?.map((badge) => (
            <Badge
              key={badge}
              variant="outline"
              className="font-mono text-[10px] uppercase tracking-[0.12em]"
            >
              {badge}
            </Badge>
          ))}
        </div>
        <span className="font-mono text-[11px] text-muted-foreground">
          {timestamp ? formatDateLong(timestamp) : "—"}
        </span>
      </div>
      <div className="pt-2">{children}</div>
    </article>
  );
}

function CollapsibleJson({ value }: { value: unknown }) {
  const text = useMemo(() => {
    try {
      return JSON.stringify(value, null, 2);
    } catch {
      return String(value);
    }
  }, [value]);
  const [expanded, setExpanded] = useState(false);
  const isLong = text.length > 600;
  const display = expanded || !isLong ? text : `${text.slice(0, 600)}…`;
  return (
    <div>
      <pre className="overflow-x-auto whitespace-pre-wrap break-words font-mono text-[11px] leading-5 text-muted-foreground">
        {display}
      </pre>
      {isLong && (
        <button
          type="button"
          className="mt-2 text-[11px] font-mono uppercase tracking-[0.16em] text-primary hover:underline"
          onClick={() => setExpanded((v) => !v)}
        >
          {expanded ? "collapse" : "expand"}
        </button>
      )}
    </div>
  );
}

function lifecycleLabel(event: string) {
  switch (event) {
    case "task_started":
      return "Task started";
    case "task_complete":
      return "Task complete";
    case "turn_aborted":
      return "Turn aborted";
    case "last-prompt":
      return "Last prompt";
    case "ai-title":
      return "AI title";
    case "queue-operation":
    case "enqueue":
      return "Queue";
    default:
      return event;
  }
}

function formatTokenLine(event: {
  inputTokens?: number | null;
  outputTokens?: number | null;
  cacheReadTokens?: number | null;
  cacheCreationTokens?: number | null;
}) {
  const parts: string[] = [];
  if (event.inputTokens != null) parts.push(`in ${event.inputTokens}`);
  if (event.outputTokens != null) parts.push(`out ${event.outputTokens}`);
  if (event.cacheReadTokens != null) parts.push(`cache_r ${event.cacheReadTokens}`);
  if (event.cacheCreationTokens != null)
    parts.push(`cache_w ${event.cacheCreationTokens}`);
  return parts.length === 0 ? "(no usage data)" : parts.join(" · ");
}

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
    eventCount: summary.messageCount,
    userCount: summary.userCount,
    assistantCount: summary.assistantCount,
    toolCallCount: summary.toolCallCount,
    reasoningCount: 0,
    contentChars: 0,
    totalInputTokens: 0,
    totalOutputTokens: 0,
    cacheReadTokens: 0,
    cacheCreationTokens: 0,
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

const UNCATEGORIZED_KEY = "__uncategorized__";

interface SessionGroup {
  key: string;
  label: string;
  fullPath: string | null;
  sessions: SessionSummary[];
  latestUpdatedAt: string;
}

function groupSessionsByWorkspace(sessions: SessionSummary[]): SessionGroup[] {
  const buckets = new Map<string, SessionSummary[]>();
  for (const session of sessions) {
    const key = session.workingDir || session.projectPath || UNCATEGORIZED_KEY;
    let bucket = buckets.get(key);
    if (!bucket) {
      bucket = [];
      buckets.set(key, bucket);
    }
    bucket.push(session);
  }

  const groups: SessionGroup[] = [];
  for (const [key, bucket] of buckets) {
    const sorted = [...bucket].sort((a, b) =>
      b.updatedAt.localeCompare(a.updatedAt),
    );
    const isUncategorized = key === UNCATEGORIZED_KEY;
    groups.push({
      key,
      label: isUncategorized ? "未分类" : formatGroupLabel(key),
      fullPath: isUncategorized ? null : key,
      sessions: sorted,
      latestUpdatedAt: sorted[0]?.updatedAt ?? "",
    });
  }

  groups.sort((a, b) => b.latestUpdatedAt.localeCompare(a.latestUpdatedAt));
  return groups;
}

function formatGroupLabel(rawPath: string): string {
  const home = rawPath
    .replace(/^\/Users\/[^/]+/, "~")
    .replace(/^\/home\/[^/]+/, "~");
  const trimmed = home.replace(/\/+$/, "");

  if (trimmed === "~" || trimmed === "") {
    return "~";
  }

  if (trimmed.startsWith("~/")) {
    const segments = trimmed.slice(2).split("/").filter(Boolean);
    if (segments.length <= 3) {
      return trimmed;
    }
    return `~/${segments[0]}/…/${segments[segments.length - 1]}`;
  }

  const segments = trimmed.split("/").filter(Boolean);
  if (segments.length <= 3) {
    return trimmed;
  }
  return `/${segments[0]}/…/${segments[segments.length - 1]}`;
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

function formatTokens(count: number) {
  if (count >= 1_000_000) return `${(count / 1_000_000).toFixed(1)}M`;
  if (count >= 1_000) return `${(count / 1_000).toFixed(1)}k`;
  return String(count);
}

function formatTokenBreakdown(stats: SessionStats) {
  const parts: string[] = [];
  if (stats.totalInputTokens > 0) parts.push(`in ${formatTokens(stats.totalInputTokens)}`);
  if (stats.totalOutputTokens > 0) parts.push(`out ${formatTokens(stats.totalOutputTokens)}`);
  if (stats.cacheReadTokens > 0) parts.push(`cache ${formatTokens(stats.cacheReadTokens)}`);
  return parts.join(" · ") || "—";
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
