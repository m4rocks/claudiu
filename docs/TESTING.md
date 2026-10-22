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
- [ ] RUNNING lists Claude sessions and open chats (no shells); hover shows ×. Drag a running session over another: they swap.
- [ ] Switch between sessions with clicks and Ctrl+Tab: the other terminals keep running (start something long in one).
- [ ] Click a **former** session: it opens straight in Claude Code (`--resume`), no intermediate page.
- [ ] Exit Claude (`/exit`): a banner appears with Resume; the entry moves from RUNNING into RECENT.
- [ ] Open a Claude session and close it before typing anything: it does **not** leave an empty "New Claude session" in history.
- [ ] Session titles update from "New Claude session" to the real title after the first prompt (within ~5 s).
- [ ] Right-click a session: Rename (shows only in Claudiu), Hide from list (check that the transcript under `~/.claude/projects` is still there), Copy session ID, Copy folder, Show in Explorer, End session.
- [ ] Search (Ctrl+Shift+F): filters running sessions, chats and history by title, folder and project; Esc clears.
- [ ] RECENT shows 12 entries, newest first, sessions and chats mixed; a collapsed OLDER (with a count) holds the rest and its state survives a restart. While searching everything matching is shown.

## 3. Projects
- [ ] Add a Git repo (+ button, drag a folder onto the window, and `claudiu.exe <folder>`). Name and branch show; a repo with linked worktrees shows `+N`.
- [ ] Sessions started from any of its worktrees group under the project. Adding a project gives existing sessions in its folders its name as their chip.
- [ ] The project chip under the search box lists All projects, each project (✓ on the current one) and "Add project…". Choosing one narrows RUNNING and RECENT to its sessions and chats (a project-less chat disappears); the choice survives a restart; removing that project clears it. With a project chosen the ⋯ button offers: new Claude session / chat here, GitHub, open in VS Code / Zed (only installed ones appear), Explorer, copy path, remove project (sessions stay listed, nothing deleted). "+ Code session" starts in that project's folder.
- [ ] A non-Git folder works as a project.

## 4. Import / reconciliation
- [ ] First launch imports your existing Claude Code sessions (the sidebar fills within a second or two; the window never freezes).
- [ ] Restart: no duplicates; renames and hidden sessions persist.
- [ ] Rename or delete a transcript file outside Claudiu, relaunch: the session is flagged *stale*, never silently removed.
- [ ] Break `state.json` (invalid JSON), relaunch: app starts clean and keeps `state.json.corrupt`.

## 5. Usage meters (per-process mod, zero tokens)
- [ ] Start a **new** Claude session from Claudiu and send one message. Within about 5 s of the reply, ACCOUNT 5 HOUR / 7 DAY show percentages with a reset countdown, and SESSION → CONTEXT shows an exact figure (caption without `~`).
- [ ] The values keep updating after later replies. Open a second Claude session: ACCOUNT stays the same (account-wide) while CONTEXT follows the selected session, and shows "no Claude session selected" for shells.
- [ ] `%LOCALAPPDATA%/Claudiu/data/usage/<session-id>.json` exists and holds only limits, context figures, session id, model, auto-compact, advisor and the last command name (no prompts, paths or costs).
- [ ] Your own `~/.claude/settings.json` is **unchanged** (check its modified time) and contains nothing from Claudiu.
- [ ] Your own statusLine, if any, renders exactly as before, and Claude Code's footer keyboard hints are all there (Claudiu no longer sets a status line).
- [ ] In the session, type `/advisor` (no argument) and `/autocompact`: the snapshot's `last_command` follows. Do not run the setting forms: they change your saved settings.
- [ ] Close and reopen Claudiu: the meters show the last values with an age label, not blank. Sessions started outside Claudiu do not feed the meters (expected).

