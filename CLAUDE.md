# CLAUDE.md — Claudiu

## Project

**Claudiu** is a native desktop application written in **Rust + GPUI** that provides a polished, Claude Desktop-inspired environment for running and organizing **Claude Code CLI** sessions.

The application is a terminal/session manager around the user's existing `claude` executable. It does **not** implement Claude API access itself and does **not** authenticate to Anthropic directly.

The UI should be dark/black-first and use the **Claude Desktop / Clawd-style icon** supplied for the application.

## Core principle

> Claudiu owns the workspace/session-management experience. Claude Code owns the agent experience.

Do not recreate Claude Code's UI, agent protocol, authentication, permissions system, tool protocol, or model API. Run the real `claude` CLI inside a real PTY and render its terminal UI faithfully.

The user should be able to use Claude Code exactly as they would in a terminal, including keyboard shortcuts such as Alt+P and other interactive behavior.

---

## Target platforms

Primary targets:

- Windows
- macOS

Keep platform-specific code isolated behind small abstractions so the architecture remains portable.

Do not make Windows-only assumptions in the core application model.

---

## Technology direction

### Required

- Rust
- GPUI
- `alacritty_terminal` as the terminal emulation core, unless a clearly superior compatible option is demonstrated
- Native PTY implementation for each platform
  - Windows: ConPTY
  - macOS: Unix PTY APIs

### Terminal architecture

Each running shell/Claude session gets **exactly one PTY**.

Conceptually:

```text
GPUI terminal view
       │
       ▼
alacritty_terminal
       │
       ▼
PTY abstraction
   ┌───┴────┐
Windows    macOS
 ConPTY     Unix PTY
   │         │
   ▼         ▼
shell / claude process
```

The terminal must support normal VT behavior, resizing, scrollback, selection/copy/paste, mouse input, alternate screen behavior, colors, Unicode, cursor state, and keyboard modifiers.

Do not intercept Claude Code shortcuts merely to implement application-level shortcuts. Reserve only explicit Claudiu-global shortcuts and document them.

---

# Product model

## Current Session

The sidebar's first section is **Current Session**.

It lists all currently running processes/sessions, not only Claude sessions.

Example:

```text
CURRENT SESSION

● Claude       pilotist
● Claude       giolt
$ PowerShell   pilotist
● Claude       website
```

A session remains associated with the running PTY until its process exits.

When a process exits, it must remain discoverable in project/session history rather than simply disappearing from the application.

Switching sessions does **not** terminate, suspend, or otherwise alter the other PTYs.

---

## Projects

A project is primarily a **Git project/repository**.

Projects may be imported/registered from local folders. Git metadata should be used where available to identify the repository and provide useful metadata.

Project records should support, where practical:

- display name
- filesystem path
- Git repository information
- current/default branch when available
- detected worktrees
- associated Claude Code sessions
- editor launch actions
- icon/avatar where useful
- project-specific preferences

Do not require every folder to be a Git repository for basic folder support, but the primary project abstraction is Git-aware.

### Project discovery

The app should be able to import a project folder through the UI.

Consider later supporting discovery of repositories under user-selected workspace roots, but do not scan the entire filesystem without an explicit user choice.

---

# Session model

A session is a persistent record representing either:

1. a Claude Code CLI process, or
2. a normal shell process.

Each live session has:

- unique local Claudiu session ID
- process ID when running
- PTY handle/state
- working directory
- associated project, if any
- session type (`claude` or `shell`)
- display title
- creation time
- last active time
- running/exited state

Claude sessions should additionally retain, where available:

- Claude session ID
- session name/title
- model
- context usage
- last known Claude Code metadata
- transcript/session path reference

Do not assume every field is available at every point in time.

---

# Claude Code integration

## Invocation

Claudiu must launch the user's existing **`claude` CLI executable**.

Do not call Anthropic's API directly.

Do not embed an API key flow.

Do not implement Claude.ai subscription authentication.

Do not impersonate Claude Desktop authentication.

Claude Code remains responsible for authentication and communication with Anthropic.

