# Claudiu

A desktop app for juggling [Claude Code](https://code.claude.com) sessions. It's written in Rust with [GPUI](https://gpui.rs), the UI framework behind Zed.

I run several Claude sessions at once, across a few repos, and kept losing track of which terminal was which. Claudiu puts them all in one window, with a sidebar for projects and past sessions and a few meters for usage.

This is a personal project. It does what I need, it has rough edges, and plenty of things you'd expect are missing. Issues and PRs are welcome, but I can't promise I'll get to them quickly.

**Disclaimer:** this app was vibe-coded with Claude Code. I've reviewed and tested it thoroughly, but it's still AI-written code, so treat it accordingly.

## How it works

Claudiu doesn't reimplement Claude Code. It starts your own `claude` executable in a real pseudo-terminal (ConPTY on Windows, a Unix PTY on macOS) and draws the output with `alacritty_terminal`. Alt+P, Shift+Tab, mouse input and the rest all go straight to Claude Code, because as far as it can tell it's running in a normal terminal.

There's no API key and no login in Claudiu. Authentication stays with Claude Code.

## What you get

- **A real terminal.** 24-bit color, scrollback, selection, mouse reporting, bracketed paste, alt screen, Ctrl+click on links, IME input.
- **Current Session.** The sidebar lists everything that's running, Claude sessions and plain shells alike. Switching between them never touches the other processes.
- **Projects.** Git-aware, with branch and linked worktrees. Add one with the button, by dropping a folder on the window, or with `claudiu <folder>`.
- **History.** Ended Claude sessions stay in the sidebar, and a click resumes one through `claude --resume`. Plain shells aren't saved.
- **Import on startup.** Your existing Claude Code sessions are found and merged in (read-only, no duplicates).
- **Splits.** Up to four panes, each its own session.
- **Usage meters.** Context per session, plus 5-hour and 7-day account limits. See [Usage meters](#usage-meters) for the catch.
- **Git buttons.** Commit & Push (Haiku writes the message through your own `claude` CLI) and Pull. Branch management too.
- **Tab titles.** Claude names its own tabs.
- **Editors.** Open a project in VS Code or Zed, reveal it in Finder or Explorer, copy its path.
- **Auto-update** from GitHub Releases, for installed copies.

## Shortcuts

Claudiu's own shortcuts use Ctrl+Shift (Cmd+Shift on macOS), so plain Ctrl and Alt chords always reach Claude Code.

| Shortcut | Action |
|---|---|
| Ctrl+Shift+N | New Claude session, in the active session's folder |
| Ctrl+Shift+T | New shell |
| Ctrl+Shift+D | Split: new shell beside the current pane |
| Ctrl+Shift+W | End the focused session (no confirmation) |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous running session |
| Ctrl+Shift+] | Focus next pane |
| Ctrl+Shift+B | Toggle sidebar |
| Ctrl+Shift+F | Search sessions and projects |
| Ctrl+Shift+O | Add a project folder |

Inside the terminal, Ctrl+Shift+C or Ctrl+Insert copies, and Ctrl+V, Ctrl+Shift+V or Shift+Insert pastes. Right-click copies a selection, or pastes if there isn't one. Shift+PageUp/PageDown/Home/End scroll the history. Ctrl+C copies only while text is selected and is a normal interrupt otherwise. On macOS it's Cmd+C and Cmd+V.

## What Claudiu touches

I wanted this to be safe to try on a setup you care about, so:

- **Claude Code's data is read-only.** Claudiu never writes to `~/.claude` and never edits `settings.json`, keybindings or any other Claude Code config. Hiding a session only removes it from Claudiu's own index.
- **Two things are passed per process**, and live only as long as that session: a `--settings` file for the status line (see [Usage meters](#usage-meters)) and a `--mcp-config` file for the tab-title tool. Both point back at the Claudiu executable. Nothing lands in your Claude Code config.
- **IDE auto-connect is switched off** for the sessions Claudiu starts, through environment variables on the child process.
- **Everything else is documented CLI flags**: `--session-id`, `--resume`, `--settings`, `--mcp-config`, `--allowedTools`.
- **Claudiu's own state** is in `%LOCALAPPDATA%\Claudiu\data\state.json` on Windows and `~/Library/Application Support/Claudiu` on macOS. Crashes go to `crash.log` next to it.
- **Nothing is uploaded.** Terminal contents are never logged. The only network request Claudiu makes is the update check against the public GitHub Releases API. To turn it off, set `"settings": { "update_url": "" }` in `state.json`.
- **Clean environment for children.** Markers from other terminals (like `WT_SESSION`) and Claude Code's nested-session markers are removed, and `TERM_PROGRAM=Claudiu` is set. A `NO_COLOR` that leaked in from a Claude Code tool shell is dropped. One you set yourself is kept.

## Usage meters

The 5-hour and 7-day meters, and the exact context figure, come from Claude Code's own status-line data. Each Claude session that Claudiu starts gets a per-process `--settings` file whose `statusLine` runs `claudiu --statusline-tee`. Claude Code calls it locally after each response, and per its docs that costs no tokens. Claudiu keeps a tiny snapshot: limits, context figures, session id. If you have your own status line, Claudiu forwards to it, so yours looks the same as before.

So the meters aren't empty at launch, Claudiu also runs one `claude -p "/usage" --no-session-persistence` per launch. It skips this if there's a reading from the last 5 minutes. Anthropic's docs say `/usage` may use a small number of tokens.

Limits of this approach:

- Pro and Max accounts only.
- The live feed only covers sessions started from Claudiu. Stale values are labelled with their age.
- Before a session has responded, or for sessions resumed outside Claudiu, context is estimated from the transcript (200k window, 1M once exceeded) and shown with a `~`.

## Known gaps

- **macOS is lightly tested.** It builds, and CI produces an ad-hoc signed universal app, but I mostly developed on Windows. Expect bugs. The build isn't notarized, so Gatekeeper complains on first launch.
- **Quitting ends running PTYs.** Claude sessions stay resumable, but live processes don't survive a restart. Keeping them alive is something I'd like to do eventually.
- **Transcript scanning is best-effort.** It's only an import aid. Unparseable lines are skipped, and launching or resuming never depends on it.
- **The icon is a stand-in.** `scripts/make-icon.ps1` generates a placeholder. Swap in the real Claude Desktop/Clawd artwork if you have it.

## Building

You need stable Rust. On Windows, also Visual Studio Build Tools (C++ workload) and the Windows SDK.

```sh
cargo run                       # dev build: fast to compile, shows stderr, no update checks
cargo run -- path/to/repo       # also registers folders as projects
cargo test                      # unit tests, including process lifecycle
cargo build --release           # optimized, no console window; the first build takes a few minutes
```

State lives in the data folder listed above. Delete it to start fresh.

If you launch Claudiu from inside a Claude Code session, it scrubs the inherited Claude and terminal markers before starting children (see [What Claudiu touches](#what-claudiu-touches)).

## Installer and releases

Packaging and updates use [Velopack](https://velopack.io), with GitHub Releases as the update server, so there's nothing to host.

```powershell
dotnet tool install -g vpk      # once; needs the .NET SDK
scripts\package.ps1             # -> dist\releases\Claudiu-win-Setup.exe
```

To release, bump `version` in `Cargo.toml`, commit, then `git tag v0.2.0 && git push --tags`. `.github/workflows/release.yml` does the rest: a Windows job, then a macOS job (universal arm64 + x86_64, `.icns` from `scripts/make-icns.sh`), both publishing to the same release. For proper macOS signing, add `vpk --signAppIdentity/--notaryProfile`.

Installed copies check for updates on startup and every 6 hours, download in the background, and restart into the new version when you ask. Dev builds never check. The repo URL defaults to `https://github.com/m4rocks/claudiu` (`src/updater.rs`) and can be overridden with `settings.update_url`.

## Code layout

```
src/terminal.rs       PTY + alacritty Term (no UI types)       src/store.rs     persisted state + reconciliation
src/terminal_view.rs  GPUI renderer + input                    src/claude.rs    discovery, launch specs, read-only import
src/keys.rs           keystroke -> VT bytes                    src/git.rs       libgit2 repo info
src/glyphs.rs         block / box-drawing quads                src/commit.rs    Commit & Push, Pull
src/app.rs            workspace state & actions                src/mcp.rs       tab-title MCP helper
src/platform.rs       OS-specific helpers, editors             src/updater.rs   Velopack/GitHub updates
src/sidebar.rs, views.rs, widgets.rs, theme.rs                 UI
```

The manual test checklist is in [docs/TESTING.md](docs/TESTING.md).
