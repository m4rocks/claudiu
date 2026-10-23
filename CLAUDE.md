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

## The session list

The left sidebar (284 px by default, 220–520 px by dragging its right edge; `layout.left_w` remembers it) is **one list** of Claude Code sessions (product owner decision: no tabs, no project tree; chats were removed, see "Visual panel"). **Running** comes first: terminals in the order the user arranged them (drag to reorder). **Recent** follows, newest first; past the first 12 the rest sit behind a collapsed **Older**. Each row carries a project chip (the project's name, or the folder for an unregistered one).

```text
RUNNING
● Claude       pilotist
● Claude       giolt
RECENT
○ Claude       website
○ Claude       naming ideas
▸ OLDER  23
```

**Projects are a filter, not a tree.** A chip under the search box lists the projects (and "Add project…"); picking one narrows the list to its sessions, and `layout.project_filter` remembers it. With a project chosen, a ⋯ button opens its actions: new session here, GitHub, open in VS Code or Zed, reveal, copy path, remove. "+ New" starts in the filtered project. Sessions can no longer be dragged onto a project; a session's project is the one its folder belongs to.

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

A session is a persistent record representing a Claude Code CLI process. (A shell is not a session of its own: each Claude session can show a shell under its terminal, see "Shell panel". The only other terminal tabs are short-lived sign-in flows, `claude auth login` and `gh auth login`.)

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

**Usage and status through the per-process mod (product owner decision, replaces the status-line helper):** to read account usage (5-hour / 7-day), exact context and a few settings without spending tokens, the per-process mod (`mod/`, below) reports from inside Claude Code: on `session.measure`, `config.set` and `command.run` it writes one small JSON file per Claude session into `CLAUDIU_USAGE_DIR` (`usage.rs` reads them). The snapshot holds only limits, context figures, the model, the `/config` auto-compact switch, the token count auto-compaction runs at (`compact_at`, from the local, tokenless `$.session.usage({breakdown:'summary'})`, absent while auto-compact is off), the `advisorModel` setting, and the session id. It never blocks or fails Claude Code and writes only when something changed. There is no `--settings` file and no `statusLine` any more, so the user's own status line is untouched. Claudiu never runs the setting forms of `/model`, `/effort` or `/advisor` to test, since they change the user's saved settings.

**Claudiu MCP server:** each Claude session is started with a per-process `--mcp-config <file>` pointing at Claudiu's own executable in a hidden helper mode (`--mcp-title`), plus `--allowedTools` for its tools (`set_tab_title`, `attach_github`, `show_visual` and the session-reaching ones below) so none prompts. A second tool, `set_model`, is declared by that server but answered in-process by a per-process mod (`mod/`, written to the data dir at launch and loaded with `--plugin-dir`): its `turn.step` hook overrides model/effort from the next request, even mid-turn, with no permission prompt and nothing typed into the PTY; once Claude Code is idle the mod also runs `/model` and `/effort` through `$.command.run` so the session's own model (what `/model` shows) changes, which Claude Code saves as the user's default. If the mod isn't active the tool just reports an error. The same mod applies Claudiu's tab title as Claude Code's own session name: Claudiu writes the wanted name to a file in its data dir (`CLAUDIU_NAME_FILE`) and the mod runs `/rename` through `$.command.run`, which Claude Code queues until idle. Nothing is ever typed into the PTY on the user's behalf. IDE integration is switched off per process through the `CLAUDE_CODE_AUTO_CONNECT_IDE=false` and `CLAUDE_CODE_IDE_SKIP_AUTO_INSTALL=1` environment variables. Nothing is written to the user's Claude Code config. Claudiu never runs a `claude -p` of its own (product owner decision, so a change to how headless and third-party use is billed cannot touch it): the one-shot text it writes in the background (the Commit & Push message and "Regenerate title") goes through the running session's mod, below.

**Alt accounts (product owner decision):** the main account is the user's normal Claude Code (`~/.claude`, no `CLAUDE_CONFIG_DIR`) and Claudiu never writes to it. Each alt account is a Claudiu-created folder under its data dir (`accounts/<id>`), passed per process as `CLAUDE_CONFIG_DIR`. It keeps only its own login (`.credentials.json`, `.claude.json`). Every other entry is linked back to `~/.claude` (junctions on Windows, symlinks elsewhere), so the alt shares settings, history and so on, and Claude Code writes made in an alt land in the main folder. Sign-in runs `claude auth login`, and email/plan come from `claude auth status --json`. Removing an alt unlinks the links and deletes only that folder. Without Windows Developer Mode, files are hard-linked. When Claude Code replaces one, the next launch rejoins it and the newer copy wins: an alt's change is copied into main after backing up main's old copy in the alt's `backups/`. The account list lives in `accounts.json`, not `state.json`, so an older Claudiu that is still running can't erase it. Alt processes also get `CLAUDE_SECURESTORAGE_CONFIG_DIR`, which keys the macOS Keychain entry to each alt.

**Chats were removed (product owner decision):** they were a second way to run Claude Code (a native conversation view through the Agent Client Protocol and Zed's Node adapter) and their one distinguishing feature was generative UI. Everything now lives in the Claude Code TUI: the model draws Markdown and generative UI in the Visual panel of a code session. Claudiu starts no adapter and installs nothing into `<data>/acp`. Files an older Claudiu left in the data folder (`chats.json`, `acp/`, `chat-cache/`, `chat-picks.json`, `chats/`) are ignored and never read or deleted.

**Visual panel (product owner decision):** the model can draw beside the terminal. The per-process MCP server declares a `show_visual(title?, content)` tool (deferred behind ToolSearch, so the long dialect description costs no tokens until the model loads it; its description is `visual::ui::PROMPT`) that the per-process mod answers in-process, with no prompt and nothing typed into the PTY. `content` is Markdown, and a fenced block with the info string `ui` is written as HTML with inline CSS and drawn natively in Claudiu's theme (`visual/ui/`: `parse.rs` reads a forgiving HTML/CSS subset, tolerant of half-streamed input, `render.rs` draws it with GPUI elements). It is a general canvas, not a widget catalog: flex/grid layout, box styling, text styling, inline `<svg>` (rasterised once by GPUI), theme colours as `var(--accent)` etc. It is static: no scripts, no external images, no buttons; links open only on http(s). Keep `PROMPT`, `default_css` and `Css::set` in step.

**Streaming (product owner decision):** while the model is still writing a `show_visual` call, the mod's `turn.step` hook reads the partial JSON of its arguments and writes the Markdown so far (at most about six times a second) to `CLAUDIU_VISUAL_FILE` (`<data>/visual/<record id>.json`: `{call, title, content, done, at}`); the `tool.call` hook writes the finished card with `done: true`. `app/visuals.rs` polls that folder every 200 ms off the UI thread (`visual::read_changed`, keyed by modified time and size; what existed at start-up belongs to an earlier run and is not shown) and keeps a stack of cards per session (`Workspace.visuals`, 20 at most, a card is replaced by the same `call`, and the cards go with the session). A new card for the shown session opens the panel without moving the keyboard; the panel is a third view of the right sidebar (Menu, GitHub, **Visual**: newest card first, "drawing…" while a card is unfinished, the arrow returns to the menu, × forgets the cards) and shares GitHub's wider, resizable width (`layout.right_gh_w`). The menu lists "Visual · n" while there are cards. GitHub takes precedence over Visual. Icons are Lucide, embedded through `icon_assets!` in `main.rs` (a test fails if one is used but not listed); only the brand mark is drawn by hand.

**Commit message and title through the mod (product owner decision):** Claudiu asks the running session's mod for one-shot text. A request `{id, model: "haiku", system, prompt, deadline}` goes to `CLAUDIU_ASK_FILE` (`<data>/ask/<record id>.json`, `ask.rs`); the mod (`$.clock.every`, twice a second) answers next to it in `<name>.res.json`: `working` as soon as it picked the request up (so a session without the mod fails after 8 s, not after minutes), then `done` with the text or `error`; it makes the call with `$.model.complete` on the session's own client and skips requests whose deadline has passed. Commit & Push (`commit::commit_and_push` takes the completion as a closure) and "Regenerate title" (`app/titles.rs`, from the text parts of the transcript file) therefore need a **running Claude session**; for an ended one the row is dimmed or a toast says to start it. How `$.model.complete` is billed is Claude Code's own business, as it is for the session's turns.

**Sessions reaching each other (product owner decision):** the same per-process MCP server (`--mcp-title`) offers five tools to Claude Code sessions: `list_code_sessions`, `read_code_session`, `search_sessions` (titles, projects and folders only), `search_projects` (name, folder, branch) and `start_code_session`. The helper is a separate process, so it files a request under `<data>/bridge/req` and waits for `bridge/res` (`bridge.rs`); Claudiu answers each request on its next tick with no confirmation dialog (product owner decision; the first message is at most 2,000 characters, longer is refused). A request older than the helper's 5-minute patience is dropped. Code sessions identify themselves by Claudiu's own id (`CLAUDIU_RECORD`), which survives `/clear`. The tools are pre-allowed with `--allowedTools` so Claude Code adds no prompt either; their descriptions tell the model to call them only when the user asked. Reading is limited to what is live: the tail of a running terminal's screen and scrollback; Claude's transcript files are not read. A prompt for a new session is passed as the `claude` positional argument, with a leading `-` neutralised. A session started that way from another session is opened in that session's folder when none is given.

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

RUNNING
  Claude / project
  Claude / project

RECENT
  Claude / project

OLDER

────────────────

ACCOUNT
  5 HOUR
  ███████░░░ 72%
  resets in 2h 14m

  7 DAY
  ████░░░░░░ 34%
  resets in 4d 8h
```

The **context meter (and the prompt-cache timer) sit in the right sidebar, see "Right sidebar", and change when the selected session changes**.

The **5-hour and 7-day meters are account-wide** and should not change simply because the user switches tabs.

The account-wide section should remain visually stable while the session meters follow the selected Claude session.

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

# Shell panel

The main area shows one session at a time. There are **no splits** (product owner decision: they were cumbersome).

Each Claude session can show a **shell under its terminal**, toggled with Ctrl+Shift+T or Ctrl+J (Cmd+Shift+T or Cmd+J on macOS; Ctrl+J therefore no longer reaches Claude Code on Windows, Shift+Enter still breaks a line), the top bar's terminal button or the session's right-click menu. Ctrl+Shift+] moves the keyboard between Claude and the shell.

```text
┌────────────────────────────────────────────────┐
│ Claude                                         │
│ project-a                                      │
├────────────────────────────────────────────────┤  <- drag to resize
│ shell, in the session's folder                 │
└────────────────────────────────────────────────┘
```

- The shell is started the first time it is shown, in the session's folder, with the user's shell. It is a separate PTY, never listed in the sidebar and never persisted.
- It keeps running while another session is shown, remembers its height for as long as the session lives, and ends with the session (closing it, or the session leaving the running list). If the shell itself exits, the panel closes and the next toggle starts a fresh one.
- The shell is not a special Claude integration; it is a plain terminal.

**Scale (product owner decision):** Settings has a Scale (80–160 %, `settings.scale`, `scale.rs`). GPUI has no global zoom, so it multiplies every `text_size` (through `crate::scale::px`) and the terminal font; paddings, row heights and gpui-kit widgets keep their size.

# Right sidebar

A column on the right, under the top bar (so the window buttons stay in the corner), on by default. Toggle it with the top bar's panel button or Ctrl+Shift+R (Cmd+Shift+R); `layout.right_hidden` remembers it. It has three views, switched inside the same column:

- **Menu** (272 px by default, `layout.right_menu_w`, 240–480 px by dragging its left edge): the actions that used to crowd the top bar (Open in editor ▾, Open folder, Commit & Push, Pull, show or hide the shell, GitHub), the **issues and pull requests Claude attached** to the session, the sessions **linked** with it, and the session's **status**: context, prompt-cache timer, auto-compact and advisor. Auto-compact (On/Off) and Advisor (Off, Opus, Sonnet, Other… with a name box) are dropdowns while the session runs. The 5 HOUR / 7 DAY account meters stay in the left sidebar.
- **GitHub**: the project's issues and pull requests (`github/view.rs`), in a wider column (`layout.right_gh_w`, 560 px by default, resizable by dragging its left edge). The arrow returns to the menu. The view follows the shown session to that session's project; it is never a session, a tab or a main-area page.
- **Visual**: what the model drew for the shown session (see "Visual panel"), in the same wider column; it opens by itself when a card arrives.

Status comes from the per-process mod (`usage.rs` snapshots, no tokens): it reports context, limits, the `/config` auto-compact switch, the auto-compact threshold, `advisorModel` and the advisor **declared for the current conversation** (`ClaudeMeta.advisor` / `advisor_active`). Nothing is typed into the PTY.

**Context bar scale (product owner decision):** with auto-compact on and a threshold known, the CONTEXT bar is full at that threshold ("28k of 167k tokens"), so it shows how close compaction is; with auto-compact off it is full at the model's context window (`claude::context_limit`).

**Changing auto-compact and the advisor (product owner decision):** the dropdowns write `{seq, key, value}` commands (latest 8, numbers only go up) to `<data>/ctl/<record id>.json` (`usage::send_ctl`, path in `CLAUDIU_CTL_FILE`). The mod looks every second and applies each once: `$.config.set` for `autoCompact`, `/advisor <model>` through `$.command.run` (names limited to letters, digits, `. _ -`). Claude Code saves both as the user's defaults, like typing them. Commands sent before the process started are not replayed. Auto-compact applies immediately. So does turning the advisor off or on; changing the advisor **model** does not: the running conversation keeps the advisor tool it already declared until `/clear` or `/compact`, so the sidebar shows `active → saved` while a change is pending. The menu ticks the saved setting. The sidebar reflects the change as soon as the mod has applied it (a second or two).

**Attaching items (product owner decision):** the model, not Claudiu, decides what belongs to a session. The MCP server's `attach_github(kind, number, title?)` (pre-allowed, no focus change) appends a line to `<data>/attach/<record id>.jsonl`; Claudiu ingests and deletes the file on its tick and keeps the items in `ClaudeMeta.attached` (deduplicated, 20 at most, dismissible with ×). Clicking one opens it in the GitHub view. The transcript is not scanned for issue numbers. Claude Code defers MCP tools behind ToolSearch, so the server marks these bookkeeping tools (`attach_github`, `detach_github`, `link_session`, `unlink_session`, `set_tab_title`) with `_meta: {"anthropic/alwaysLoad": true}` and the model sees their schema from the start. The tool's description and the server instructions name the triggers (an issue mentioned, read through `gh`, a PR opened) and say to load its schema with ToolSearch if it is deferred.

**Linking sessions (product owner decision):** `link_session(id)` / `unlink_session(id)` (silent, pre-allowed) ask Claudiu to show two sessions together or stop (`detach_github(kind, number)` likewise takes an attached issue off); `search_sessions` finds the id. Links are kept on both sides (`ClaudeMeta.linked`), listed under LINKED in the right sidebar (click to switch, × to unlink), and made automatically when one session starts another with `start_code_session`.

---

# Starting a session

While `claude` starts (`TerminalView::loading`), a skeleton of its prompt box (`skeleton.rs`) covers the terminal with what is being loaded and the elapsed time (product owner decision). The user can type into it (Enter queues the message, Shift+Enter breaks the line, Esc takes a queued message back); nothing reaches the PTY meanwhile. It is laid out like Claude Code's start screen (rules, `❯`, the terminal's font and grid, at the bottom where Claude Code keeps its prompt). When Claude Code first draws, the draft is typed into it, but the skeleton stays until the text shows on the real screen (its first frame can come before it reads the keyboard); anything typed meanwhile follows, and Enter after that if the message was queued. Images pasted meanwhile (or dropped image files) show as `[Image #n]` and are kept under `<data>/pasted/` (pruned after a week). At hand-over each goes onto the clipboard in turn (a bitmap on Windows, where Claude Code reads `Clipboard.GetImage`), Claudiu presses Claude Code's image-paste key and waits for its chip to appear before the next, then puts the user's own clipboard back; so several images work.

To see where the start-up seconds go, set `CLAUDIU_TRACE_START=1`: each step is appended to `<data>/start-trace.log` (files prepared, child created, first output, first content, end of the start-up burst), and `CLAUDIU_NO_MCP=1` / `CLAUDIU_NO_MOD=1` start sessions without Claudiu's MCP server or mod so the difference shows what they cost. Both switches do nothing without the trace variable.

# Smooth scrolling

Mouse-wheel notches ease instead of jumping (product owner decision), in both sidebars (the Visual panel included) and the GitHub panes: `glide.rs` (`Smooth` wires a scrolling element, `Glide` is the easing; a trackpad's pixel steps are applied as they come). In the terminal the same easing moves the scrollback by fractions of a line (`TerminalView.frac`): the rows are drawn shifted, with the line above peeking in, clipped to the view. A program that takes the wheel (mouse reporting, or the alternate screen with alternate scroll) still gets whole-line events and no smoothing.

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

OS processes do not outlive Claudiu, but what was running does come back (product owner decision): the open Claude sessions are remembered in `running.json` (its own file, so another Claudiu sharing the data folder cannot erase it; kept current while running, written again at quit, and not touched until the saved list has been brought back) and resumed at the next start, after the first frame, then the one that was shown is shown again. A session the user closed, or that no longer exists, is not brought back.

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

The window follows the floating-panel layout of current VS Code: the left sidebar, the session/home area and the right sidebar are separate panels with a small gutter (`theme::PANEL_GAP`), rounded corners (`theme::PANEL_RADIUS`) and a faint outline. The window background is the *lighter* dark shade (`theme::APP_BG`) and the panels are the darker one (`theme::PANEL_BG`), so the gutter reads as the app behind them. The top bar is the exception: it spans the window and is drawn as background, not as a panel; it owns the drag area and the window buttons.

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
- shell panel
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

Claude Code's mod API (`session.measure`) exposes context and, where applicable, Claude.ai rate-limit information such as 5-hour and 7-day utilization/reset data; Claudiu's per-process mod reports them (see above). The status-line mechanism exposes the same figures but is no longer used. Claudiu should consume supported/exposed information.

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
- First sidebar section: **Running** (one list of sessions, projects are a filter)
- The Running section lists the running **Claude sessions**; a shell belongs to its session as a panel, not a list entry
- Projects are **Git-first**
- Each Claude session can show its own shell in a panel under its terminal
- One PTY per session
- Multiple sessions can run simultaneously
- Switching tabs/sessions does not kill other sessions
- Context meter is **per session**
- 5-hour limit is **account-wide**
- 7-day limit is **account-wide**
- **No splits** (product owner decision, replaces the earlier wish for them): the main area shows one session
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
6. Exact keyboard shortcuts.
7. ~~Whether application restart should offer to restore the previous UI layout automatically.~~ Decided: the sessions that were running come back at the next start (see "Process lifecycle").
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
