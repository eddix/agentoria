# Agentoria Development Guide

## Overview

Agentoria is a native macOS GPUI application for browsing local Claude Code, Codex, and Grok Build JSONL sessions and Agent Skills. It is read-only with respect to the agent source directories.

The application is a Cargo workspace:

- `crates/agentoria-core`: source discovery, Claude/Codex/Grok parsing, SQLite byte-offset index, lazy record loading, and benchmarks.
- `crates/agentoria-gpui`: GPUI application state, virtualized session/transcript and skill views, Markdown, process/raw drawers, preferences, file watching, and packaging entry points.

The final Tauri implementation is preserved by the `tauri-final` git tag. Do not reintroduce WebView, React, or Tauri dependencies on main.

## Commands

```bash
cargo check --workspace --locked
cargo test --workspace --locked
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo fmt --all -- --check
```

Build and install the formal app:

```bash
./build_and_open.sh
./build_and_open.sh --no-open
```

Build the isolated Preview channel:

```bash
./build_gpui_preview.sh release
```

Run the large-file index benchmark without printing session content:

```bash
cargo run -p agentoria-core --example index_bench --release --locked -- \
  codex /absolute/path/to/session.jsonl
```

## Dependency Rules

- `crates/agentoria-gpui/Cargo.toml` uses `gpui-kit = "0.7.1"`; `Cargo.lock` pins the resolved release. Kit re-exports the matching GPUI, base, component, platform, and assets crates.
- Import GPUI types through `gpui-kit` rather than adding a second GPUI source. Distinct GPUI package IDs produce incompatible `App`, `Window`, and element types.
- Keep `Cargo.lock` committed and use `--locked` in verification and packaging commands.

## Index Invariants

The SQLite index is a rebuildable cache. It stores file identity, timestamps, byte ranges, event kinds, counts, hashes, and display metadata; it does not store full conversation or process bodies.

- Only append when device/inode, prefix hash, and scanned-boundary hash still match.
- Rebuild after truncation, replacement, fingerprint mismatch, corruption, or incompatible schema.
- A final JSONL record without a newline is pending, not corrupt. Leave `scanned_offset` before it and retry after the next append.
- A malformed complete interior line is recorded in `index_errors`, skipped, and does not make the full session unreadable.
- Scanner cancellation must roll back the current SQLite transaction.
- Never write into Claude, Codex, or Grok source directories.
- Before publishing a cached source list and after the initial scan, reconcile confirmed missing/non-file paths with the index in a cancellable transaction. Deleting a file row cascades to its line/error rows. Do the same after filesystem removals, renames (including whole directories), watcher errors, or rescan flags; publish removed IDs so the UI drops stale rows and invalidates an affected selected/pending session. Preserve cache entries on permission/other I/O errors; an incomplete directory discovery must not be treated as proof of deletion.
- Grok's `updates.jsonl` is the authoritative complete timeline. Prefer it over the model-context `chat_history.jsonl`, which can shrink after compaction; discover exactly one transcript per session at `<sessions>/<cwd-group>/<session-id>/`. Ignore event/rewind/checkpoint logs. Read bounded `summary.json` metadata even on unchanged scans, and map summary file-watch events back to the canonical transcript.
- Grok ACP text chunks have millisecond timestamps and are merged only when adjacent (user chunks must also share `promptIndex`). The last chunk retains an unsuppressed byte span over the whole message; lazy loading concatenates that span's JSON records. Preserve whitespace-only chunks and enforce the same inline source-byte limit on the complete span. Switching between Grok transcript formats reuses the file ID and rebuilds offsets in the same cancellable transaction.

The Preview and formal channels use isolated directories:

- Preview: `~/Library/Application Support/Agentoria GPUI Preview`
- Formal: `~/Library/Application Support/Agentoria`

On first launch, Agentoria copies preferences and uses SQLite's online backup API to migrate the index from the corresponding Sessionary directory, without deleting the old data.

## UI Architecture

