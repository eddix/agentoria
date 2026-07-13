# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this app does

**Sessionary** is a local Tauri 2 desktop app for browsing on-disk session logs produced by two local AI coding agents:

- **Codex** (OpenAI) — JSONL rollouts under `~/.codex/sessions/**`, plus an aggregated `~/.codex/session_index.jsonl`.
- **Claude** (Anthropic CLI / Claude Code) — JSONL transcripts under `~/.claude/projects/**`, plus per-project `sessions-index.json` files.

It is read-only: the Rust backend walks those directories, parses each provider's distinct event schema into a normalized `SessionSummary` / `SessionDetail` shape, and the React UI lists sessions on the left and renders the conversation on the right. The codebase was initially scaffolded by Codex, then refactored to give Claude parity: subagent transcripts are now surfaced, list-view counts are real, per-event `cwd` / `gitBranch` / `model` are extracted, and assistant `thinking` blocks are preserved as reasoning events.

## Commands

Use `pnpm` (preferred — `build_and_open.sh` falls back to `npm` then to direct `node_modules/.bin` binaries).

```bash
pnpm install                    # install JS deps
pnpm dev                        # Vite dev server on http://127.0.0.1:1420
pnpm build                      # tsc + vite build -> dist/
pnpm tauri dev                  # full Tauri dev (Rust + Vite together)
pnpm tauri build                # production .app bundle (with icon)
./build_and_open.sh             # release build + open Sessionary.app (macOS)
./build_and_open.sh debug       # debug build (raw binary, no bundle)
./build_and_open.sh --no-open   # build only, log binary path
cargo check --manifest-path src-tauri/Cargo.toml     # fast Rust typecheck
cargo build --manifest-path src-tauri/Cargo.toml     # debug Rust build
```

No test suite, no linter is wired in — `tsc` (run by `pnpm build`) is the only type guard on the TS side; `cargo check` / `cargo build` is the only guard on the Rust side. Run both after any non-trivial change.

The TS config has `noEmit: true` and Vite + SWC handles emission, so don't try to run `tsc` for output.

`pnpm-workspace.yaml` lists `@swc/core` and `esbuild` under `allowBuilds` — pnpm 11 refuses to run their postinstall scripts otherwise, and the dev server / Vite build die without them. Don't delete that file.

## Build system

The build pipeline has three load-bearing pieces that have each bitten us at least once. Touch them with care.

### Use `pnpm tauri build` for release — never raw `cargo build --release`

Tauri 2's `tauri-build` crate inspects which cargo features are set. If `custom-protocol` is absent, it emits `cargo:rustc-cfg=dev` and the embedded webview loads `devUrl` (`http://127.0.0.1:1420`) instead of `frontendDist`. The CLI auto-injects `--features custom-protocol` for release; raw `cargo build --release` does not. Symptom: production binary launches but shows a permanently blank window.

The `custom-protocol` feature is defined in `src-tauri/Cargo.toml` so that, if someone really must run `cargo build --release` (e.g. CI without the Tauri CLI), they can pass `--features custom-protocol` explicitly. **Prefer the CLI path.**

### `bundle.active: true` is required for the macOS icon

`src-tauri/tauri.conf.json` sets `bundle.active: true`, `bundle.targets: ["app"]`, and lists `bundle.icon[]`. This causes `tauri build` to produce `target/release/bundle/macos/Sessionary.app` with `Contents/Resources/icon.icns` and an `Info.plist` whose `CFBundleIconFile` points at it. Without this, you only get the raw binary — Dock / window / Cmd+Tab all fall back to the generic executable icon. **Don't flip `bundle.active` back to `false`**; if you don't want a `.dmg`, the `targets: ["app"]` filter already keeps that off.

### `build_and_open.sh` flow