## 6. Shell panel, right sidebar and layout
- [ ] There are no splits: no split button in the top bar, no "Add to split" in any menu, no Ctrl+Shift+D.
- [ ] Ctrl+Shift+T (or the top-bar terminal button, or the row menu's "Show or hide shell") opens a shell under the Claude terminal, in the session's folder. Type in both; Ctrl+Shift+] moves the keyboard between them. Toggling again hides it, and the next toggle brings back the same shell with its history.
- [ ] Drag the divider above the shell: it resizes, never below ~120 px and never past the window. Resize the window: both terminals re-wrap correctly. Switch to another session and back: the shell is still there at the same height.
- [ ] Ending the Claude session ends its shell. Exiting the shell (`exit`) closes the panel; the next toggle starts a fresh one. Chats have no shell button.
- [ ] The right sidebar is on at first, under the top bar (window buttons stay top right). The top bar only has the sidebar toggle, title, folder, branch chip and the right-sidebar toggle; Ctrl+Shift+R and the toggle hide and show it, and the choice survives a restart.
- [ ] Menu: Open in <editor> (and ▾ picks another), Open folder, Commit & Push, Pull (greyed with "Committing…"/"Pulling…" while one runs), Show/Hide shell, GitHub. A session outside a Git project has no Commit/Pull; with no folder there are no editor rows.
- [ ] Status: CONTEXT and CACHE meters follow the shown session (they moved out of the left footer, which has only 5 HOUR / 7 DAY). In a real session `/config` auto-compact, `/advisor` (never run the setting forms against your own config) and the last slash command update the three rows within a few seconds.
- [ ] Ask a session to work on an issue ("look at issue 52"): it calls `attach_github` without a prompt and the item appears under GITHUB (kind icon, number, title). Clicking it opens the GitHub view on that item (the keyboard moves into the view); × removes it; it survives a restart.
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

## 11. Chats (Claude through ACP)
Automated: `cargo test -- --ignored headless` also installs the adapter into a throwaway data folder, starts a chat, checks it lists the agent's skills and commands, sends one tiny prompt (costs a few cents), closes the chat and reopens it from history. Needs Node.js, npm and a signed-in `claude`.
- [ ] There are no Chat/Code tabs: **+ Code session** and **+ Chat** sit above the one list, where a chat has its own icon. A chat's conversation never shows up as a code session after a rescan.
- [ ] **+ Chat** opens the empty chat at once with the cursor in the box; Claude starts behind it with no "Starting" message. Type and send before it is ready: the message shows right away and is delivered as soon as Claude is up, in order.
- [ ] First **+ New chat** shows "Setting up the Claude adapter (first time only)…" (one `npm install` into `<data>/acp`, about 15 s), then the empty chat. Later chats start in a few seconds.
- [ ] Without Node.js the pane says so with a link; with a broken `claude` path it says Claude Code was not found. "Try again" retries.
- [ ] Task Manager while a chat is open: one `node.exe` and, below it, **your** `claude.exe` (`~/.local/bin`), not a bundled copy. Ending the chat (row ×, pane ×, or closing the app) leaves neither behind: check this by hand, the automated run only proves the chat view is dropped (which closes the adapter).
- [ ] Type `/`: your skills and commands appear (Up/Down, Tab/Enter, Esc, click). A command without arguments runs at once; one with an argument hint is filled in for you to finish. Plugin skills like `/ponytail:ponytail-help` work.
- [ ] Streaming text renders as Markdown; tool calls show as rows (click to see output); plans show as a checklist; Stop cancels the turn.
- [ ] Mode / model chips under the input open a picker; the choice applies to the next message. In Manual mode a tool call shows "Claude asks permission" with buttons; a turn never hangs waiting for it.
- [ ] A chat you close stays in the list (dim dot); clicking it reopens it with the old messages. The × on a dim row removes it from the list only; the transcript under `~/.claude/projects` stays. A chat closed before the first message leaves no entry.
- [ ] Chats belong to no account: after **Switch account** the open chats restart under the new login once idle, with the same conversation; the 5 HOUR / 7 DAY meters (left) and the CONTEXT meter (right sidebar) follow the chat you are looking at (they read "after first message" until its first reply).
- [ ] **Ctrl+Shift+M** opens a new chat. Ctrl+V with a picture (or a copied image file) in the input attaches it as a thumbnail with a ×; Send includes it, and the sent message shows the picture. Text still pastes as text. Images over 4.5 MB are refused with a toast; reopened history shows "[image]" for old pictures.
- [ ] A new chat is **not tied to a project** (the chip under the input reads "No project", the right sidebar has no editor or folder rows). The chip opens a project list whenever the chat is idle (not mid-turn): before the first message picking one just restarts in that folder; after messages the old ones stay on screen above a notice and Claude starts a new conversation in the folder; the earlier messages are compacted into a summary (Haiku, in the background) that rides along with your next message, so Claude knows the gist (ask "what were we doing?"). The chip stays visible, but not clickable, while Claude is answering. The earlier Claude session is no longer listed under the chat, so it can show up in the Code list after a rescan. **New chat here** in a project's menu starts it tied.

