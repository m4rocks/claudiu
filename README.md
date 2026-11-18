# Claudiu

Claudiu is a native desktop app, written in Rust with [Rune](https://github.com/anomalyco/rune), that runs several [Claude Code](https://code.claude.com) sessions in one window on Windows, macOS and Linux. It is a personal project with rough edges: macOS and Linux are lightly tested, and the macOS build isn't notarized. Issues and pull requests are welcome.

> **Claudiu is an independent project. It is not affiliated with, endorsed by or sponsored by Anthropic.** Claude and Claude Code are trademarks of Anthropic.

## What it does

Each session is your own `claude` process in a real terminal, and sign-in stays with Claude Code. Around it, Claudiu adds a sidebar of running and recent sessions, notifications when a background session finishes or asks something, an editor mode (Ctrl+Shift+E), usage meters, a GitHub pane, cards Claude draws inline, sessions that move between git worktrees, extra accounts that share one configuration, and Codex and OpenCode subagents (Settings → Custom subagents). Codex subagents need Node and a `codex login`, OpenCode subagents need `opencode` on PATH, and with Allow edits on, either acts without Claude Code's permission rules. With Codex subagents on, the sidebar also shows Codex's 5-hour and 7-day limits, which Claudiu reads from the session logs in `~/.codex`. Settings lists the rest.

## What it touches

Claudiu never calls the Anthropic API or deletes a transcript, and it changes Claude Code's settings only when you flip a switch. Its network use is GitHub (the update check and the GitHub pane), plus npm and OpenAI once you turn on Codex subagents, and the providers you set up in OpenCode once you turn on OpenCode subagents. Terminal contents are never logged.

## Development

You need stable Rust, plus Visual Studio Build Tools on Windows or the headers Rune builds against on Linux, and [just](https://just.systems): `just run`, `just test`, `just ci`. `src/` is the app, `mod/` is the Claude Code plugin each session loads, and `web/` is the site. `CLAUDE.md` has the conventions. A debug build keeps its data in its own folder, so it runs beside the installed app; a second app on the same folder says Claudiu is already running and quits. To release, bump `version` in `Cargo.toml` and push a `vX.Y.Z` tag.
