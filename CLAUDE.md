# CLAUDE.md — Claudiu

The code is the source of truth. This file holds only rules.

## Boundaries

- Claudiu is a native Rust + GPUI desktop app that manages sessions around the user's existing `claude` CLI. Claude Code owns the agent experience; Claudiu owns the workspace.
- Never call the Anthropic API. Never implement or embed authentication, API keys or subscription login. Never impersonate Claude Desktop.
- Never type into a PTY on the user's behalf.
- Never run `claude -p` from Claudiu. Background one-shot text goes through the running session's mod.
- Never edit the main account's `~/.claude`. Alt accounts are separate folders passed with `CLAUDE_CONFIG_DIR`.
- Never write into Claude Code's private data or depend on undocumented transcript (JSONL) schemas. Prefer documented CLI interfaces and documented mod hooks.
- Do not edit Claude Code's settings files. A feature may ask Claude Code to change its own settings (such as `/model`) through the mod.
- Claude Code's own TUI is the only conversation view. Do not recreate its UI or protocol.
- Do not intercept Claude Code shortcuts. Reserve only explicit, documented Claudiu-global shortcuts.
- Never delete Claude Code session data as a side effect. Hiding a Claudiu entry must not delete a transcript.
- Nothing leaves the machine by default: no transcripts, project files, source, credentials, environment variables or API keys go to any Claudiu server.
- Do not log terminal contents. Persist environment variables only on explicit user opt-in.
- Use public crates and APIs; do not copy Zed source. Respect licenses.

## Platforms and architecture

- Windows and macOS are the primary targets. Isolate platform code behind small abstractions; the core model makes no Windows-only assumptions.
- Use `alacritty_terminal` as the terminal core unless a clearly superior compatible option is demonstrated.
- Each Claude session has exactly one PTY. Switching sessions never terminates, suspends or alters another session's PTY.
- Keep terminal emulation, process management, persistence, Claude discovery and GPUI rendering in separate modules. Persistence stays independent of GPUI.
- Never block the GPUI thread with filesystem scans or process I/O. Use background tasks and send compact state updates.
- Use platform application-data directories through a crate. Never hardcode paths, separators or home directories. Store Claudiu metadata apart from Claude Code data.
- Favor simple, debuggable code over premature abstraction. Do not over-engineer persistence before the session lifecycle is proven.

## Sessions and projects

- A session is a persistent record of a Claude Code process. A shell belongs to a Claude session as a panel, never as its own sidebar entry.
- Exited sessions stay in history.
- Startup reconciliation never deletes a record because an external record is unreadable. Mark it stale or unknown instead.
- Projects are Git-first. A plain folder is supported but is not the primary abstraction.
- Do not scan the filesystem for repositories without an explicit user choice.
- Resume through documented Claude Code CLI mechanisms. Do not reconstruct conversations.
- A click on a session must never restart a long-running process.

## Terminal correctness (critical)

Claude Code must behave as in a capable terminal, not a line-oriented shell. Test at least: Alt+P, Shift+Tab, Ctrl+C/D/Z, arrow keys, Home/End, modified keys, mouse, selection, paste, multiline input, resize, Unicode and emoji, ANSI colors, hyperlinks, alternate screen, scrollback, title changes, bell and image paste.

## Process lifecycle

- Handle spawn errors, PTY initialization, resize, stdin, output, exit, graceful and forced termination, shutdown and orphaned children.
- Closing the window must not silently kill Claude sessions.
- Sessions that were running come back at the next start. Sessions the user closed do not.

## Usage and limits

- Never invent a value. Show a neutral unavailable or stale state instead.
- Do not present limits as exact when Claude Code does not expose them.
- Do not build a separate Anthropic subscription API integration to obtain them.
- Account-wide meters must not change when the user switches sessions.

## UI

- Dark-first, minimal, dense without feeling cramped, keyboard-friendly, subtle in animation. Native, not a web wrapper.
- Use the Claude Desktop / Clawd icon as the application identity, never a generic terminal icon.
- Do not overuse cards, gradients, large rounded rectangles or decoration.
- Conveniences support the core product. Do not turn Claudiu into a generic IDE.
- Keep the Visual panel's `visual::ui::PROMPT`, `default_css` and `Css::set` in step.
- Every icon used must be listed in `icon_assets!`; a test enforces it.