- [ ] With nothing shown, the welcome page has New code session / New chat / Add project and recents of both kinds.
- [ ] `/` works in the middle of a message ("please run /sk" offers matches; picking one completes only that word and keeps the rest). The list scrolls (wheel, Up/Down) through every skill and command, with no cut-off.
- [ ] Open a new chat, type "half a thought" without sending, quit and start Claudiu: the chat is still there, loads, and the box holds "half a thought". Do the same without typing: the empty chat is gone after restart (and when closed, and on quit).
- [ ] Quit Claudiu with a chat that has messages and reopen it: the old conversation is on screen at once, with "Loading the conversation…" at the bottom, then the live one replaces it and the box accepts Send.
- [ ] Right-click a chat or a Claude Code session > **Regenerate title**: a toast says "Writing a new title…", then the row is renamed (a session's new name also shows in Claude Code's `/resume`). A chat or session with nothing said says "Nothing to title yet."
- [ ] Every icon is a Lucide icon: session/chat/GitHub rows, close, archive, plus, chevrons, search, settings, the shell button, the branch pill, GitHub states and checks. A working session or chat, and a running tool, show a turning loader. No blank spots where an icon should be.

### Chats and code sessions reaching each other
Automated by `headless` (the confirmation, the decline, a chat opened with a first message, a code session opened and read).
- [ ] In a chat ask "list my code sessions", then "open a Claude Code session in <project> and ...": Claudiu shows a dialog naming the chat and what it wants (folder, first prompt) and nothing happens until **Allow**; Cancel, Esc or a click outside tells Claude the user declined and it stops. Enter does **not** confirm.
- [ ] In a code session ask "start a chat about X": a dialog appears, Allow opens a chat that already received the first message, not tied to a project. Same for reading: `read_chat` works for an open chat, `read_code_session` for a running session; an ended session or closed chat says it can't be read.
- [ ] A request for an unknown project, or with a first message over 2,000 characters, is answered at once, without a dialog. The dialog shows the whole first message (scrolling if long). Several requests queue, one dialog at a time; opening Settings (Ctrl+,) while one is showing declines it. Allowing a request after Claude gave up (more than 5 minutes) does nothing and says so.
- [ ] **Remove from list** on a chat, then restart Claudiu: its conversation does not appear in the Code list.

## Generative UI (`ui` blocks in chat)

- [ ] Ask a chat for "a dashboard of three stats with a bar chart" and "a comparison table": each comes back as a native card/table/drawing in the dark theme, not as code.
- [ ] While it streams, the block fills in without flicker or errors; an SVG shows "Drawing…" until it closes.
- [ ] Reopen the chat (cache and `session/load`): the blocks render again. Copying a block's text yields the `ui` source.
- [ ] A link in a block opens the browser; nothing else is clickable.
Automated: `cargo test chat::ui` (parser, plus a headless draw of a sample block).
