# Sessionary Development Guide

## Overview

Sessionary is a native macOS GPUI application for browsing local Claude Code and Codex JSONL sessions. It is read-only with respect to `~/.claude` and `~/.codex`.

The application is a Cargo workspace:

- `crates/sessionary-core`: source discovery, Claude/Codex parsing, SQLite byte-offset index, lazy record loading, and benchmarks.
- `crates/sessionary-gpui`: GPUI application state, virtualized session/transcript views, Markdown, process/raw drawers, preferences, file watching, and packaging entry points.

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
cargo run -p sessionary-core --example index_bench --release --locked -- \
  codex /absolute/path/to/session.jsonl
```

## Dependency Rules

- `gpui-component` is pinned to an exact git revision in `crates/sessionary-gpui/Cargo.toml`.
- GPUI uses the same Zed git source as `gpui-component`; `Cargo.lock` pins the single resolved graph to `f66ed399cdde86092af8af3dc7b418abf45f37f8`.
- Do not add an absolute local path dependency or a second GPUI source. Two GPUI source IDs produce incompatible `App`, `Window`, and element types.
- Keep `Cargo.lock` committed and use `--locked` in verification and packaging commands.

## Index Invariants

The SQLite index is a rebuildable cache. It stores file identity, timestamps, byte ranges, event kinds, counts, hashes, and display metadata; it does not store full conversation or process bodies.

- Only append when device/inode, prefix hash, and scanned-boundary hash still match.
- Rebuild after truncation, replacement, fingerprint mismatch, corruption, or incompatible schema.
- A final JSONL record without a newline is pending, not corrupt. Leave `scanned_offset` before it and retry after the next append.
- A malformed complete interior line is recorded in `index_errors`, skipped, and does not make the full session unreadable.
- Scanner cancellation must roll back the current SQLite transaction.
- Never write into Claude or Codex source directories.

The Preview and formal channels use isolated directories:

- Preview: `~/Library/Application Support/Sessionary GPUI Preview`
- Formal: `~/Library/Application Support/Sessionary`

## UI Architecture

- `WorkspaceView` owns source selection, progressive indexing, grouping/filtering, preferences, the resizable split, and selected-session lifecycle.
- `SessionView` owns transcript virtualization, visible-range body loading, bounded LRU state, tail following, scroll anchors, and Markdown/Raw mode.
- Process and oversized raw records use paged drawers. Never materialize an entire large session or oversized line in the UI.
- Use `gpui::list` for variable-height transcripts. Do not replace it with `gpui_component::VirtualList`, which requires caller-supplied exact item sizes.
- GPUI entities and background tasks use revision/cancellation checks. Results from an older source or session must never overwrite current UI state.
- Do not read `ListState` from inside its scroll callback; GPUI invokes that callback while holding an internal mutable borrow. Use `ListScrollEvent.visible_range` instead.

## Performance Gates

Formal promotion requires release evidence:

- Cached session list usable in at most 500 ms.
- Indexed session first screen in at most 500 ms.
- Transcript scroll p95 at most 16 ms.
- Largest-session RSS at most 400 MiB.

`session_view::tests::transcript_scroll_p95_stays_within_one_frame` enforces the release scroll gate. Run it with `--release -- --nocapture`; debug timing is informational only.

## Packaging

`build_gpui_formal.sh` and `build_gpui_preview.sh` construct standard `.app` bundles, copy `assets/icon.icns`, ad-hoc sign, verify, install into `~/Applications`, and remove staging bundles so Spotlight does not see build duplicates.

The formal app uses bundle ID `com.eddix.sessionary`. The Preview uses `com.eddix.sessionary.gpui-preview`.
