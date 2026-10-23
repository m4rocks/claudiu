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
- [ ] RUNNING lists Claude sessions (no shells); hover shows ×. Drag a running session over another: they swap.
- [ ] Switch between sessions with clicks and Ctrl+Tab: the other terminals keep running (start something long in one).
- [ ] Click a **former** session: it opens straight in Claude Code (`--resume`), no intermediate page.
- [ ] Exit Claude (`/exit`): a banner appears with Resume; the entry moves from RUNNING into RECENT.
- [ ] Open a Claude session and close it before typing anything: it does **not** leave an empty "New Claude session" in history.
- [ ] Session titles update from "New Claude session" to the real title after the first prompt (within ~5 s).
- [ ] Right-click a session: Rename (shows only in Claudiu), Hide from list (check that the transcript under `~/.claude/projects` is still there), Copy session ID, Copy folder, Show in Explorer, End session.
- [ ] Search (Ctrl+Shift+F): filters running sessions and history by title, folder and project; Esc clears.
- [ ] RECENT shows 12 entries, newest first; a collapsed OLDER (with a count) holds the rest and its state survives a restart. While searching everything matching is shown.

## 3. Projects
- [ ] Add a Git repo (+ button, drag a folder onto the window, and `claudiu.exe <folder>`). Name and branch show; a repo with linked worktrees shows `+N`.
- [ ] Sessions started from any of its worktrees group under the project. Adding a project gives existing sessions in its folders its name as their chip.
- [ ] The project chip under the search box lists All projects, each project (✓ on the current one) and "Add project…". Choosing one narrows RUNNING and RECENT to its sessions; the choice survives a restart; removing that project clears it. With a project chosen the ⋯ button offers: new Claude session here, GitHub, open in VS Code / Zed (only installed ones appear), Explorer, copy path, remove project (sessions stay listed, nothing deleted). "+ Code session" starts in that project's folder.
- [ ] A non-Git folder works as a project.

## 4. Import / reconciliation
- [ ] First launch imports your existing Claude Code sessions (the sidebar fills within a second or two; the window never freezes).
- [ ] Restart: no duplicates; renames and hidden sessions persist.
- [ ] Rename or delete a transcript file outside Claudiu, relaunch: the session is flagged *stale*, never silently removed.
- [ ] Break `state.json` (invalid JSON), relaunch: app starts clean and keeps `state.json.corrupt`.

## 5. Usage meters (per-process mod, zero tokens)
- [ ] Start a **new** Claude session from Claudiu and send one message. Within about 5 s of the reply, ACCOUNT 5 HOUR / 7 DAY show percentages with a reset countdown, and SESSION → CONTEXT shows an exact figure (caption without `~`).
- [ ] CONTEXT reads "Nk of Mk tokens" with M the auto-compact threshold while auto-compact is on; switch auto-compact off in the dropdown and within a few seconds M becomes the model's window (and the bar shrinks accordingly).
- [ ] The values keep updating after later replies. Open a second Claude session: ACCOUNT stays the same (account-wide) while CONTEXT follows the selected session, and shows "no Claude session selected" for shells.
- [ ] `%LOCALAPPDATA%/Claudiu/data/usage/<session-id>.json` exists and holds only limits, context figures, session id, model, auto-compact, advisor (no prompts, paths or costs).
- [ ] Your own `~/.claude/settings.json` is **unchanged** (check its modified time) and contains nothing from Claudiu.
- [ ] Your own statusLine, if any, renders exactly as before, and Claude Code's footer keyboard hints are all there (Claudiu no longer sets a status line).
- [ ] Close and reopen Claudiu: the meters show the last values with an age label, not blank. Sessions started outside Claudiu do not feed the meters (expected).

