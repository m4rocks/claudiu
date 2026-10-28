# CLAUDE.md

Claudiu is a native Rust + GPUI desktop app that manages sessions around the user's existing `claude` CLI.

## Settings

- Tracker: GitHub issues in m4rocks/claudiu
- Tests: when a cheap test seam exists
- Commits: only when the maintainer says
- Pushes and pull requests: only when the maintainer says
- Closing issues: ask first
- Subagents: only when the maintainer asks

## Working here

GitHub issues hold intent, one issue per feature with a checklist in its body; the code and the maintainer decide what is true.
Check off items the code already does. Add entries for requests that aren't listed, and for problems you notice outside the task, rather than fixing those in passing, so every diff stays reviewable. Order is a suggestion: if a request seems to depend on unfinished work, say so in one line and continue.

Care about the project's long-term health and whether the maintainer's problem actually goes away, not just the request in front of you. Suggest what they haven't asked for but will need. When a request would harm the project, say so plainly and early, give your own recommendation, and don't agree just to be agreeable. Hold your position if the pushback brings no new information. Once the maintainer decides, follow the decision and note your disagreement.

Ask the maintainer about product direction, scope, and anything irreversible or outward-facing, such as pushing, deleting data, adding a dependency or closing issues, unless the settings above allow it. Ask through the harness's structured question tool when it has one, recommended option first; otherwise ask one question per message with numbered options. The maintainer is the source of truth and decides everything; don't guess. Every real choice, including scope, behavior, wording and design, goes to them with your recommendation and the reason, and so does anything they'd want to know, such as a gap or a better approach, when you find it. What the repo already answers, such as how similar code is built, is a fact rather than a choice: follow it and say what you followed. Finishing a change is not such a point: report it in a line or two and stop, and leave committing and pushing to the settings or the maintainer's word.

The maintainer can override any setting for a single request, for example "go autonomous, commit and push everything" or "ask before each commit"; the override lasts until that request is done.

When a task finishes, say once what changed, what you skipped or couldn't check, and any risk. After that, follow the maintainer's current topic: raise finished work again only when the current topic affects it or they ask, and keep options about it out of questions on other subjects.

When the m4 skills are installed, route requests through them even when phrased casually, because they carry the steps that keep work consistent: a bug or error goes through m4:fix, building or changing behavior through m4:do, reviewing a diff through m4:tidy, planning through m4:shape, explaining code through m4:explore, and a repeated correction through m4:harden. One-line edits are made directly. Before writing prose that lands somewhere, such as docs, a README, comments, commit or pull request text, tracker entries, or a message to another person, run m4:unslop on the draft. In chat replies, follow its rules without invoking it.

Work inline; spawn subagents only as the settings allow, except the plan review in m4:shape, because they spend tokens the maintainer wants to spend deliberately.

## Code

People who don't use agents read and maintain this code, so aim for code that is obviously correct to a newcomer. Rank choices by correctness, then how easily the code reads and tests, then size; a one-liner that needs decoding is not simple.

Build only what was asked: features, options and configuration nobody requested wait until someone needs them. New code takes the shape of the existing code that does something similar: its file layout, naming, error handling, state, UI and tests, and its helpers rather than new ones. When that shape doesn't fit, or more than one design is reasonable, recommend the one whose correctness is most obvious and ask. Before writing new code, look for it in this repo, then the standard library and platform, then installed dependencies.

Before editing, find every place the change must reach (callers, tests, fixtures, config, exports) and finish all of them. When a better structure would make the task simpler, propose it; when it is bigger than the task, recommend doing the task and adding the redesign to the tracker with a one-line reason. Keep validation of untrusted input, error handling that prevents data loss, security and accessibility, including in code you move.

Comments explain why something non-obvious is true, such as a known limit and when it will matter. When code needs a comment to say what it does, rename or restructure it instead. Leave existing comments in code you don't change. New logic that can break quietly (branches, parsers, money, security, data writes, bug fixes) ships with a test that fails when it breaks.

Leave nothing behind but the change: no summary files, commented-out code, debug logs or session scripts. When a test fails, fix the code, or ask if the test's expectation looks wrong. Update the README in the same commit when user-visible behavior changes.

## Boundaries

Claudiu types into a session's terminal in one place. In editor mode, moving the keyboard to the terminal while lines are selected pastes `@path#L12-30: ` into Claude Code's input box, and leaving without typing erases it again (`terminal/view/typing.rs`, `app/selection_reference.rs`). It does this only while the cursor is in that input box. It stays a deliberate exception: messages, settings changes and every other request reach Claude Code through the mod, not the terminal. Any further typing into a PTY needs the maintainer's decision first.

## Enforced rules

| Rule | Enforced by |
|---|---|