- Release: delegates the whole pipeline to `pnpm tauri build`, which runs `beforeBuildCommand` (`pnpm build`) → cargo release build with `custom-protocol` → `.app` bundling. Skip the manual `build_frontend` in this path or `tsc + vite build` runs twice.
- Debug: `cargo build` (no `custom-protocol`) + manual `build_frontend`. The resulting binary loads `devUrl`, so you also need `pnpm dev` running in another terminal — otherwise the same blank-window symptom appears. For interactive development, prefer `pnpm tauri dev`, which manages both processes.

## Architecture

### Two processes, one Tauri command surface

`src-tauri/src/main.rs` registers exactly **two** Tauri commands; everything else is helpers:

- `list_sessions(source: "codex" | "claude") -> SessionSummary[]`
- `get_session_detail(source, path) -> SessionDetail`

The frontend calls them via `invoke()` from `@tauri-apps/api/core`. There is no IPC streaming, no file watcher, no DB — every UI navigation re-reads the relevant JSONL file from disk in a blocking thread (`async_runtime::spawn_blocking`).

### Path validation is the security boundary

`validate_session_path` (in `session_store/mod.rs`) is the only thing standing between the frontend and arbitrary file reads. It **canonicalizes** both the source root (`~/.codex/sessions` or `~/.claude/projects`) and the candidate path, then rejects anything that doesn't `starts_with` the canonical root — symlink escapes included. **Never bypass this**, and when adding new commands that take paths, mirror the same guard.

### Module layout

`src-tauri/src/session_store/` is split into focused modules so the two parsers don't bleed into each other:

- `mod.rs` — `#[tauri::command]` entry points, `parse_source`, `validate_session_path`.
- `model.rs` — wire types (`SessionSource`, `SessionSummary`, `SessionDetail`, `SessionStats`, and the tagged `SessionEvent` enum). Single source of truth for what crosses the IPC boundary.
- `codex.rs` — Codex JSONL parser (~22 event variants).
- `claude.rs` — Claude JSONL parser (subagent-aware).
- `util.rs` — shared helpers (`home_dir`, `codex_root`, `claude_root`, `clean_title`, `pretty_json`, `truncate_chars`, etc.).

Don't try to merge the two parsers — they consume different shapes and converge only at `SessionEvent` / `SessionStats`.

### The `SessionEvent` discriminated union

`SessionEvent` is a Serde-tagged enum (`#[serde(tag = "kind", rename_all = "snake_case")]`) shared by both parsers and mirrored verbatim in `src/types.ts`. Current variants: `user`, `assistant_message`, `reasoning`, `tool_call`, `tool_result`, `turn_context`, `token_usage`, `compaction`, `lifecycle`, `system_notice`, `permission_mode`, `attachment`, `file_snapshot`, `unknown`. Every event carries an `EventMeta { id, timestamp, sourceType }` flattened in.

When adding a new variant: edit `model.rs`, then mirror the shape in `src/types.ts`, then add a `case "<kind>":` in `EventRow` (`src/App.tsx`) **and** a branch in `estimateEventHeight()` so the virtualizer reserves the right amount of space.

### Codex JSONL (one event per line, all wrapped in `{ "type": ..., "payload": {...} }`)

- `type: "session_meta"` — `payload.id`, `payload.cwd`, `payload.timestamp` (source of truth for session ID and working directory).
- `type: "event_msg"` with `payload.type == "user_message"` — user turn; `payload.message` is the raw string.
- `type: "response_item"` with `payload.type == "message"` and `payload.role == "assistant"` — assistant turn; `payload.content` is an array of `{ type: "output_text" | "input_text", text }` parts.
- `type: "response_item"` with `payload.type == "function_call"` — tool call. `payload.name` + `payload.arguments` (a JSON-encoded **string**, not an object). Special-cased names: `exec_command` (extract `.cmd`), `update_plan` (extract `.plan[].step`), `apply_patch` (left raw).
- `type: "response_item"` with `payload.type == "reasoning"` — emitted as `Reasoning` events; encrypted blobs flagged via `encrypted: true`.
- `type: "turn_context"` — captures cwd / model / sandbox / approval for the upcoming turn.
- `type: "event_msg"` with `payload.type == "token_count"` — folded into both per-event `TokenUsage` and aggregate `SessionStats` totals.