## 6. Shell panel, right sidebar and layout
- [ ] There are no splits: no split button in the top bar, no "Add to split" in any menu, no Ctrl+Shift+D.
- [ ] Ctrl+Shift+T or Ctrl+J (or the top-bar terminal button, or the row menu's "Show or hide shell") opens a shell under the Claude terminal, in the session's folder. Type in both; Ctrl+Shift+] moves the keyboard between them. Toggling again hides it, and the next toggle brings back the same shell with its history.
- [ ] Drag the divider above the shell: it resizes, never below ~120 px and never past the window. Resize the window: both terminals re-wrap correctly. Switch to another session and back: the shell is still there at the same height.
- [ ] Ending the Claude session ends its shell. Exiting the shell (`exit`) closes the panel; the next toggle starts a fresh one.
- [ ] The right sidebar is on at first, under the top bar (window buttons stay top right). The top bar only has the sidebar toggle, title, folder, branch chip and the right-sidebar toggle; Ctrl+Shift+R and the toggle hide and show it, and the choice survives a restart.
- [ ] Menu: Open in <editor> (and ▾ picks another), Open folder, Commit & Push, Pull (greyed with "Committing…"/"Pulling…" while one runs), Show/Hide shell, GitHub. A session outside a Git project has no Commit/Pull; with no folder there are no editor rows.
- [ ] Status: CONTEXT and CACHE meters follow the shown session (they moved out of the left footer, which has only 5 HOUR / 7 DAY). In a real session `/config` auto-compact, `/advisor` (never run the setting forms against your own config) update the rows within a few seconds.
- [ ] Auto-compact and Advisor in STATUS are dropdowns while the session runs: choosing On/Off changes what `/config` shows in that session right away; choosing Off/Opus/Sonnet changes the saved advisor, and the row shows `active → saved` until `/clear` or `/compact` (turning the advisor off or on applies at once). "Other…" asks for a model name and refuses spaces. For an exited session they are plain text.
- [ ] Ask a session "find the session where we planned X and link it": it calls `search_sessions` (Claudiu asks first) then `link_session`; both sides then list the other under LINKED, clicking switches, × unlinks. A session started by another through `start_code_session` is linked automatically.
- [ ] With a project chosen in the chip, the header button reads "+ Code in <project>" and starts there; with All projects it opens "Start the session in…" listing the projects (or starts in the shown session's folder when there are none). Ctrl+Shift+N starts in the chosen project too.
- [ ] Ask a session to work on an issue ("look at issue 52"): it calls `attach_github` without a prompt and the item appears under GITHUB (kind icon, number, title). Clicking it opens the GitHub view on that item (the keyboard moves into the view); × removes it; it survives a restart.
- [ ] After quitting with a few Claude sessions open, the next start brings them back (the window paints first), in the same order, with the one that was shown on screen; ones closed before quitting stay closed. Same after killing Claudiu from Task Manager, and with another (older) Claudiu running on the same data folder: `running.json` survives it.

## 7. Lifecycle
- [ ] Ctrl+Shift+W / the row × / the pane × end a running session immediately (no prompt). Check Task Manager: no orphaned `claude.exe`/`pwsh.exe`/`conhost.exe` left behind.
- [ ] Smooth scrolling: with a mouse wheel (not a trackpad) the left list, the right menu, the Visual panel and the GitHub list/detail glide to a stop instead of jumping; a fast burst of notches keeps accelerating one glide. In a terminal with scrollback (shell: `1..300`), the wheel scrolls smoothly with no gaps or flicker at the top or bottom edge, selection by mouse while scrolled still picks the right text, typing snaps back to the bottom, and in Claude Code's full-screen views the wheel still scrolls the app in whole lines.
- [ ] Closing the window with running sessions asks "Quit Claudiu?"; Cancel keeps everything alive; Quit ends them and they appear as history.
- [ ] Kill Claudiu from Task Manager while sessions run: check for orphaned child processes. (Not guaranteed to be clean yet.)
- [ ] If the app ever dies unexpectedly, look at `%LOCALAPPDATA%\Claudiu\data\crash.log` and send it over.

## 8. Environment hygiene
- [ ] Launch Claudiu from a shell that has `NO_COLOR` or Claude's own env markers set (e.g. from inside a Claude Code session): Claude inside Claudiu is still colored and doesn't warn about a nested session.

## 9. Installer and updates (needs a published release)
- [ ] `scripts\package.ps1` produces `dist\releases\Claudiu-win-Setup.exe`; install, launch from Start menu, uninstall.
- [ ] Push a tag (`v0.1.0`), let CI publish; install it. Push `v0.1.1`: the installed app shows "Claudiu 0.1.1 is available", downloads, and "Restart to update" swaps versions (with a confirmation if sessions are running).
- [ ] A dev build (`cargo run`) never shows an update banner.
- [ ] macOS: build, run, and the same flows above. The macOS release job (universal build, ad-hoc signed, no notarization) has never run: check the Actions log after the first tag, and that the installed app launches past Gatekeeper (right-click → Open).

## 10. Title bar and GitHub pane (never run by hand yet)
- [ ] Windows: press on empty space anywhere along the top bar and the sidebar header and drag; the window moves. Double-click maximizes. The sidebar toggle, branch chip, right-sidebar toggle, settings icon, buttons row and search box still click normally.
- [ ] Ctrl+Shift+G (or the right sidebar's "GitHub" row, the sidebar "GitHub" button, or a project's ⋯ menu) shows the project's GitHub view in the right sidebar: the column widens (560 px at first), the arrow returns to the menu. It is not listed under CURRENT SESSION and never takes over the main area. Drag the column's left edge: it resizes within limits and the width is remembered after a restart. Switching to a session of another project switches the view to that project.
- [ ] Without `gh` or when signed out, the pane shows a neutral state; "Sign in" opens a shell tab running `gh auth login`. A folder with no GitHub remote says so.
- [ ] Issues / Pull requests tabs show Open and Closed counts. The search box takes GitHub syntax (`is:open label:bug author:@me`), Enter applies it; the chips and Open/Closed tabs rewrite the query. Scroll to the end and "Load more" appends.
- [ ] Pane narrower than ~860 px stacks list and detail with a "Back to list" button; wider shows them side by side.
- [ ] Issue: open, comment, close (with reason), reopen, edit title/body, react, labels, assignees, milestone, lock. PR: Conversation / Commits / Files changed tabs, review (approve / request changes / comment), merge methods, auto-merge, ready / draft, update branch, delete branch, checkout.
- [ ] "Ask Claude" starts a Claude session with a short prompt about the item, and shows it.
- [ ] Automated, no window opens (all need a signed-in `gh`, except `headless`): `cargo test -- --ignored render` draws every pane state with real data; `schema` sends every GitHub mutation with made-up ids to catch type mismatches; `live` checks the queries; `headless` drives the real workspace (sessions, splits, parked splits, the GitHub pane) and fails if starting a session blocks the window for 400 ms; `e2e` (about 4 minutes) runs every pane action against a throwaway private repo it creates on your account (`claudiu-gh-pane-test-<time>`; deleting it needs the `delete_repo` scope, otherwise `gh repo delete <name> --yes`).

## 11. Visual panel, and text written through the mod
Automated: `cargo test visual` (cards, parser, a headless draw of a `ui` block), `cargo test ask` (the request/answer files), `claude plugin test mod` (the mod's streaming and completion hooks) and `cargo test -- --ignored headless` (the panel opens, shares GitHub's width and clears).
- [ ] There are no chats: the header has only **+ Code session**; no Ctrl+Shift+M, no "New chat" on the welcome page, no Chat rows. Leftover `chats.json`, `acp/` and `chat-cache/` in the data folder are ignored.
- [ ] In a running session ask "draw me a release plan with a table and three stat boxes with the Visual panel". Claude loads `show_visual` (ToolSearch) and calls it with no permission prompt; the right sidebar switches to **Visual** by itself, without taking the keyboard from the terminal.
- [ ] While the call is written the card fills in live (Markdown renders as it grows, "drawing…" next to the title), then settles; a `ui` block draws natively in the dark theme (cards, bars, an SVG) and is never shown as code.
- [ ] A second `show_visual` call adds a card above the first. The arrow returns to the menu, which now lists **Visual · 2** (click reopens, × forgets the cards); × in the panel header does the same. Switching to a session without cards shows the menu; back again, the cards are there.
- [ ] The panel shares GitHub's width and the drag handle on its left edge; the menu keeps its own. GitHub, when opened, takes the column; closing it returns to the menu.
- [ ] A link in a card opens the browser (http/https only); nothing else is clickable. Mouse wheel scrolling glides.
- [ ] A session started with `CLAUDIU_NO_MOD=1` (with `CLAUDIU_TRACE_START=1`): `show_visual` answers that the panel isn't available; Commit & Push is dimmed or fails within ~8 s with "didn't pick the request up".
- [ ] **Commit & Push** in a running session with changes: the commit message appears within seconds, written by the session's mod (no `claude -p` process in Task Manager); the toast says "Committed and pushed: <subject>". With an ended session the row reads "Commit & Push · needs a running session" and is dimmed.
- [ ] Right-click a running session > **Regenerate title**: "Writing a new title…", then the row is renamed (also in Claude Code's `/resume`). For an ended session a toast says to start it first.
- [ ] Every icon is a Lucide icon: session/GitHub rows, close, archive, plus, chevrons, search, settings, the shell button, the branch pill, the Visual eye, GitHub states and checks.

### Sessions reaching each other
Automated by `headless` (a code session opened and read, an unknown project refused, an expired request dropped).
- [ ] In a code session ask "list my code sessions", then "open a Claude Code session in <project> and ...": the session opens in the sidebar at once, linked to the first, with no dialog.
- [ ] A request for an unknown project, or with a first message over 2,000 characters, is refused with the reason. A request older than 5 minutes is dropped.

## Start-up timing

- Start Claudiu with `CLAUDIU_TRACE_START=1`, open a few code sessions, then read `<data>/start-trace.log`. Repeat with `CLAUDIU_NO_MOD=1`, then `CLAUDIU_NO_MCP=1`, and compare the "last output of the start-up burst" lines.
- Resume an ended session: its last screen should show faintly behind the spinner until the real one appears.