- The application entry point registers the global Quit action, `⌘Q`, the native application menu, and `QuitMode::LastWindowClosed`. Formal and Preview share this lifecycle behavior; quitting must work regardless of which input or view has focus.
- `WorkspaceView` owns source selection, progressive indexing, grouping/filtering, preferences, the resizable split, and selected-session lifecycle.
- `SessionView` owns transcript virtualization, layout-driven body loading, bounded LRU state, tail following, scroll anchors, and Markdown/Raw mode. Only one body-load batch (up to 16 records) runs at a time on GPUI's background executor; the next batch follows the latest rendered range and obsolete results are discarded. The cache holds up to 128 entries / 64 MiB of actual text, with currently rendered rows protected from eviction; oversized records count as metadata, not resident source bytes. Read failures stay visible until an explicit retry or cache eviction.
- Transcript text uses `SharedString`; process metadata is shared as `Arc<[IndexedLine]>` between the summary and drawer. Do not deep-clone either on every frame.
- In-session Find scans indexed USER/ASSISTANT text on the background executor after a 200 ms debounce. Query revisions and a cancellation token prevent stale results; session drop cancels scanning. Only UTF-8 source ranges are retained (first 10,000 matches, with the complete total count). Records above the 512 KiB inline source limit and unreadable records are counted explicitly in the search bar. The active Markdown match owns a single parsed text entity, maps its source range to rendered text, and uses theme-aware highlights; Raw mode highlights source text. Process payloads are outside this search scope.
- `SkillsView` scans a fixed set of Claude Code, Codex, Grok Build, Kimi, ZCode, and Trae user/project skill roots on a background thread. The list reads bounded metadata; selecting a skill scans its bounded directory tree and loads local Markdown, source, or image files on demand. A collapsible, independently resizable right pane virtualizes the file tree. Text files are capped at 2 MiB and images at 20 MiB. Symlinks and Markdown links cannot navigate outside the selected skill. It does not scan plugin caches or remote/cloud skills.
- Skill filtering runs only over scanned names and full descriptions for the selected Agent. Name prefix/substring/subsequence matches outrank description matches; the query is cleared on Agent changes and retained when switching between Sessions and Skills.
- Process and oversized raw records use paged drawers. Never materialize an entire large session or oversized line in the UI.
- Use `gpui::list` for variable-height transcripts. Do not replace it with `gpui_component::VirtualList`, which requires caller-supplied exact item sizes.
- GPUI entities and background tasks use revision/cancellation checks. Results from an older source or session must never overwrite current UI state.
- Do not read `ListState` from inside its scroll callback; GPUI invokes that callback while holding an internal mutable borrow. Defer exact anchor reads with `window.defer`. Load rows observed during layout after the frame, since restoration, resizing, and async row-height changes need not emit scroll events.

## Performance Gates

Formal promotion requires release evidence:

- Cached session list usable in at most 500 ms.
- Indexed session first screen in at most 500 ms.
- Transcript scroll p95 at most 16 ms.
- Largest-session RSS at most 400 MiB.

`session_view::tests::transcript_scroll_p95_stays_within_one_frame` enforces the release scroll gate. Run it with `--release -- --nocapture`; debug timing is informational only.

`session_view::tests::large_transcript_keeps_cache_bounded_across_viewports` uses a temporary JSONL fixture with 10,000 conversation records and 100,000 process metadata rows. It enforces the indexed first-screen gate in release mode and checks cache bounds across 73 viewport jumps. This synthetic fixture does not replace real-session RSS and end-to-end indexing measurements.

## Packaging

`build_gpui_formal.sh` and `build_gpui_preview.sh` construct standard `.app` bundles, copy `assets/icon.icns`, ad-hoc sign, verify, install into `~/Applications`, and remove staging bundles so Spotlight does not see build duplicates.

The formal app uses bundle ID `com.eddix.agentoria`. The Preview uses `com.eddix.agentoria.gpui-preview`.