A basic new Claude session should be conceptually equivalent to:

```text
shell/PTY → claude
```

For an existing Claude session, use Claude Code's supported resume mechanisms rather than reconstructing the conversation ourselves.

## Claude Code configuration

Claudiu does not edit Claude Code's settings files itself. Per-process flags and environment variables are the preferred way to integrate, but this is no longer a hard rule (product owner decision): a feature may have Claude Code change its own settings, for example by typing `/model` or `/effort`, which Claude Code saves as the user's default.

**Per-process `--settings`:** to read account usage (5-hour / 7-day) and exact context without spending tokens, Claudiu may pass a *per-process* `--settings <file>` containing only a `statusLine` that points back at Claudiu's own executable in a hidden helper mode (`--statusline-tee`). That flag applies to a single session and writes nothing to the user's Claude Code config. The helper must forward to the user's own status line (if any) so their display is unchanged, store only a minimal snapshot (limits, context figures, session id), and never block or fail Claude Code.

**Claudiu MCP server:** each Claude session is started with a per-process `--mcp-config <file>` pointing at Claudiu's own executable in a hidden helper mode (`--mcp-title`), plus `--allowedTools mcp__claudiu__set_tab_title` so renaming never prompts. A second tool, `set_model`, is declared by that server but answered in-process by a per-process mod (`mod/`, written to the data dir at launch and loaded with `--plugin-dir`): its `turn.step` hook overrides model/effort from the next request, even mid-turn, with no permission prompt and nothing typed into the PTY; once Claude Code is idle the mod also runs `/model` and `/effort` through `$.command.run` so the session's own model (what `/model` shows) changes, which Claude Code saves as the user's default. If the mod isn't active the tool just reports an error. The same mod applies Claudiu's tab title as Claude Code's own session name: Claudiu writes the wanted name to a file in its data dir (`CLAUDIU_NAME_FILE`) and the mod runs `/rename` through `$.command.run`, which Claude Code queues until idle. Nothing is ever typed into the PTY on the user's behalf. IDE integration is switched off per process through the `CLAUDE_CODE_AUTO_CONNECT_IDE=false` and `CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL=1` environment variables. Nothing is written to the user's Claude Code config. The "Commit & Push" action runs a separate headless `claude -p` (Haiku, no tools, `--setting-sources ""`, no saved session) through the user's own CLI.

Read supported CLI output/interfaces where possible.

---

# Session import and synchronization

Claude Code's local session data is an external source of information that Claudiu can index/import.

## Startup reconciliation

Every time Claudiu launches:

1. Load Claudiu's own project/session database/index.
2. Discover relevant local Claude Code projects/session records.
3. Reconcile new/changed sessions into Claudiu's index.
4. Preserve Claudiu-only metadata that Claude Code does not provide.
5. Do not delete local Claudiu records merely because an external record temporarily cannot be read.
6. Mark stale/deleted/unknown external sessions appropriately instead of silently destroying history.

The goal is to make the sidebar feel immediately up to date after launch.

A later improvement may watch Claude Code's relevant filesystem locations while Claudiu is running and perform incremental reconciliation.

## Important data ownership rule

The Claude Code filesystem/session data is **read/import data** from Claudiu's perspective.

Claudiu should not write into Claude Code's private data structures unless a future, documented Claude Code API explicitly makes a write operation appropriate.

Do not depend on undocumented internal JSONL schemas for normal execution.

Session discovery may inspect local data where useful, but launching/resuming should prefer documented Claude Code CLI functionality.

---

# Session UX

## Selecting a session

Use a hybrid session interaction:

- clicking a session selects it and shows metadata/preview
- an explicit resume/open action starts or attaches to it
- double-click may be used as a convenience to resume

Do not make an accidental click unexpectedly restart a long-running process.

The exact interaction can be refined during implementation.

## Old sessions

Exited sessions remain available in the project/session history.

Users should be able to:

- search sessions
- see recent sessions
- resume a Claude Code session
- rename/display-name sessions where supported
- inspect basic metadata
- remove/hide an entry from Claudiu's index without deleting the underlying Claude Code transcript

