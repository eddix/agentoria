# Agentoria

Agentoria is a read-only macOS app for browsing local coding-agent sessions and skills.

- Browse Claude Code and Codex conversations by recency or workspace. Long transcripts are indexed incrementally and rendered lazily.
- Find USER/ASSISTANT message text with `⌘F` or the transcript's **Find** button. Matching ignores case and searches the Markdown source. Use `Enter` / `Shift+Enter` or `⌘G` / `⌘⇧G` to cycle through matches, and `Esc` to close. The search bar reports skipped large or unreadable records and any result limit.
- Browse local Skills for Claude Code, Codex, Kimi, ZCode, and Trae. Open Markdown, images, and source files from a collapsible file tree; scripts use a highlighted, read-only code editor.
- Follow links between files inside a Skill. Paths and symlinks cannot navigate outside its directory.
- Switch between the built-in light and dark themes. Preferences and the rebuildable SQLite index stay on your Mac.

Agentoria never modifies agent session or Skill files. It does not upload their content.

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

The index and preferences live in `~/Library/Application Support/Agentoria/`. The index stores offsets and metadata, not full transcript bodies; it can be rebuilt from your local sessions. On first launch, Agentoria copies preferences and migrates the index from an existing Sessionary installation without deleting the old data.

Original code and artwork are licensed under MIT. See [LICENSE](LICENSE) and [third-party notices](THIRD_PARTY_NOTICES.md).
