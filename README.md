# Claudiu

A native desktop workspace for [Claude Code](https://code.claude.com) sessions, written in Rust with [GPUI](https://gpui.rs).

Claudiu does **not** reimplement Claude Code. It runs your real `claude` CLI inside a real pseudo terminal (ConPTY on Windows, Unix PTY on macOS) and renders it faithfully, so every Claude Code feature and shortcut (Alt+P, Shift+Tab, …) behaves exactly as in a normal terminal. Claudiu adds the workspace around it: sessions, projects, history, usage meters.

> Claudiu owns the workspace/session-management experience. Claude Code owns the agent experience.

## Features

- **Real terminal**: `alacritty_terminal` emulation, 24-bit color, scrollback, selection, mouse reporting, bracketed paste, alt screen, hyperlinks (Ctrl+click), pixel-exact block/box-drawing glyphs.
- **Current Session**: every running process (Claude and plain shells). Switching never touches the other PTYs.
- **Terminal sessions** exist only under CURRENT SESSION while running; they are never saved to project history. Claude sessions are saved and resumable.
- **Projects**: Git-aware (branch, linked worktrees). Add by button, drag-and-drop a folder, or `claudiu <folder>`.
- **History**: ended sessions stay in the sidebar. Clicking a past Claude session resumes it immediately via `claude --resume`.
- **Startup import**: your existing Claude Code sessions are discovered (read-only) and merged into the sidebar, without duplicates.
- **Splits**: up to four panes, each its own session.
- **Usage**: per-session context meter, 5-hour and 7-day account meters (see *Known limits*).
- **Editors**: open a project in VS Code or Zed (auto-detected), reveal in Explorer/Finder, copy path.
- **Auto-update**: installed copies update themselves from GitHub Releases (Velopack).

## Shortcuts

Claudiu-global shortcuts use **Ctrl+Shift** (Cmd+Shift on macOS) so plain Ctrl/Alt chords always reach Claude Code.

| Shortcut | Action |
|---|---|
| Ctrl+Shift+N | New Claude session (in the active session's folder) |
| Ctrl+Shift+T | New shell |
| Ctrl+Shift+D | Split: new shell beside the current pane |
| Ctrl+Shift+W | End the focused session immediately |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous running session |
| Ctrl+Shift+] | Focus next pane |
| Ctrl+Shift+B | Toggle sidebar |
| Ctrl+Shift+F | Search sessions and projects |
| Ctrl+Shift+O | Add project folder |

In the terminal: Ctrl+Shift+C / Ctrl+Insert copy, Ctrl+V / Ctrl+Shift+V / Shift+Insert paste, right-click copies a selection or pastes, Shift+PageUp/PageDown/Home/End scroll history. Ctrl+C copies **only while text is selected**; otherwise it is a normal interrupt. On macOS use Cmd+C / Cmd+V.

## What Claudiu touches

- **Claude Code's data is read-only.** Claudiu never writes to `~/.claude`, and never edits `settings.json`, keybindings or any Claude Code config. The one thing it adds is a per-process `--settings` status line for usage data (see *Known limits*), which lives only for that session. Hiding a session only removes it from Claudiu's own index.
- Launch/resume use documented CLI flags only (`--session-id`, `--resume`, `--settings`).
- Claudiu's own state lives in `%LOCALAPPDATA%\Claudiu\data\state.json` (macOS: `~/Library/Application Support/Claudiu`). Crashes are logged to `crash.log` beside it.
- Nothing is uploaded anywhere. Terminal contents are never logged. The only network request is the update check (public GitHub Releases API). Disable it by setting `"settings": { "update_url": "" }` in `state.json`.
- Child processes get a clean terminal environment: other terminals' markers (e.g. `WT_SESSION`) and Claude Code's nested-session markers are removed, `TERM_PROGRAM=Claudiu` is set. A `NO_COLOR` that leaked in from a Claude Code tool shell is dropped; a `NO_COLOR` you set yourself is respected.

## Known limits

- **5-hour / 7-day meters and exact context** come from Claude Code's own status-line data, at zero token cost: each Claude session Claudiu starts gets a per-process `--settings` file whose `statusLine` points back at Claudiu (`claudiu --statusline-tee`). Claude Code runs it locally after each response (its docs: the status line "does not consume API tokens"), and Claudiu keeps a tiny snapshot (limits, context figures, session id). Nothing in your Claude Code config files changes, and if you have your own status line, Claudiu forwards to it so your display is unchanged. At launch, so the meters aren't empty before your first message, Claudiu runs one `claude -p "/usage" --no-session-persistence` (once per launch, skipped if a reading from the last 5 minutes exists; Anthropic's cost docs note `/usage` may use a small number of tokens). Limits: Pro/Max accounts only; the live feed covers only sessions started from Claudiu; stale values are labelled with their age.
- **Context meter** is exact (as reported by Claude Code) once a session has responded; before that, or for sessions resumed outside Claudiu, it is estimated from the transcript (200k window, 1M once exceeded) and marked `~`.
- Transcript scanning is a best-effort import aid: every field is optional and unparseable lines are skipped; launching/resuming never depends on it.
- Closing the window or quitting ends running PTYs (Claudiu asks first). Claude sessions stay resumable; keeping live processes across restarts is a future feature.
- macOS support is implemented but not yet exercised by the author on a Mac.

## Building

Requires Rust (stable). On Windows: Visual Studio Build Tools (C++ workload) with the Windows SDK.

**Dev mode** (fast to compile, console window shows stderr, update checks are inactive):

```powershell
cargo run                                  # start Claudiu
cargo run -- C:\path\to\repo               # also register folders as projects
cargo test                                 # unit tests incl. process lifecycle
```

**Release build** (optimized, no console window; slower to compile, expect several minutes the first time):

```powershell
cargo build --release                      # -> target\release\claudiu.exe
target\release\claudiu.exe
```

State is stored in `%LOCALAPPDATA%\Claudiu\data\` (delete the folder to start fresh). If you start Claudiu from inside a Claude Code session, Claudiu scrubs the inherited Claude/terminal environment markers for its children (see *What Claudiu touches*).

## Installer and updates

Packaging and updates use [Velopack](https://velopack.io), with **GitHub Releases as the update server**: there is nothing to host.

```powershell
dotnet tool install -g vpk        # once (needs the .NET SDK)
scripts\package.ps1               # -> dist\releases\Claudiu-win-Setup.exe
```

Releasing (CI does this for you):

1. Bump `version` in `Cargo.toml`, commit.
2. `git tag v0.2.0 && git push --tags`.
3. `.github/workflows/release.yml` builds, packs (with delta packages), and publishes the installer and feed to the GitHub Release.

Installed copies check the repository's releases on startup and every 6 hours, show a banner, download in the background, and restart into the new version on request (confirming first if sessions are running). Dev builds and non-installed copies never check. The repository URL defaults to `https://github.com/m4rocks/claudiu` (`src/updater.rs`); override it with `settings.update_url` in `state.json`.

The Windows app icon is generated by `scripts/make-icon.ps1` (`assets/claudiu.ico`). Replace it with the official Claude Desktop/Clawd artwork if you have it.

## Layout

```
src/terminal.rs       PTY + alacritty Term (no UI types)       src/store.rs     persisted state + reconciliation
src/terminal_view.rs  GPUI renderer + input                    src/claude.rs    discovery, launch specs, read-only import
src/keys.rs           keystroke -> VT bytes                    src/git.rs       libgit2 repo info
src/glyphs.rs         block / box-drawing quads                src/platform.rs  OS-specific helpers, editors
src/app.rs            workspace state & actions                src/updater.rs   Velopack/GitHub updates
src/sidebar.rs, views.rs, widgets.rs, theme.rs                 UI
```

See [docs/TESTING.md](docs/TESTING.md) for the manual test checklist.