Deleting a Claudiu entry must not silently delete Claude Code data.

---

# Sidebar

The sidebar is a major part of Claudiu's identity and should feel closer to the current Claude Desktop application than to Windows Terminal.

It should be dark/black, dense but readable, and visually calm.

Suggested structure:

```text
CLAUDIU

CURRENT SESSION
  Claude / project
  Claude / project
  $ PowerShell / project

PROJECTS
  Project A
    Session 1
    Session 2
  Project B
    Session 3

────────────────

ACCOUNT
  5 HOUR
  ███████░░░ 72%
  resets in 2h 14m

  7 DAY
  ████░░░░░░ 34%
  resets in 4d 8h

SESSION
  CONTEXT
  █████████░ 78%
```

The **context meter changes when the selected session changes**.

The **5-hour and 7-day meters are account-wide** and should not change simply because the user switches tabs.

The account-wide section should remain visually stable while the session context meter follows the selected Claude session.

---

# Usage and limits

The app should display three separate concepts:

### Context

Per Claude Code session.

Show a visual progress bar and percentage when available.

If data is unavailable, use a neutral unavailable state rather than inventing a value.

### 5-hour limit

Account-wide Claude.ai subscription usage/rate-limit information when exposed by Claude Code.

Show:

- utilization percentage when available
- reset time/countdown when available

### 7-day limit

Account-wide Claude.ai subscription usage/rate-limit information when exposed by Claude Code.

Show:

- utilization percentage when available
- reset time/countdown when available

Do not pretend these values are exact if Claude Code does not expose them.

Cache the last known account-level values locally so the UI can remain useful between runs, but clearly distinguish cached/stale data from current data if necessary.

Do not build a separate Anthropic subscription API integration merely to obtain these values.

---

# Editor integrations

Projects should have convenient buttons/actions to open the project in external editors.

At minimum investigate:

- Visual Studio Code
- Zed

The implementation should detect installed executables where practical instead of hard-coding one path.

The command concept should be equivalent to:

```text
code <project-path>
zed <project-path>
```

but use the platform-appropriate executable/discovery mechanism.

Architecture should allow additional editors later.

Possible future integrations:

- Cursor
- Windsurf
- other user-configured editors

---

# Splits

The application should support terminal splits if the GPUI architecture can accommodate them cleanly.

Example:

```text
┌───────────────────────┬───────────────────────┐
│ Claude                │ Claude                │
│ project-a             │ project-b             │
│                       │                       │
├───────────────────────┴───────────────────────┤
│ PowerShell                                     │
│ project-a                                     │
└────────────────────────────────────────────────┘
```

Each split remains backed by its own session/PTY.

Splits must not create a special Claude integration; they are simply a layout of independent terminal sessions.

---

# Cross-platform filesystem/data design

Use platform-appropriate application-data directories through a Rust abstraction/crate.

Do not assume:

- Windows `%APPDATA%` paths
- macOS `~/Library/...` paths
- Windows path separators
- a particular user's home directory

Store Claudiu's own metadata separately from Claude Code's own data.

A small embedded database or structured local store is appropriate. Keep the persistence layer independent of GPUI.

Potential entities:

```text
Project
Session
WorkspaceLayout
Editor
AppSettings
UsageSnapshot
```

Do not over-engineer the database before the session lifecycle is proven.

---

# Process lifecycle

Claude and shell processes may be long-running.

The process/session manager must handle:

- spawn
- startup errors
- PTY initialization
- resize
- stdin forwarding
- stdout/stderr/event processing
- process exit
- graceful termination
- forced termination where appropriate
- application shutdown
- orphaned child process cleanup

Closing the application should have an explicit, predictable policy for live sessions.

Do not silently kill every Claude session just because the main window closes if this can be avoided.

A future version may support restoring live sessions after application restart, but MVP can first guarantee reliable persistence/resume of Claude sessions rather than keeping OS processes alive across restarts.

---

# Terminal correctness

This is a critical requirement.

Claude Code must behave as if it were running in a capable terminal.

