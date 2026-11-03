# Agentoria

Agentoria is a read-only macOS app for browsing local coding-agent sessions, Skills, hooks, and MCP configuration.

- Browse Claude Code, Codex, Grok Build, and Trae CN desktop conversations by recency or workspace. Long transcripts are rendered lazily.
- Filter Sessions by work directory with the embedded Rust `nucleo-matcher` engine. Type abbreviations or multiple terms; `^prefix`, `suffix$`, `'exact`, and `!exclude` are supported, as is `~/`. Matching uses smart case and Unicode normalization. Grouped orders matching directories by score, with sessions newest first inside each group; Recent keeps chronological order. Escape in the filter clears it. The query stays when switching source or view mode and does not search transcript text or session titles.
- The existing **Hidden / Shown** control includes Codex auxiliary sessions such as guardian reviews. Cached classifications are refreshed before displaying the list without rebuilding transcript offsets or changing source files.
- Find USER/ASSISTANT message text with `⌘F` or the transcript's **Find** button. Matching ignores case and searches the Markdown source. Use `Enter` / `Shift+Enter` or `⌘G` / `⌘⇧G` to cycle through matches, and `Esc` to close. The search bar reports skipped large or unreadable records and any result limit.
- Browse local Skills for Claude Code, Codex, Grok Build, Kimi, ZCode, and Trae. Open Markdown, images, and source files from a collapsible file tree; scripts use a highlighted, read-only code editor.
- Follow links between files inside a Skill. Paths and symlinks cannot navigate outside its directory.
- Browse hook handlers and MCP server definitions for Claude Code, Codex, and Grok Build. Filter by name, event, matcher, transport, or source; view a highlighted, read-only JSON representation with its source file and scope. The selected session supplies the project. Use **Refresh** to reread configuration and `⌘F` to focus the filter.
- Switch between the built-in light and dark themes. Preferences and the rebuildable SQLite index stay on your Mac.
- Quit with `⌘Q`, the application menu, or by closing the last window.

Agentoria never modifies agent sessions, Skills, or agent configuration. It does not upload their content or execute hooks and MCP servers.

## Download

Download the ZIP for your Mac's architecture from [GitHub Releases](https://github.com/eddix/agentoria/releases). Unzip it and move `Agentoria.app` to Applications. The app is ad-hoc signed, not Apple notarized, so macOS may block the downloaded app.

## Build on macOS

Requires macOS 15 or later, Xcode Command Line Tools (`xcode-select --install`), and Rust 1.92 or later. The project is tested with Rust 1.95.0. Then run:

```bash
./build_and_open.sh
```

This builds the release binary, ad-hoc signs it, installs `~/Applications/Agentoria.app`, and opens it. To build without opening the app, use `./build_and_open.sh --no-open`.

Run the tests with `cargo test --workspace --locked`.

## Local data

Trae CN desktop sessions are read directly from the encrypted `~/Library/Application Support/Trae CN/ModularData/ai-agent/database.db`, including its active WAL. The current `ai-agent` database takes precedence over the older `ai-chat` location, so migrated history is not duplicated. This integration targets the verified Trae CN 3.4.13 database schema; it is separate from Trae CLI. No running Trae process, Keychain query, internal IPC, or plaintext database export is needed. `AGENTORIA_TRAE_CN_DATA_DIR` can override the `ModularData` root.

Select **Trae CN** in Sessions. On first use, enter your own installation's 64-character hexadecimal session key. Agentoria validates it before saving it to `~/.config/agentoria/trae-cn.json`, with directory mode `0700` and file mode `0600`. The file contains the key in plaintext as requested for local reuse; keep it private. The key is not part of preferences, the transcript index, logs, or the application binary. Use **Session key** in the sidebar to replace it; a failed validation preserves the previous saved key. Formal and Preview share this user-supplied credential file.

Trae CN's catalog and message references are queried from its native SQLite indexes, without a duplicate transcript cache. Original USER text comes from `chat_message.user_message_context.query`; older migrated rows without that field fall back only to their own linked `user_input` history. Explicitly empty original inputs stay empty. The sidebar counts user turns rather than model response chunks; assistant text comes from `history_v2.raw_messages`, and child agent runs with an explicit `agent_run.parent_run_id` stay in process details. Archived conversation messages remain available; soft-deleted sessions and records are excluded. Multi-root projects use the first available workspace folder for grouping. Database/WAL changes refresh the catalog; **Refresh** rereads it manually. These desktop sessions are currently all treated as ordinary sessions at the session-list level.

Grok Build sessions are read from `~/.grok/sessions/` (or `$GROK_HOME/sessions/`). Agentoria prefers the complete `updates.jsonl` timeline, merges adjacent text chunks, and falls back to `chat_history.jsonl` for older sessions. It reads titles, workspace, model, and session dates from `summary.json`; changes to that file refresh the list without rescanning the transcript. Grok Skills browsing includes built-in skills in `~/.grok/bundled/skills` and the standard `.grok`, `.agents`, `.claude`, and `.cursor` user/project `skills/` and `commands/` directories; custom runtime configuration and plugin caches are outside this catalog.

The index and preferences live in `~/Library/Application Support/Agentoria/`. The index stores offsets and metadata, not full transcript bodies; it can be rebuilt from your local sessions. Entries whose source files have been removed are pruned before showing the cached list and after filesystem deletion or rename events. Permission and other I/O errors do not discard cached entries. On first launch, Agentoria copies preferences and migrates the index from an existing Sessionary installation without deleting the old data.

The Hooks and MCP catalogs read fixed user/project configuration locations: Claude `settings*.json`, `~/.claude.json`, and project `.mcp.json`; Codex `config.toml` and `hooks.json`; Grok `config.toml` and `hooks/*.json`. Grok also includes Claude and Cursor compatibility definitions unless disabled by its on-disk `compat` settings. `CLAUDE_CONFIG_DIR`, `CODEX_HOME`, and `GROK_HOME` override the respective configuration directories. Entries retain separate file/scope identities even when names match. The catalog shows definitions in these files, not runtime connectivity, trust, or the final merged configuration. Plugin bundles, organization-managed configuration, ancestor-project discovery, CLI overrides, and dynamically injected servers are outside this catalog.

Configuration files are limited to 2 MiB each, hook directories to 256 entries, and the catalog to 4,096 definitions / 8 MiB of formatted detail. Limits and read/parse failures are shown in the view. Explicit token/password/secret fields, environment/header values, and HTTP URL credentials/query strings are masked before retaining details; arbitrary secrets embedded in shell commands or arguments are not detected. Configuration details are held in memory and are not written to the session index.

Original code and artwork are licensed under MIT. See [LICENSE](LICENSE) and [third-party notices](THIRD_PARTY_NOTICES.md).
