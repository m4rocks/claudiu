# m4code

m4code is a native desktop workspace, written in Rust with [GPUI](https://gpui.rs), for running coding agent sessions (**Claude Code**, **Codex**, **OpenCode**, and **Google Antigravity**) in one window on Windows, macOS, and Linux.

It is a personal project with rough edges: macOS and Linux are lightly tested, and the macOS build isn't notarized. Prebuilt binaries are on [Releases](https://github.com/m4rocks/m4code/releases/latest). Issues and pull requests are welcome.

> **m4code is an independent project. It is not affiliated with, endorsed by, or sponsored by Anthropic.** Claude and Claude Code are trademarks of Anthropic.

## Supported agents

Every session runs in m4code's native chat view over the [Agent Client Protocol](https://agentclientprotocol.com) (ACP). Sign-in stays with each agent. **Settings → Harnesses** enables or disables harnesses, selects which models appear in the picker, and configures subagent and edit permissions.

| Agent | Requirements | Notes |
| --- | --- | --- |
| **Claude Code** | `claude` CLI, Node, npm | Runs through the [Claude Agent SDK](https://code.claude.com/docs/en/agent-sdk/overview) (installed into m4code's data folder on first run) using whatever sign-in your `claude` has (subscription, API key, Bedrock, or Vertex). Disables `claude.ai`-only tools (scheduled tasks, push notifications, connectors) for m4code sessions only. Supports multiple accounts sharing one config and a Claude in Chrome toggle. |
| **Codex** | Node, `codex login` | Runs through its ACP adapter from npm (downloaded on first run). |
| **OpenCode** | `opencode` on `PATH` | Uses the providers configured in OpenCode. |
| **Google Antigravity** | `agy` CLI, signed in | Opt-in for Claude Code's native subagent integration; excluded from shared MCP subagent tools for now. |

> **Antigravity terms note**: Google's [Antigravity terms](https://antigravity.google/terms) and [FAQ](https://antigravity.google/docs/faq) forbid using another company's agent (such as Claude Code) with a personal Google sign-in (Free, AI Plus, Pro, or Ultra), and Google can suspend or terminate accounts that do. Turn Antigravity subagents on at your own risk.

## Features

### Chat and model handoff

- **Unified composer**: Home and chat share the same composer with chips for model (across all enabled harnesses), project, effort level, permission mode (including Claude Code Auto mode when offered), and branch or worktree. Press `Alt+P` (`Option+P` on macOS) in a chat to search models or adjust effort.
- **In-place harness handoff**: Picking a model on another harness hands off the conversation on your next message, or when you ask an agent to switch via `set_model`. Returning to a previous harness resumes its saved provider session and sends only the intervening conversation. A failed handoff restores the original session and unsent draft.
- **Steer or queue while working**: **Settings → General** chooses what happens when you send a message mid-turn:
  - **Steer** (default): feeds the follow-up into the running turn (Claude Code reads it after its current step; Codex uses native steering when supported; other adapters stop the current turn and start the follow-up).
  - **Queue**: waits for the turn to finish, showing queued messages above the composer with edit, remove, and send-now controls. Interrupting a turn or restarting holds the queue until you resume.
  - Press `Cmd/Ctrl+Enter` (or `Cmd/Ctrl`-click Send) to swap Steer and Queue for a single message.
- **Questions and background shells**: Answer agent questions with the keyboard or mouse, or type your own answer for Claude Code. Codex can ask questions while working without blocking chat. Claude Code background shells appear above the composer with Output and Stop controls.
- **Context compaction**: Schedules compaction at the next idle boundary once reported context reaches 80%, using the adapter's native compact command while keeping the provider session. Native automatic compaction is disabled for Claude Code and OpenCode; Codex retains a native fallback noted in the status panel.
- **Keyboard shortcuts**: Supports familiar Claude Code shortcuts (`Esc` to stop, `Esc` twice to edit your last message, `Shift+Tab` for permission mode, `Up` for message history, `Shift+Enter` for a new line). **Settings → Keyboard** lists every shortcut.

### Workspace, Git, and editor

- **Branches and worktrees**: Create a branch or spin up a new worktree (from `main` or the current checkout) in one click from the branch chip, inspect external worktrees, or move a session between worktrees.
- **Change review and snapshots**: Before each message in a git repository, m4code saves the working tree. **Review changes** under the composer shows hunk-by-hunk diffs since your last message—keep or revert individual hunks, or comment on lines (`@path#L12: comment`, `Cmd/Ctrl+Enter` to send).
- **Built-in editor (`Ctrl+Shift+E`)**: File tree, tabs, syntax highlighting, Git gutter marks, and LSP diagnostics, hover, completions, and go-to-definition using language servers already on your `PATH`. Works in active sessions and on Home, keeping each project's tabs and unsaved edits. Click the comment button beside hovered or selected lines to add `@path#L12-30: comment` to the composer.
- **Shell panel (`Ctrl+J` / `Ctrl+\``)**: A real PTY shell in the session's folder. Agents never run in a PTY and m4code never types into the shell.
- **Sessions, sidebars, and cards**:
  - Left sidebar of running and recent sessions (`Cmd/Ctrl+Z` reopens the last closed session), **Pick up a session here** to open terminal-started sessions in a folder, and background notifications when a session finishes or asks a question.
  - Right sidebar with drag-to-reorder cards for session status, subagents, GitHub issues and pull requests, and linked sessions, plus inline visual cards agents draw in chat.
  - Usage meters in the sidebar footer for connected harnesses with a limits adapter (Claude Code, Codex 5-hour/7-day limits read from `~/.codex`, and Antigravity model-group limits polled from `agy /usage` every five minutes at no token cost).
  - Configurable text generation model (**Settings → General**) for Regenerate title and Commit & Push—pick any enabled model or keep local summary.

### Cross-agent subagents

- **Shared MCP subagent tools**: Claude Code, Codex, and OpenCode sessions share `spawn_subagent`, `list_subagents`, `send_subagent_message`, `wait_subagent`, and `stop_subagent`. Children run in the parent's working folder without stealing focus, appear in the right sidebar, and fold their report back into the parent chat. Follow-ups reuse the child session, stopping a parent stops its children, and a read-only parent cannot grant edits.
- **Claude Code native subagents**: Claude Code can also start Codex, OpenCode, and (opt-in) Antigravity subagents directly. Each streams its work in its own task view with configurable effort (`max` maps to Codex's `xhigh`; Antigravity caps at the model's highest thinking level) and supports live or queued follow-ups.
- **Edit permissions**: With **Allow edits** enabled, a subagent acts without Claude Code's permission rules. Read-only Antigravity runs in an isolated folder under m4code's data directory with a hook that permits only reading and searching (your personal Antigravity rules, skills, and MCP servers do not load there).

### Skills

**Settings → Skills** browses `skills.sh`, installs single `SKILL.md` packages or entire GitHub repositories (grouped by repository, with single-skill, per-repo, and Update All controls), and symlinks enabled skills globally or per project into each active harness. When scoped to a project, it also lists skills shipped in the repository (such as `.claude/skills`) read-only.

## What it touches

- **Local files and settings**: m4code never calls the Anthropic API directly, never deletes a transcript, never logs terminal contents, and changes Claude Code's settings only when you flip a switch.
- **Git refs**: In a git repository, each session keeps its 20 newest working-tree snapshots as commits under `refs/m4code/`. They never touch your index, branches, or stash, though `git log --all` shows them.
- **Network**: GitHub (update checks, the GitHub pane, and installing skills), `skills.sh` (browsing or searching skills), npm (the Claude Agent SDK and Codex ACP adapter), OpenAI (Codex sessions and subagents), your configured OpenCode providers (OpenCode sessions and subagents), and Google (Antigravity sessions and subagents).

## Development

Prerequisites: stable Rust, Visual Studio Build Tools on Windows (or the system headers GPUI builds against on Linux), and [just](https://just.systems).

```sh
just run    # run the app
just test   # app, bridge, and mod tests
just ci     # build, tests, and web build
```

Repository layout:

- `src/` — the Rust + GPUI desktop app
- `vendor/gpui-base/` — patched copy of the editor crate (`Cargo.toml` documents what the patch changes and when to drop it)
- `mod/` — the Claude Code plugin loaded by each session
- `bridge/` — the Node script that runs Claude Code sessions through the Claude Agent SDK
- `web/` — the project website

See [AGENTS.md](AGENTS.md) for the code layout and commands. Debug builds store data in a separate folder so they can run alongside an installed release; launching a second instance against the same data folder reports that m4code is already running and exits.

To release, bump `version` in `Cargo.toml` and push a `vX.Y.Z` tag.