Test specifically:

- Alt+P
- Shift+Tab
- Ctrl+C / Ctrl+D / Ctrl+Z where applicable
- arrow keys
- Home/End
- modified cursor keys
- mouse input
- text selection
- paste
- multiline input
- terminal resize
- Unicode
- emoji where supported
- ANSI colors
- hyperlinks
- alternate screen
- scrollback
- cursor movement
- terminal title changes
- bell/events where applicable
- image/paste behavior where supported by Claude Code and the terminal stack

Do not assume a basic line-oriented shell is sufficient.

---

# Visual direction

Claudiu should be:

- black/dark-first
- minimal
- premium
- dense without feeling cramped
- keyboard-friendly
- subtle in animation
- inspired by Claude Desktop's current desktop UX
- unmistakably a native application rather than a web wrapper

The terminal should visually belong to Claudiu while still remaining a faithful terminal emulator.

Do not overuse cards, gradients, huge rounded rectangles, or excessive decoration.

The sidebar should prioritize information hierarchy and fast navigation.

Use the provided Claude Desktop/Clawd icon as the application identity; do not substitute a generic terminal icon.

---

# Convenience features

The product should include small quality-of-life features where they fit naturally, such as:

- open project in VS Code
- open project in Zed
- copy project path
- reveal project in Explorer/Finder
- open a shell in project
- open a new Claude session in project
- resume previous Claude session
- rename local display labels
- session search
- project search
- recent projects/sessions
- context/usage visibility
- sensible keyboard shortcuts
- drag/reorder where it materially improves the workflow
- session close/kill actions with appropriate confirmation

These should support the core product rather than turn Claudiu into a generic IDE.

---

# MVP priorities

Build in this order unless implementation evidence strongly suggests otherwise:

## Phase 1 — shell/PTY foundation

- GPUI desktop window
- black theme
- sidebar shell
- one terminal view
- one PTY
- spawn the platform default shell
- input/output
- resize
- scrolling/selection/copy/paste

## Phase 2 — Claude Code

- configurable discovery of `claude`
- launch `claude`
- preserve working directory
- verify Claude Code interactive behavior
- multiple independent Claude sessions
- session switching without killing other PTYs

## Phase 3 — projects

- Git-aware project registration
- project/session relationship
- project persistence
- current-session section
- exited session history
- import/reconciliation on startup

## Phase 4 — Claude session import/resume

- discover local Claude Code sessions
- map them to projects
- show imported sessions
- preview metadata
- resume through supported CLI mechanisms
- protect against duplicate/redundant entries

## Phase 5 — usage sidebar

- per-session context display
- account-wide 5-hour display
- account-wide 7-day display
- reset countdowns
- unavailable/stale states

## Phase 6 — productivity features

- VS Code integration
- Zed integration
- project reveal/open-folder actions
- session search
- splits
- keyboard shortcuts
- layout persistence

## Phase 7 — polish

- crash/error handling
- startup performance
- process cleanup
- macOS parity
- Windows parity
- accessibility basics
- packaging/distribution

---

# Engineering rules

1. Keep terminal emulation, process management, persistence, Claude discovery, and GPUI rendering as separate modules.
2. Keep platform-specific code isolated.
3. Prefer documented Claude Code interfaces over undocumented internal formats.
4. Never make direct Anthropic API calls.
5. Never delete Claude Code session data as a side effect of Claudiu actions.
6. Treat imported external state as reconcileable/stale, not as Claudiu-owned truth.
7. Avoid blocking the GPUI UI thread with filesystem scans or process I/O.
8. Use async/background tasks where appropriate and send compact state updates back to GPUI entities.
9. Write tests around session reconciliation and process lifecycle before adding lots of UI polish.
10. Favor simple, debuggable code over premature abstraction.
11. Do not copy Zed source code merely because Zed and GPUI are related; use compatible public crates/APIs and respect their licenses.

---

# Important security/privacy expectations

Claudiu is a local desktop utility around the user's existing Claude Code installation.

It should not upload:

- Claude session transcripts
- project files
- source code
- credentials
- environment variables
- API keys