The Codex index (`~/.codex/session_index.jsonl`) is an append-log of `{ id, thread_name, updated_at }` records — read all lines, last-write-wins per id.

### Claude JSONL (flat top-level `type` — no `payload` wrapper)

Observed variants: `user`, `assistant`, `attachment`, `permission-mode`, `last-prompt`, `system`, `file-history-snapshot`, `queue-operation`. The parser handles all of them; unknowns are surfaced as `Unknown` events rather than swallowed.

- `type: "user"` — skip if `isMeta == true`. `message.content` is either a **string** (a real user prompt) or an **array** of `{ type: "tool_result", tool_use_id, content }` items (synthetic tool-result echoes — emitted as `ToolResult` events, NOT counted as user turns).
- `type: "assistant"` — `message.id` is the dedupe key; one logical assistant turn can span multiple lines with the same id, and `claude::handle_assistant` merges their `content` array. Items can be `text`, `thinking` (→ `Reasoning` event), or `tool_use` (→ `ToolCall` with a per-tool `summary` from `claude::summarize_tool_use` covering Bash / Read / Write / Edit / Glob / Grep / TodoWrite / Task / WebFetch / WebSearch / Skill). Token usage from `message.usage.{input_tokens, output_tokens, cache_read_input_tokens, cache_creation_input_tokens}` is aggregated into `SessionStats` and emitted as `TokenUsage` events.
- Per-event `cwd` and `gitBranch` are pulled off every line and used to update the summary — no more relying on the slugged `projectPath`.

The Claude index is one `sessions-index.json` **per project directory**; `claude::read_indexed_paths` walks all of them and merges, keyed by the canonical full path (sessionId alone collides between a parent session and its subagent files).

### Subagent handling (Claude only)

Subagent transcripts live at `~/.claude/projects/<project>/subagents/agent-*.jsonl`. They have `isSidechain: true`, an `agentId`, and a parent `sessionId` referring to the main thread. The parser:

1. Discovers them in `claude::list_sessions` and gives each a synthetic id `"{parentSessionId}::agent-{agentId}"` to avoid collisions when one parent spawns many subagents.
2. Marks them with `isSidechain: true`, `parentSessionId`, and `agentRole` / `agentNickname` (lifted from the file's first lines).
3. Counts how many subagents belong to each parent and stamps `subagentCount` onto the parent's `SessionSummary`. The list view shows that as a badge.

If you want to hide subagents again, the filter point is `claude::list_sessions`, not `collect_claude_session_files` (which now keeps them).

### Frontend: virtualized event stream, shadcn primitives, Tailwind

`src/App.tsx` (~1390 lines, single default-exported component) holds all UI state: source toggle, session list, selected session, detail pane, theme, collapsed group set. Helpers below it (`EventStream`, `EventRow`, `EventShell`, `CollapsibleJson`, `estimateEventHeight`, `EmptyState`, `formatDateLong`, `buildStatusLine`, `groupSessionsByWorkspace`, `formatGroupLabel`, etc.) are co-located on purpose — don't split them into separate files unless you're doing real reuse.

The conversation pane renders through `EventStream`, which wraps `@tanstack/react-virtual`'s `useVirtualizer`:

- Container is a plain `<div className="h-full overflow-auto" style={{ contain: "strict" }}>` — **not** wrapped in `ScrollArea`, because Radix's scroll viewport hides the scroll events the virtualizer needs.
- `getItemKey` is `${sessionPath}:${eventId}:${index}` so switching sessions fully invalidates the measurement cache.
- `estimateSize` uses per-variant heuristics in `estimateEventHeight()`; the actual height is re-measured on mount via `measureElement`.
- `overscan: 6` — bump if scroll feels janky on very heterogeneous sessions.

Sessions can be tens of thousands of events long; rendering them all at once was the original perf cliff. **Don't replace `EventStream` with a plain `.map()` — even "just for a small change".** If you add a new event variant, also add a row in `estimateEventHeight()`, or the virtualizer over-fetches and re-measures every row.

The sidebar (left column, 360px fixed) lists sessions grouped by workspace via `groupSessionsByWorkspace`: bucket key is `workingDir ?? projectPath ?? "未分类"`, sessions inside each group sort by `updatedAt` desc, groups across the list sort by their freshest session's `updatedAt` desc (so the most active workspace floats to the top). Groups are collapsible; collapsed keys live in a `Set<string>` in component state. Header labels go through `formatGroupLabel`, which substitutes `~` for the home prefix and collapses 4+ path segments down to `~/first/…/last`. The "未分类" bucket participates in the activity sort like any other group.

- `@/components/ui/*` are stock shadcn components (Badge, Button, DropdownMenu, ScrollArea, Skeleton, Separator, Card). `components.json` is configured for `baseColor: neutral`, `cssVariables: true`, alias `@/components`, utils `@/lib/utils`. Add components with `pnpm dlx shadcn@latest add <name>`.
- **`ScrollArea` is patched.** `src/components/ui/scroll-area.tsx` adds `[&>div]:!block` to the Radix `Viewport`. Radix's default viewport wraps children in a `display: table; min-width: 100%` div, which shrink-wraps to content width and silently breaks every `truncate` / `min-w-0` / `shrink-0` inside it. The override forces `display: block` so the inner container actually respects the 360px column. Don't remove it unless you've audited every consumer.
- `@/lib/theme.ts` is the only place that touches `localStorage` / the `dark` class on `<html>`.
- The path alias `@/*` → `src/*` is set in both `tsconfig.json` and `vite.config.ts` — update both if you change it.

### How a session reaches the screen

1. User picks a source in the dropdown → `App` calls `invoke("list_sessions", { source })`.
2. Rust always walks every `*.jsonl` under the source root and runs the per-source `scan_summary` for real counts. The index files (`~/.codex/session_index.jsonl`, per-project `sessions-index.json`) are read only to enrich the title / `updated_at` — they're never the authoritative source, because Codex evicts older entries from its index and an index-only loop would hide those historical sessions. After scanning, the list is sorted by `updated_at` desc and **truncated to 1000** (`LIST_LIMIT` in both `codex.rs` and `claude.rs`).
3. User clicks a row → `App` calls `invoke("get_session_detail", { source, path })`.
4. Rust re-opens that one file, streams it line by line, builds the `SessionEvent[]` and `SessionStats`, returns `SessionDetail`.
5. `EventStream` virtualizes the rendering; `EventRow` switches over the 14 kinds and routes through `EventShell` with a tone-specific border colour.

## Platform notes

- Targeted at macOS (`build_and_open.sh` calls `open -n …Sessionary.app`); the Rust code uses `dirs::home_dir()` so Linux/Windows paths resolve, but bundle assumptions are mac-only.
- Tauri config: window 1440×920, min 1120×700, `bundle.active = true` with `bundle.targets: ["app"]` — produces `Sessionary.app` (with icon) but not `.dmg`. See the **Build system** section above before changing either field.
- Vite dev server is pinned to `127.0.0.1:1420` with `strictPort: true` — matches `tauri.conf.json devUrl`.
- Rust toolchain comes from Homebrew's `rustup` formula. `~/.zshenv` prepends `/opt/homebrew/opt/rustup/bin` to `PATH` so `cargo` / `rustc` resolve directly to the active toolchain. If you ever see `cargo: no such file or directory`, the shim is stale — `brew reinstall rustup` and re-source `~/.zshenv` rather than invoking a hardcoded toolchain path.
