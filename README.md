# Agentoria

Agentoria is a read-only macOS app for browsing local coding-agent sessions and skills.

- Browse Claude Code and Codex conversations by recency or workspace. Long transcripts are indexed incrementally and rendered lazily.
- Browse local Skills for Claude Code, Codex, Kimi, ZCode, and Trae. Open Markdown, images, and source files from a collapsible file tree; scripts use a highlighted, read-only code editor.
- Follow links between files inside a Skill. Paths and symlinks cannot navigate outside its directory.
- Switch between the built-in light and dark themes. Preferences and the rebuildable SQLite index stay on your Mac.

Agentoria never modifies agent session or Skill files. It does not upload their content.

## Build on macOS

Requires macOS 15 or later, Xcode Command Line Tools (`xcode-select --install`), and Rust 1.92 or later. The project is tested with Rust 1.95.0. Then run:

```bash
./build_and_open.sh
```

This builds the release binary, ad-hoc signs it, installs `~/Applications/Agentoria.app`, and opens it. To build without opening the app, use `./build_and_open.sh --no-open`.

Run the tests with `cargo test --workspace --locked`.

## Local data

The index and preferences live in `~/Library/Application Support/Agentoria/`. The index stores offsets and metadata, not full transcript bodies; it can be rebuilt from your local sessions. On first launch, Agentoria copies preferences and migrates the index from an existing Sessionary installation without deleting the old data.

Only the source code is published here. There is no downloadable binary release yet.

Original code and artwork are licensed under MIT. See [LICENSE](LICENSE) and [third-party notices](THIRD_PARTY_NOTICES.md).