to any Claudiu-operated server by default.

The initial architecture should be entirely local.

Do not log terminal contents by default.

Do not persist environment variables unless a feature explicitly requires it and the user knowingly opts into it.

---

# Research notes / current technical assumptions

These are implementation assumptions verified during initial research and should be re-checked against current upstream documentation before finalizing the implementation.

### GPUI

Current Zed/GPUI sources contain platform implementations for both Windows and macOS. Windows uses Win32/DirectWrite, while macOS uses Metal. GPUI is Apache-2.0.

Reference:
https://github.com/zed-industries/zed/tree/main/crates/gpui

### alacritty_terminal

Current `alacritty_terminal` provides the terminal grid, VT parsing/terminal state, PTY abstractions, selection, search, terminal events, and resize-related APIs. The current crate supports Windows and macOS; Windows terminal operation relies on ConPTY support in the Alacritty ecosystem.

References:
https://docs.rs/alacritty_terminal/latest/alacritty_terminal/
https://docs.rs/alacritty_terminal/latest/alacritty_terminal/tty/

### Claude Code sessions

Claude Code provides supported mechanisms for resuming sessions. Local session/transcript data can be discovered by project. Do not make undocumented transcript formats the main execution API.

Reference:
https://code.claude.com/docs/en/sessions

### Claude Code status / usage information

Claude Code's current status-line mechanism exposes context and, where applicable, Claude.ai rate-limit information such as 5-hour and 7-day utilization/reset data. Claudiu should consume supported/exposed information.

Reference:
https://code.claude.com/docs/en/statusline

---

# Decisions already made with the product owner

- Product name: **Claudiu**
- Application icon: **Claude Desktop / Clawd icon**
- Language: **Rust**
- UI framework: **GPUI**
- Theme: **black/dark-first**
- Sidebar: **left-side, Claude Desktop-inspired**
- First sidebar section: **Current Session**
- Current Session contains both **Claude sessions and normal shells**
- Projects are **Git-first**
- Normal shell sessions are supported
- One PTY per session
- Multiple sessions can run simultaneously
- Switching tabs/sessions does not kill other sessions
- Context meter is **per session**
- 5-hour limit is **account-wide**
- 7-day limit is **account-wide**
- Splits are desired if technically practical
- Projects have editor launchers, including VS Code and Zed
- Startup should reconcile/import Claude Code sessions so the sidebar stays current
- Claudiu does **not** use the user's Claude subscription directly
- Claudiu launches the user's normal **`claude` CLI**
- The product should be portable across **Windows and macOS**

---

# Open questions for implementation/design review

These are intentionally left for the implementation/specification round rather than being silently decided:

1. Exact persistent storage technology (SQLite vs another embedded store).
2. Exact Claude Code session discovery strategy on each platform.
3. How much metadata to import from Claude Code session records.
4. Whether to add a background filesystem watcher in v1 or keep startup reconciliation only.
5. Exact sidebar density, icons, typography, and spacing.
6. Exact split UX and keyboard shortcuts.
7. Whether application restart should offer to restore the previous UI layout automatically.
8. How to detect installed editors robustly on Windows and macOS.
9. Whether Claudiu should support user-configured alternate shell profiles in addition to the platform default shell.
10. Packaging/signing/update strategy for Windows and macOS.

Do not resolve these by adding arbitrary complexity. Prefer the smallest implementation that satisfies the core UX and keeps future choices open.

---

# First implementation instruction

Before building the full UI, create a minimal vertical slice proving these four things together:

```text
GPUI window
   ↓
terminal renderer
   ↓
PTY
   ↓
claude CLI
```

Prove that real Claude Code interactive input/output works correctly, including Alt+P and terminal resizing, on the target platform.

Once that works, build the session manager and sidebar around the proven PTY/terminal component rather than designing the UI around a fake terminal abstraction.

The eventual goal is a polished, fast, native **Claudiu** desktop application that feels like a dedicated Claude Code workstation while remaining a thin, non-invasive wrapper around the real Claude Code CLI.
