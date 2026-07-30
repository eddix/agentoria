// Lightweight localStorage-backed UI preferences. Mirrors the pattern in
// theme.ts: pure resolve/persist helpers, all guarded for non-browser envs.

export type ViewMode = "recent" | "grouped";

const VIEW_MODE_KEY = "sessionary-view-mode";
const HIDE_SUBAGENTS_KEY = "sessionary-hide-subagents";

export function resolveViewMode(): ViewMode {
  if (typeof window === "undefined") {
    return "recent";
  }
  const stored = window.localStorage.getItem(VIEW_MODE_KEY);
  return stored === "grouped" ? "grouped" : "recent";
}

export function persistViewMode(mode: ViewMode) {
  if (typeof window === "undefined") {
    return;
  }
  window.localStorage.setItem(VIEW_MODE_KEY, mode);
}

export function resolveHideSubagents(): boolean {
  if (typeof window === "undefined") {
    return true;
  }
  const stored = window.localStorage.getItem(HIDE_SUBAGENTS_KEY);
  // default ON — subagents are noise when hunting for the main thread
  return stored === null ? true : stored === "true";
}

export function persistHideSubagents(hide: boolean) {
  if (typeof window === "undefined") {
    return;
  }
  window.localStorage.setItem(HIDE_SUBAGENTS_KEY, String(hide));
}

// Draggable sidebar width. Clamped so neither pane collapses; the window min
// width is 1120 (tauri.conf.json), so 640 max still leaves the transcript room.
const SIDEBAR_WIDTH_KEY = "sessionary-sidebar-width";
export const SIDEBAR_WIDTH_MIN = 260;
export const SIDEBAR_WIDTH_MAX = 640;
const SIDEBAR_WIDTH_DEFAULT = 360;

export function clampSidebarWidth(width: number): number {
  return Math.min(SIDEBAR_WIDTH_MAX, Math.max(SIDEBAR_WIDTH_MIN, width));
}

export function resolveSidebarWidth(): number {
  if (typeof window === "undefined") {
    return SIDEBAR_WIDTH_DEFAULT;
  }
  const stored = window.localStorage.getItem(SIDEBAR_WIDTH_KEY);
  const parsed = stored ? Number.parseInt(stored, 10) : NaN;
  return Number.isFinite(parsed) ? clampSidebarWidth(parsed) : SIDEBAR_WIDTH_DEFAULT;
}

export function persistSidebarWidth(width: number) {
  if (typeof window === "undefined") {
    return;
  }
  window.localStorage.setItem(SIDEBAR_WIDTH_KEY, String(Math.round(width)));
}
