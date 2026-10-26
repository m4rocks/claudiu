# Claudiu

A native desktop app (Rust + [GPUI](https://gpui.rs)) for running several [Claude Code](https://code.claude.com) sessions in one window, on Windows and macOS.

> **Claudiu is an independent project. It is not affiliated with, endorsed by or sponsored by Anthropic.** Claude and Claude Code are trademarks of Anthropic.

It's a personal project with rough edges; issues and PRs are welcome.

## How it works

- **Your own `claude`, in a real terminal.** Each session is one `claude` process in its own pseudo-terminal (ConPTY on Windows, a Unix PTY on macOS), drawn with `alacritty_terminal`. Claude Code sees a normal terminal, so Alt+P, Shift+Tab, mouse, paste and the rest go straight to it. Claudiu types into the terminal in one case: in editor mode, the `@file#L` reference of the selected lines, which it takes out again if you leave without typing.
- **No API, no login.** Claudiu never calls the Anthropic API and has no API keys or sign-in of its own. Authentication stays with Claude Code.
- **Sessions are records.** A session is Claudiu's record of a Claude process. Ended sessions stay in the sidebar and resume through `claude --resume`. Switching sessions never touches another session's process. Sessions that were running come back at the next start.
- **Projects are Git-first.** Add a folder with the button, by dropping it on the window, or with `claudiu <folder>`. Claudiu never scans your disk for repositories.
- **Documented hooks only.** Each session is started with `--session-id` or `--resume`, plus a per-process `--plugin-dir` mod and a `--mcp-config` file (tab titles). They live only as long as the session.
- **Usage meters.** The mod writes a small snapshot after each response: 5-hour and 7-day limits, context, auto-compact and advisor settings. Where Claude Code offers background compaction ("Precompute compaction" in `/config`, still behind a feature flag on many accounts), the inspector shows a switch for it too. The auto-compact, background compaction and advisor switches apply when that session next starts or resumes: Claudiu passes them in the session's own `--settings` file, so a running session keeps its value (the inspector shows `On → Off` while a change waits) and Claude Code's settings are not written. No extra tokens are spent, and your own status line is untouched. Limits show only for sessions started from Claudiu, and stale values are labelled with their age. For sessions resumed elsewhere, context is estimated from the transcript and shown with `~`.
- **Claude in Chrome.** Switch it per session in the inspector, which restarts the session with `--chrome`. Settings → Sessions → Claude in Chrome for new sessions sets the default.
- **New-session defaults.** Settings → Sessions sets the model, effort and permission mode that new Claude sessions start with (empty means Claude Code's own default). They are passed as `--model`, `--effort` and `--permission-mode` to every new session, including ones another session or the GitHub pane starts; a flag the caller already passes wins, resumed sessions get none, and Fix Git with Claude never starts in plan mode. `bypassPermissions` is on the list and skips every prompt in every new session, so choose it with care.
- **Claudiu's own tools.** Claudiu gives each Claude session a small MCP server (tab titles, the Visual panel, the GitHub sidebar, tools that reach your other sessions, model switching). Settings → Claudiu tools switches the Visual panel, GitHub sidebar, Other sessions and Model switch off one by one: a feature that is off loses its tools and the instruction text about them, in new and resumed sessions (a resumed one may keep the old instructions until its next compaction). Tab titles always stay.
- **Trimmed Claude Code tools.** Claude Code sends every tool definition with every request. Settings → Claude Code tools switches off whole features (bundled skills, workflows, remote control, claude.ai connectors, artifacts) and single tools (plan mode, AskUserQuestion, scheduled tasks, SendMessage and more) for the sessions Claudiu starts, through a per-session `--settings` file; your own Claude Code settings are not edited, and nothing is trimmed by default. The list follows [How to kill the bloat in Claude Code's system prompt](https://www.aihero.dev/how-to-kill-the-bloat-in-claude-codes-system-prompt).
- **Alt accounts.** An extra account is a separate Claude Code config folder (`CLAUDE_CONFIG_DIR`) holding only its own login. Settings, `CLAUDE.md` and keybindings are copied from your main `~/.claude` before each launch, one way, so a change made in an account is replaced at its next start; folders (agents, skills, plugins, session history) link back to `~/.claude`. New sessions start under the active account; a session keeps its account after you close it or restart Claudiu, until you hand it off to the active one from the right sidebar. Only sign in to accounts you own.
- **GitHub, Git and editors.** Issues, PRs, Commit & Push (greyed out when there is nothing to commit or push) and Pull go through the `gh` and `git` you already have; the GitHub pane also calls the GitHub API directly with the token `gh` holds. Open a project in VS Code or Zed.
- **Editor mode.** Ctrl+Shift+E puts the session's files and a light editor beside its terminal: tabs, quick open, find in files, Git change marks (a bar over the line numbers: green for added lines, blue for changed ones, a red tick where lines were removed, as of the last save), and highlighting for common languages. Diagnostics, hover and go-to-definition come from language servers already on your PATH: `rust-analyzer` (Rust), TypeScript 7's `tsc --lsp` (TypeScript and JavaScript) and `gleam` (Gleam). A missing one is named under the editor; Claudiu installs nothing. TypeScript's comes with `npm install --global typescript`, and with rustup, run `rustup component add rust-analyzer`. Select lines in the editor and click into Claude's terminal to put `@file#L12-30: ` into Claude's input, then type your comment after it (start a new line yourself before the next one); leave without typing and it's taken back out. Files with tab-indented lines show every tab as spaces and save untouched lines as they were (a line you edit keeps the tabs of its indent, not any inside it, and only in a file indented mostly with tabs). Right-click in the file tree to create a file or folder, rename one, copy its relative path, reveal it in File Explorer or Finder, or delete it (after a confirmation; it doesn't go to the trash). Right-click in the editor to ask Claude to explain, fix or write tests for the selection (or the clicked line), or to fix the problem a language server reports there: the request is sent to Claude as your next message once it is idle, without touching what you have typed in its prompt. Hold Ctrl over a file path in Claude's terminal, such as `src/main.rs:42`, and it is underlined; Ctrl+click opens it at that line (also while Claude Code is capturing the mouse). Quick open lists the files you showed last first.

## What Claudiu touches

- Claudiu never deletes a transcript, and hiding a session removes it from Claudiu's index only. Claudiu does not write to `~/.claude`. For an alt account it copies the config files over its own.
- Nothing is uploaded. Terminal contents are never logged. Claudiu contacts only GitHub: the update check against GitHub Releases, and the GitHub pane and attached issue titles through your `gh` login. Turn the update check off with `"settings": { "update_url": "" }` in `state.json`.
- Claudiu's own state is in `%LOCALAPPDATA%\m4rocks\Claudiu\data` on Windows and `~/Library/Application Support/Claudiu` on macOS. Crashes go to `crash.log` there.
- Children start with a clean environment: markers from other terminals and from nested Claude Code sessions are removed, IDE auto-connect is off, and `TERM_PROGRAM=Claudiu`.

## Shortcuts

Claudiu's own shortcuts use Ctrl+Shift (Cmd+Shift on macOS), plus a few Ctrl chords for the shell and session switching. Everything else reaches Claude Code.

| Shortcut | Action |
|---|---|
| Ctrl+Shift+N | New session, in the active session's folder |
| Ctrl+` / Ctrl+J | Show or hide the session's shell |
| Ctrl+Shift+W | End the focused session |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous running session |
| Ctrl+1 … Ctrl+9 | Show that running session; hold Ctrl a moment to see the numbers in the sidebar, and the other shortcuts beside their buttons |
| Ctrl+Shift+] | Move the keyboard between Claude and its shell |
| Ctrl+Shift+E | Editor mode: the session's files and an editor beside its terminal |
| Ctrl+P | Editor mode, over the files or editor: go to a file by name |
| Ctrl+S | Editor mode: save the shown file |
| F12 / Ctrl+click | Editor mode: go to definition (needs the language's server, see Editor mode above) |
| Ctrl+G | Editor mode: go to line (`42` or `42:7`) |
| Alt+← / Alt+→ | Editor mode: back to where you jumped from, and forward again |
| Ctrl+F / Ctrl+H | Editor mode: find, or find and replace, in the shown file |
| Ctrl+click a path | In Claude's terminal: open that file in editor mode, at its line |
| Ctrl+Shift+B / Ctrl+Shift+R | Toggle left / right sidebar |
| Ctrl+Shift+F | Search sessions and projects; in editor mode, find in the session's files |
| Ctrl+Shift+O | Add a project folder |
| Ctrl+Shift+G | Open GitHub |
| Ctrl+, | Open settings |

On macOS, `Ctrl+Shift` above is `Cmd+Shift`, `Ctrl+J` is `Cmd+J`, `Ctrl+1` … `Ctrl+9` are `Cmd+1` … `Cmd+9`, and `Ctrl+F` is `Cmd+F`. Back and forward are `Ctrl+-` and `Ctrl+Shift+-`. `Ctrl+`` and `Ctrl+G` stay Ctrl.

Copy with Ctrl+Shift+C (or Ctrl+C while text is selected); paste with Ctrl+V or Ctrl+Shift+V. On macOS use Cmd.

A middle click closes an editor tab, or ends a running session in the sidebar.

## Known gaps

- macOS is lightly tested, and the build isn't notarized, so Gatekeeper complains on first launch.
- Quitting ends running PTYs. Sessions stay resumable, but live processes don't survive a restart.
- Transcript scanning is a best-effort import aid; launching and resuming never depend on it.

## Building

Stable Rust. On Windows, also Visual Studio Build Tools (C++ workload) and the Windows SDK.

Common tasks go through [just](https://just.systems) (`winget install Casey.Just` on Windows, `brew install just` on macOS). Run `just` with no argument to list them.

```sh
just run          # dev build: shows stderr, never checks for updates
just build        # debug build
just test         # app tests and mod tests
just test-app     # Rust unit tests only
just test-mod     # mod tests (needs the claude CLI on PATH)
just lint         # clippy checks
just web          # web site dev server (installs dependencies first)
just web-build    # build the web site
just ci           # what CI checks, plus the mod and the web build
```

The web recipes use [Bun](https://bun.sh), matching `web/bun.lock`. For a release build, run `cargo build --release`.

Releases use [Velopack](https://velopack.io) and GitHub Releases: bump `version` in `Cargo.toml`, then `git tag vX.Y.Z && git push --tags`; `.github/workflows/release.yml` builds and publishes. Local installer: `scripts\package.ps1`.
