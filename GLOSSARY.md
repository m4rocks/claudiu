# m4code

A native desktop app for running and organising coding-agent sessions per project.

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
