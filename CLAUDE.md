# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this app does

**Sessionary** is a local Tauri 2 desktop app for browsing on-disk session logs produced by two local AI coding agents:

- **Codex** (OpenAI) — JSONL rollouts under `~/.codex/sessions/**`, plus an aggregated `~/.codex/session_index.jsonl`.
- **Claude** (Anthropic CLI / Claude Code) — JSONL transcripts under `~/.claude/projects/**`, plus per-project `sessions-index.json` files.

It is read-only: the Rust backend walks those directories, parses each provider's distinct event schema into a normalized `SessionSummary` / `SessionDetail` shape, and the React UI lists sessions on the left and renders the conversation on the right. The codebase was initially scaffolded by Codex, which is why **Codex parsing is more thorough than Claude parsing** — improving Claude support (richer message extraction, working-dir inference from project-path slugs, sidechain/subagent handling) is the main reason new work happens here.

## Commands

Use `pnpm` (preferred — `build_and_open.sh` falls back to `npm` then to direct `node_modules/.bin` binaries).

```bash
pnpm install                    # install JS deps
pnpm dev                        # Vite dev server on http://127.0.0.1:1420
pnpm build                      # tsc --noEmit + vite build -> dist/
pnpm tauri dev                  # full Tauri dev (Rust + Vite together)
pnpm tauri build                # production bundle
./build_and_open.sh             # release build + open Sessionary.app (macOS)
./build_and_open.sh debug       # debug build instead
./build_and_open.sh --no-open   # build only, log binary path
cargo check --manifest-path src-tauri/Cargo.toml     # fast Rust typecheck
cargo build --manifest-path src-tauri/Cargo.toml     # debug Rust build
```

No test suite, no linter is wired in — `tsc` (run by `pnpm build`) is the only type guard on the TS side; `cargo check` / `cargo build` is the only guard on the Rust side. Run both after any non-trivial change.

The TS config has `noEmit: true` and Vite + SWC handles emission, so don't try to run `tsc` for output.

## Architecture

### Two processes, one Tauri command surface

`src-tauri/src/main.rs` registers exactly **two** Tauri commands; everything else is helpers:

- `list_sessions(source: "codex" | "claude") -> SessionSummary[]`
- `get_session_detail(source, path) -> SessionDetail`

The frontend calls them via `invoke()` from `@tauri-apps/api/core`. There is no IPC streaming, no file watcher, no DB — every UI navigation re-reads the relevant JSONL file from disk in a blocking thread (`async_runtime::spawn_blocking`).

### Path validation is the security boundary

`validate_session_path` (in `session_store.rs`) is the only thing standing between the frontend and arbitrary file reads. It resolves the per-source root (`~/.codex/sessions` or `~/.claude/projects`) and rejects any `path` argument that doesn't `starts_with` that root. **Never bypass this**, and when adding new commands that take paths, mirror the same guard.

### Two parsers, two schemas — keep them separate

The Rust file is large (~1k lines) because Codex and Claude write fundamentally different JSONL shapes. Don't try to unify them — keep the parsing in dedicated functions and let them produce the same normalized `SessionMessage` / `SessionStats` at the end.

**Codex JSONL (one event per line, all wrapped in `{ "type": ..., "payload": {...} }`):**
- `type: "session_meta"` — `payload.id`, `payload.cwd`, `payload.timestamp` (the source of truth for session ID and working directory).
- `type: "event_msg"` with `payload.type == "user_message"` — user turn; `payload.message` is the raw string.
- `type: "response_item"` with `payload.type == "message"` and `payload.role == "assistant"` — assistant turn; `payload.content` is an array of `{ type: "output_text" | "input_text", text }` parts.
- `type: "response_item"` with `payload.type == "function_call"` — tool call. `payload.name` + `payload.arguments` (a JSON-encoded **string**, not an object). Special-cased names: `exec_command` (extract `.cmd`), `update_plan` (extract `.plan[].step`), `apply_patch` (no extraction).

