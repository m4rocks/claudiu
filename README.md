# Claudiu

A native desktop app (Rust + [GPUI](https://gpui.rs)) for running several [Claude Code](https://code.claude.com) sessions in one window, on Windows and macOS.

> **Claudiu is an independent project. It is not affiliated with, endorsed by or sponsored by Anthropic.** Claude and Claude Code are trademarks of Anthropic.

It's a personal project with rough edges; issues and PRs are welcome.

## How it works

- **Your own `claude`, in a real terminal.** Each session is one `claude` process in its own pseudo-terminal (ConPTY on Windows, a Unix PTY on macOS), drawn with `alacritty_terminal`. Claude Code sees a normal terminal, so Alt+P, Shift+Tab, mouse, paste and the rest go straight to it. Claudiu never types for you.
- **No API, no login.** Claudiu never calls the Anthropic API and has no API keys or sign-in of its own. Authentication stays with Claude Code.
- **Sessions are records.** A session is Claudiu's record of a Claude process. Ended sessions stay in the sidebar and resume through `claude --resume`. Switching sessions never touches another session's process. Sessions that were running come back at the next start.
- **Projects are Git-first.** Add a folder with the button, by dropping it on the window, or with `claudiu <folder>`. Claudiu never scans your disk for repositories.
- **Documented hooks only.** Each session is started with `--session-id` or `--resume`, plus a per-process `--plugin-dir` mod and a `--mcp-config` file (tab titles). They live only as long as the session.
- **Usage meters.** The mod writes a small snapshot after each response: 5-hour and 7-day limits, context, auto-compact and advisor settings. No extra tokens are spent, and your own status line is untouched. Limits show only for sessions started from Claudiu, and stale values are labelled with their age. For sessions resumed elsewhere, context is estimated from the transcript and shown with `~`.
- **Alt accounts.** An extra account is a separate Claude Code config folder (`CLAUDE_CONFIG_DIR`) holding only its own login; the rest links back to your main `~/.claude`. Only sign in to accounts you own.
- **GitHub, Git and editors.** Issues, PRs, Commit & Push and Pull go through the `gh` and `git` you already have. Open a project in VS Code or Zed.

## What Claudiu touches

- Claude Code's data is read-only. Claudiu never writes to `~/.claude`, never edits Claude Code's settings, and never deletes a transcript. Hiding a session removes it from Claudiu's index only.
- Nothing is uploaded. Terminal contents are never logged. The only network request is the update check against GitHub Releases (turn it off with `"settings": { "update_url": "" }` in `state.json`).
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
| Ctrl+Shift+] | Move the keyboard between Claude and its shell |
| Ctrl+Shift+B / Ctrl+Shift+R | Toggle left / right sidebar |
| Ctrl+Shift+F | Search sessions and projects |
| Ctrl+Shift+O | Add a project folder |
| Ctrl+Shift+G | Open GitHub |
| Ctrl+, | Open settings |

On macOS, `Ctrl+Shift` above is `Cmd+Shift`, and `Ctrl+J` is `Cmd+J`. `Ctrl+`` stays Ctrl.

Copy with Ctrl+Shift+C (or Ctrl+C while text is selected); paste with Ctrl+V or Ctrl+Shift+V. On macOS use Cmd.

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
