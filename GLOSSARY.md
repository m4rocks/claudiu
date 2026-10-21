# Claudiu

A native desktop app for running and organising Claude Code sessions per project.

## Language

**Task**:
One agent session, under the name the user sees. Nothing sits above or around it: a task is a session, no more.
_Avoid_: chat, thread, job, session (in the UI)

**Status label**:
An optional label on a task that shows its session's current status. It only reflects the session; it never drives or gates it.
_Avoid_: phase, stage, lifecycle

**Inline UI**:
A visual the model designs itself and places inside its reply, built from generic building blocks rather than app-defined components. Its buttons can only send a message back as the user.
_Avoid_: generative UI, card, widget

**Project**:
A folder, usually a Git repository, that tasks run in.
_Avoid_: workspace, repo (as the concept)

**Context firewall**:
The part of m4code that keeps tool output small before the model sees it, by compacting it and keeping the full text retrievable.
_Avoid_: filter, proxy, compressor

**Handle**:
A short code that names a piece of full tool output kept aside, so the model can ask for part of it later.
_Avoid_: id, pointer, reference

**Ledger**:
A record of what the model has already seen of each file, used to avoid sending it again.
_Avoid_: cache, history

**Subagent**:
A session another session starts to do part of its work, shown nested under its parent task. It is a session but not a task.
_Avoid_: child task, sidekick, helper
