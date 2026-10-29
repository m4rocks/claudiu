# Claudiu

Claudiu is a native desktop app, written in Rust with [GPUI](https://gpui.rs), that runs several [Claude Code](https://code.claude.com) sessions in one window on Windows and macOS. It is a personal project with rough edges. Issues and pull requests are welcome.

> **Claudiu is an independent project. It is not affiliated with, endorsed by or sponsored by Anthropic.** Claude and Claude Code are trademarks of Anthropic.

## What it does

Each session is your own `claude` process in a real pseudo-terminal, so Claude Code's keys, mouse and paste work unchanged. Claudiu has no API keys and never calls the Anthropic API. Sign-in stays with Claude Code. The first launch opens a setup window that checks for `claude` (required) and `gh` (optional), links to their install pages and installs nothing. Settings has a Check setup button to open it again. A sidebar lists your projects and sessions. Ended sessions resume through `claude --resume`, and sessions that were running come back at the next start.

Around the terminal, Claudiu adds an editor mode (Ctrl+Shift+E) with files, tabs, search, Git change marks and language-server diagnostics, definitions and completion. It also adds usage and context meters, a GitHub pane for issues and pull requests through your `gh` login, defaults for the model, effort and permission mode of new sessions, and extra Claude accounts that share one configuration. A session can move between git worktrees the way it switches branches, so several agents work on separate branches at once: the worktree button in the top bar switches the shown session into another worktree, or into a new one that Claudiu makes on a branch you name, and its conversation continues there. Worktrees that Claudiu makes live in its data folder, so they never show in your repository's `git status`. Its hover x removes one, the Worktrees page in Settings lists and removes them all, the branch button's x deletes a local branch after telling you which commits would be lost, and ending the last session in a worktree offers to remove it. Settings lists the rest.

Ctrl+Shift (Cmd+Shift on macOS) plus a letter drives the app. Everything else reaches Claude Code. Editor mode finds `rust-analyzer`, `tsc --lsp` and `gleam` on your PATH and installs nothing.

## What it touches

Claudiu never deletes a transcript. It writes to Claude Code's own data only to change the settings you switch in Claudiu, as `/config` does, to sync shared files between accounts, and to append one line to a transcript when a session that `claude --worktree` put in a worktree moves to another folder, so Claude Code knows it has left that worktree. Nothing is uploaded and terminal contents are never logged. The only network use is GitHub: the update check (off with `"settings": { "update_url": "" }` in `state.json`) and the GitHub pane. State lives in `%LOCALAPPDATA%\m4rocks\Claudiu\data` on Windows and `~/Library/Application Support/Claudiu` on macOS, and crashes go to `crash.log` there.

## Known gaps

- macOS is lightly tested, and the build isn't notarized, so Gatekeeper complains on first launch.
- Quitting ends running PTYs. Sessions stay resumable, but live processes don't survive a restart.

## Development

You need stable Rust. On Windows you also need Visual Studio Build Tools (C++) and the Windows SDK. Tasks go through [just](https://just.systems); run `just` to list them.

```sh
just run     # dev build: shows stderr, never checks for updates
just test    # app tests and mod tests (the mod tests need the claude CLI)
just lint    # clippy
just ci      # what CI checks, plus the mod and the web build
```

The repository has four parts:

- `src/` is the app, including the MCP server that each session connects to.
- `mod/` is the Claude Code plugin each session loads, which writes the usage snapshots.
- `web/` is the Astro site.
- `vendor/gpui-base/` is a patched copy of the editor crate. `Cargo.toml` says what the patch changes and when to drop it.

`CLAUDE.md` has the conventions for contributing. Releases use [Velopack](https://velopack.io) and GitHub Releases: bump `version` in `Cargo.toml`, then `git tag vX.Y.Z && git push --tags`. `scripts\package.ps1` builds a local installer.
