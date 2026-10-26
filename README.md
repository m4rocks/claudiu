# Claudiu

A native desktop app (Rust + [GPUI](https://gpui.rs)) for running several [Claude Code](https://code.claude.com) sessions in one window, on Windows and macOS. A personal project with rough edges; issues and PRs are welcome.

> **Claudiu is an independent project. It is not affiliated with, endorsed by or sponsored by Anthropic.** Claude and Claude Code are trademarks of Anthropic.

## How it works

- **Your own `claude`, in a real terminal.** Each session is one `claude` process in its own pseudo-terminal (ConPTY on Windows, a Unix PTY on macOS), so Alt+P, Shift+Tab, mouse and paste go straight to Claude Code. Claudiu types into the terminal in one case: the `@file#L` reference of selected lines in editor mode, which it removes if you leave without typing.
- **No API, no login.** Claudiu never calls the Anthropic API and has no keys or sign-in of its own. Authentication stays with Claude Code.
- **Sessions are records.** Ended sessions stay in the sidebar and resume through `claude --resume`; sessions that were running come back at the next start. Add a project with the button, by dropping a folder on the window, or with `claudiu <folder>`.
- **Per-session helpers.** Each session starts with `--session-id` or `--resume`, a `--plugin-dir` mod that writes usage snapshots (5-hour and 7-day limits, context, auto-compact, advisor) and a `--mcp-config` MCP server (tab titles, Visual panel, GitHub sidebar, tools for your other sessions, model switching). Settings → Claudiu tools switches each feature off.
- **Defaults and trims.** Settings → Sessions sets the model, effort, permission mode and Claude in Chrome for new sessions; `bypassPermissions` skips every prompt, so choose it with care. Settings → Claude Code tools switches off bundled skills, workflows, scheduled tasks, plan mode and more by writing them into Claude Code's `settings.json` ([why](https://www.aihero.dev/how-to-kill-the-bloat-in-claude-codes-system-prompt)). Nothing is trimmed by default.
- **Alt accounts.** An extra account is a separate config folder (`CLAUDE_CONFIG_DIR`) with only its own login. Folders (agents, skills, plugins, history) link to `~/.claude`; settings, `CLAUDE.md`, keybindings, MCP servers and project trust are kept the same in every account, both ways, within seconds. Only sign in to accounts you own.
- **GitHub, Git and editors.** Issues, PRs, Commit & Push and Pull use your `gh` and `git`. Open a project in VS Code or Zed.
- **Editor mode** (Ctrl+Shift+E). Files, tabs, quick open, find in files, Git change marks and highlighting beside the terminal. Diagnostics, hover and go-to-definition come from `rust-analyzer`, `tsc --lsp` or `gleam` on your PATH; Claudiu installs nothing. Right-click to ask Claude about a selection; Ctrl+click a path in the terminal to open it.

## What Claudiu touches

- Claudiu never deletes a transcript. It writes to Claude Code's own data in two cases: the sidebar's auto-compact, background-compaction and advisor switches and the tools page change Claude Code's settings like `/config`, and the sync between accounts copies the shared files between `~/.claude`, `~/.claude.json` and each alt's folder.
- Nothing is uploaded and terminal contents are never logged. Claudiu contacts only GitHub: the update check (off with `"settings": { "update_url": "" }` in `state.json`) and the GitHub pane through your `gh` login.
- State is in `%LOCALAPPDATA%\m4rocks\Claudiu\data` on Windows and `~/Library/Application Support/Claudiu` on macOS; crashes go to `crash.log` there. Children start with a clean environment and `TERM_PROGRAM=Claudiu`.

## Shortcuts

Ctrl+Shift (Cmd+Shift on macOS) plus N new session, W end it, E editor mode, B / R left / right sidebar, F search, O add project, G GitHub. Ctrl+Tab and Ctrl+1…9 switch sessions, Ctrl+J shows the shell, Ctrl+, opens settings. In editor mode: Ctrl+P quick open, Ctrl+S save, Ctrl+F / H find / replace, F12 definition, Alt+← / → back and forward. Everything else reaches Claude Code.

## Known gaps

- macOS is lightly tested, and the build isn't notarized, so Gatekeeper complains on first launch.
- Quitting ends running PTYs. Sessions stay resumable, but live processes don't survive a restart.

## Building

Stable Rust; on Windows also Visual Studio Build Tools (C++) and the Windows SDK. Tasks go through [just](https://just.systems); run `just` to list them.

```sh
just run     # dev build: shows stderr, never checks for updates
just test    # app tests and mod tests (the mod tests need the claude CLI)
just lint    # clippy
just ci      # what CI checks, plus the mod and the web build
```

Releases use [Velopack](https://velopack.io) and GitHub Releases: bump `version` in `Cargo.toml`, then `git tag vX.Y.Z && git push --tags`. Local installer: `scripts\package.ps1`.
