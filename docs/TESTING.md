# Manual test checklist

Automated: `cargo test` (store reconciliation, Claude transcript import, git/worktrees, key encoding, glyph geometry, process lifecycle).
Everything below needs eyes and a keyboard. Roughly ordered by risk. `[ ]` = not yet verified by anyone but the author's quick smoke runs.

## 1. Terminal correctness (Claude Code)
- [ ] Start a Claude session (Ctrl+Shift+N). The UI is colored (orange mark, colored accents) and the logo/borders have **no gaps between rows**.
- [ ] **Alt+P** opens the model picker. **Shift+Tab** cycles the mode. **Esc**, **Ctrl+C** (twice quits Claude), **Ctrl+D**, **Ctrl+Z** behave as in Windows Terminal.
- [ ] Arrow keys, Home/End, Ctrl+Left/Right word jumps, Ctrl+Backspace, PageUp/PageDown, Delete.
- [ ] Multiline input (`\` + Enter, and Shift+Enter / Alt+Enter) and long pasted text (bracketed paste).
- [ ] Resize the window and the sidebar toggle (Ctrl+Shift+B): content reflows, no garbage, no stuck rows.
- [ ] Unicode / emoji / box drawing / spinner characters render in the right cells (check Claude's spinner and tables).
- [ ] Mouse: select text by drag, double-click word, triple-click line; Ctrl+Shift+C copies; right-click copies/pastes; Ctrl+click opens a hyperlink; wheel scrolls history; selection clears when typing.
- [ ] Scrollback after a long answer, and Shift+PageUp / Shift+End.
- [ ] Image paste in Claude Code (Alt+V on Windows) if you use it.
- [ ] A plain shell (Ctrl+Shift+T): PowerShell colors, `Get-ChildItem`, `vim`/`less` (alt screen), tab completion, Ctrl+R.

## 2. Sessions and sidebar
- [ ] CURRENT SESSION lists Claude *and* shell processes; hover shows ×.
- [ ] Switch between sessions with clicks and Ctrl+Tab: the other terminals keep running (start something long in one).
- [ ] Click a **former** session: it opens straight in Claude Code (`--resume`), no intermediate page. Click a former shell: a new shell opens in its folder.
- [ ] Exit Claude (`/exit`): a banner appears with Resume; the entry moves out of CURRENT SESSION into its project's history.
- [ ] Open a Claude session and close it before typing anything: it does **not** leave an empty "New Claude session" in history.
- [ ] Session titles update from "New Claude session" to the real title after the first prompt (within ~5 s).
- [ ] Right-click a session: Rename (shows only in Claudiu), Hide from list (check that the transcript under `~/.claude/projects` is still there), Copy session ID, Copy folder, Show in Explorer, End session.
- [ ] Search (Ctrl+Shift+F): filters running sessions, projects and history; Esc clears.

## 3. Projects
- [ ] Add a Git repo (+ button, drag a folder onto the window, and `claudiu.exe <folder>`). Name and branch show; a repo with linked worktrees shows `+N`.
- [ ] Sessions started from any of its worktrees group under the project. Adding a project adopts existing "Other sessions" under it.
- [ ] Project menu: new Claude / shell here, open in VS Code / Zed (only installed ones appear), Explorer, copy path, remove project (sessions go to "Other sessions", nothing deleted).
- [ ] A non-Git folder works as a project.

## 4. Import / reconciliation
- [ ] First launch imports your existing Claude Code sessions (the sidebar fills within a second or two; the window never freezes).
- [ ] Restart: no duplicates; renames and hidden sessions persist.
- [ ] Rename or delete a transcript file outside Claudiu, relaunch: the session is flagged *stale*, never silently removed.
- [ ] Break `state.json` (invalid JSON), relaunch: app starts clean and keeps `state.json.corrupt`.

## 5. Usage meters (status-line tee, zero tokens)
- [ ] Start a **new** Claude session from Claudiu and send one message. Within about 5 s of the reply, ACCOUNT 5 HOUR / 7 DAY show percentages with a reset countdown, and SESSION → CONTEXT shows an exact figure (caption without `~`).
- [ ] The values keep updating after later replies. Open a second Claude session: ACCOUNT stays the same (account-wide) while CONTEXT follows the selected session, and shows "no Claude session selected" for shells.
- [ ] `%LOCALAPPDATA%/Claudiu/data/usage/<session-id>.json` exists and holds only limits, context figures, session id and model (no prompts, paths or costs).
- [ ] Your own `~/.claude/settings.json` is **unchanged** (check its modified time) and contains nothing from Claudiu.
- [ ] If you have your own statusLine (user, project or local settings), it renders exactly as before inside Claudiu. If you have none, Claude Code shows an empty status row and hides most footer keyboard hints while Claudiu's status line is active (documented Claude Code behaviour). Tell us if that bothers you.
- [ ] Task Manager: no lingering `claudiu.exe` helper processes after replies (the tee exits immediately).
- [ ] Close and reopen Claudiu: the meters show the last values with an age label, not blank. Sessions started outside Claudiu do not feed the meters (expected).

## 6. Splits and layout
- [ ] Ctrl+Shift+D splits (up to 4 panes), orientation button toggles rows/columns, clicking a pane focuses it, pane × ends that session.
- [ ] The selected session and sidebar visibility are restored after restart (sessions themselves are history until clicked).

## 7. Lifecycle
- [ ] Ctrl+Shift+W / the row × / the pane × end a running session immediately (no prompt). Check Task Manager: no orphaned `claude.exe`/`pwsh.exe`/`conhost.exe` left behind.
- [ ] Closing the window with running sessions asks "Quit Claudiu?"; Cancel keeps everything alive; Quit ends them and they appear as history.
- [ ] Kill Claudiu from Task Manager while sessions run: check for orphaned child processes. (Not guaranteed to be clean yet.)
- [ ] If the app ever dies unexpectedly, look at `%LOCALAPPDATA%\Claudiu\data\crash.log` and send it over.

## 8. Environment hygiene
- [ ] Launch Claudiu from a shell that has `NO_COLOR` or Claude's own env markers set (e.g. from inside a Claude Code session): Claude inside Claudiu is still colored and doesn't warn about a nested session.

## 9. Installer and updates (needs a published release)
- [ ] `scripts\package.ps1` produces `dist\releases\Claudiu-win-Setup.exe`; install, launch from Start menu, uninstall.
- [ ] Push a tag (`v0.1.0`), let CI publish; install it. Push `v0.1.1`: the installed app shows "Claudiu 0.1.1 is available", downloads, and "Restart to update" swaps versions (with a confirmation if sessions are running).
- [ ] A dev build (`cargo run`) never shows an update banner.
- [ ] macOS: build, run, and the same flows above. The macOS release job is unproven and expected to need tweaks (icon `.icns`, signing).