**Claude JSONL (one event per line, schema is top-level — no `payload` wrapper):**
- `type: "user"` — skip if `isMeta == true`. `message.content` can be a **string** (real user prompt) OR an **array** containing `{ type: "tool_result", content }` items (synthetic tool-result echoes — surfaced as a `role: "tool"` message, NOT counted as a user turn).
- `type: "assistant"` — `message.id` (use it to dedupe; one logical assistant turn can span multiple lines with the same id, and `read_claude_session_detail` merges their text). `message.content` is an array of `{ type: "text" | "tool_use" }` items. `tool_use` items have `name` and `input` (object).

The Codex index (`~/.codex/session_index.jsonl`) is an append-log of `{ id, thread_name, updated_at }` records — read all lines, last-write-wins per id. The Claude index is one `sessions-index.json` **per project directory**; `read_claude_index` walks all of them and merges.

### Frontend: one giant `App.tsx`, shadcn primitives, Tailwind

`src/App.tsx` (~830 lines, single default-exported component) holds all UI state: source toggle, session list, selected session, detail pane, theme. Helpers below it (`MessageBubble`, `EmptyState`, `formatDateLong`, `buildStatusLine`, etc.) are co-located on purpose — don't split them into separate files unless you're doing real reuse.

- `@/components/ui/*` are stock shadcn components (Badge, Button, DropdownMenu, ScrollArea, Skeleton, Separator, Card). `components.json` is configured for `baseColor: neutral`, `cssVariables: true`, alias `@/components`, utils `@/lib/utils`. Add components with `pnpm dlx shadcn@latest add <name>`.
- `@/lib/theme.ts` is the only place that touches `localStorage` / the `dark` class on `<html>`.
- The path alias `@/*` → `src/*` is set in both `tsconfig.json` and `vite.config.ts` — update both if you change it.

### How a session reaches the screen

1. User picks a source in the dropdown → `App` calls `invoke("list_sessions", { source })`.
2. Rust walks the source root (or uses the index when present), produces `SessionSummary[]`, sorts by `updated_at` desc, **truncates to 300**.
3. User clicks a row → `App` calls `invoke("get_session_detail", { source, path })`.
4. Rust re-opens that one file, streams it line by line, builds messages + stats, returns `SessionDetail`.
5. `MessageBubble` renders each message; tool calls get a different visual treatment (`messageVisuals` map keyed by `role`).

### Known rough edges in Claude support (likely targets for new work)

- `summarize_claude_session` (used by `list_sessions`) returns `tool_call_count: 0`, `user_count: 0`, `assistant_count: 0` — the counts only get filled in when `get_session_detail` is called, so the list view shows zeros.
- Claude sessions whose path includes a directory component named `subagents` are **silently skipped** in `collect_claude_session_files`. If the user wants to see subagent transcripts, that filter is where to start.
- `working_dir` for Claude comes from the index's `projectPath` (a slugged form like `-Users-bytedance--wtp-workspaces-...`), not the real cwd. There's no cwd extraction equivalent to Codex's `session_meta.payload.cwd`.
- Sidechain / branched conversations and resumed sessions aren't modeled — every JSONL file is treated as one linear thread.

When extending Claude parsing, **read a real `.jsonl` from `~/.claude/projects/` first** to confirm the shape — the format has evolved and the parser only handles the events listed above.

## Platform notes

- Targeted at macOS (`build_and_open.sh` calls `open -n …Sessionary.app`); the Rust code uses `dirs::home_dir()` so Linux/Windows paths resolve, but bundle assumptions are mac-only.
- Tauri config: window 1440×920, min 1120×700, `bundle.active = false` (so `tauri build` won't produce a `.dmg` unless flipped on).
- Vite dev server is pinned to `127.0.0.1:1420` with `strictPort: true` — matches `tauri.conf.json devUrl`.
