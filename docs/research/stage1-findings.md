# Stage 1 findings: plumbing checks on the real adapter

Ticket: [Stage 1 plumbing checks on the real adapter](https://github.com/m4rocks/claudiu/issues/33). Spec: [Claudiu: spec](https://github.com/m4rocks/claudiu/issues/31).

**Verdict: every stage 2 path is feasible on Windows. No pivot to the raw stream-json CLI is needed.** Two measurements are not done (macOS, and per-call http hook latency end to end), and one check (a `tool.check` mod) was not run. Details below.

## How this was run

- Program: `cargo run -p claudiu-core --bin stage1 -- <check>` (`crates/core/src/bin/stage1.rs`). It starts the real pinned adapter (`@agentclientprotocol/claude-agent-acp` 0.94.0) with `CLAUDE_CODE_EXECUTABLE` set to the user's own `claude` (2.1.306), headless, one adapter per check, in a scratch project under the temp dir.
- Hooks are `type: "http"` hooks passed in `_meta.claudeCode.options.settings`, answered by a small local server inside the program. Sessions run in `bypassPermissions` (via `session/set_mode`) on Haiku with tiny prompts.
- Platform: Windows 11 only. **macOS was not run** (no Mac available in this session).
- Every result below is **observed** against the real adapter unless it says otherwise.

## The eight plumbing checks

| # | Check | Result |
|---|---|---|
| 1 | `PreToolUse` deny on Bash and Write under bypass | **Pass.** Both calls were refused, neither file was created, and the refusal text reached the model verbatim (it quoted `STAGE1-REFUSED`). |
| 2 | `updatedInput` rewrite and `updatedToolOutput` redaction | **Pass.** A `Read` of `a.txt` rewritten to `b.txt` returned `b.txt`'s content to the model. A `Read` result containing `hunter2` reached the model as `[REDACTED]`. The hook receives `tool_response` as `{"file":{"content",...},"type":"text"}` and the replacement must keep that shape. **The transcript on disk holds the replaced output, not the original** (no `hunter2` in it), which differs from the docs' "only changes what Claude sees" wording. |
| 3 | Hook that is dead or too slow | **Pass.** Dead port or a hook slower than its `timeout` is **fail-open by default** (the Write went through). With `"onFailure": "block"` the Write is blocked in both cases, and the model reports the connection refusal or timeout. |
| 4 | http hook latency per call | **Partly.** The server handled calls in 0.01 ms (p95). The end-to-end time from the adapter's side is **not observable**: `hook_started`/`hook_response` raw events appear only for the user's own command hooks (SessionStart), not for http hooks. Stage 2 must measure it with timestamps at the hook server against tool start, and the bench reports the p95 against the 50 ms bar. |
| 5 | Fork and parent cache | **Pass, with a trap.** After a parent turn that read 29,372 cached tokens, the first request of the `session/fork` child read 29,374 cached tokens and wrote 381. **But the fork came back on the default model (Sonnet), not the parent's Haiku:** its first request then wrote 29,664 cache tokens and cost $0.119. The child's model must be set explicitly (`session/set_config_option`, `configId: "model"`) before its first prompt. |
| 6 | Resume with a changed `systemPrompt` | **Pass.** A session started with an appended prompt (codeword ALPHA) and resumed with a different one (BRAVO) still answered ALPHA: the recorded prompt stays in force. |
| 7 | Kill the app mid-turn, look for orphans | **Pass on Windows.** With a long shell command running, closing the adapter's stdin and killing the adapter process hard both left **no** `claude.exe` and no leftover shell child after 6 s. |
| 8 | Project `disableAllHooks: true` | **Pass, with a requirement.** With the flag layer setting `"disableAllHooks": false`, our hook fired and refused the call despite the project's `true`. **With the flag layer silent, the project's `true` switched our hooks off** and the file was written. Claudiu must always set `disableAllHooks: false` in its settings. The `tool.check` mod question was **not run** (it needs a plugin dir). Managed settings can still disable hooks and are not tested. |

## The extra surfaces (spec story 92)

| Surface | Result |
|---|---|
| Questions (`AskUserQuestion`) | **Pass.** Needs `clientCapabilities.elicitation.form` at `initialize`; without it the model has no such tool (it answered `NO-TOOL`). The request is `elicitation/create` with `mode: "form"`, not a permission request. Single-select: a string property with `oneOf` of `{const, title, description}`. Multi-select: an array property with `items.anyOf` of the same shape. Free text: `question_N_custom`. Reply `{"action":"accept","content":{...}}`. Single (Blue), multi (S, M) and free text (Teal) all reached the model correctly. |
| Compaction signals | **Pass.** Sending `/compact` as prompt text shows a `Compact conversation` tool call, a `usage_update` drop (29,672 to 8,233 used) and one raw `compact_boundary` message when raw messages are on. Claude Code's own idle compaction is terminal-only and the adapter never enables it, from the earlier research branch `research/claude-code-surface-gaps`. |
| Conversation rewind | **Pass.** `session/fork` with `_meta.jetbrains.air.fork = {"version": 1, "messageId": <first assistant messageId>}` gave a child that remembered only the first turn. **`version: 1` is required**; without it the fork is silently the latest state. |
| File rewind | **Pass, with a requirement.** `_meta.claudeCode.options.enableFileCheckpointing: true` must be passed explicitly at session start; without it `claude -p --resume <id> --rewind-files <uuid>` fails with "No file checkpoint found". With it, the rewind restored `f.txt` from `v2` to `v1` (and to absent for the first message) **both while the adapter still held the session and after it closed**. The target is the `user_message_uuids` value from the raw events of each prompt. The command also needs `CLAUDE_CODE_ENABLE_SDK_FILE_CHECKPOINTING=true` and is not in `--help`, so it is undocumented. |

## Week-1 checkpoint

All eight plumbing paths and the extra surfaces work on Windows. Stage 2 can start. The adapter chain showed no reliability problem here (no orphans, no `.cmd` shim trouble), so the raw stream-json CLI stays a fallback only.

Settings Claudiu must apply, learned here:
- `disableAllHooks: false` in the flag layer.
- `onFailure: "block"` on the guard hooks, and none on the firewall hook.
- `enableFileCheckpointing: true`.
- `elicitation.form` in the client capabilities.
- An explicit model on every fork.
- `version: 1` in the fork-at-message meta.

## Not done

- macOS (all checks).
- End-to-end http hook latency (check 4).
- A `tool.check` mod against a hook denial (check 8).
- Managed-settings machines (hooks may be switched off by policy): the canary covers this at run time.
- Whether free-text answers survive the real chat flow (only the protocol round trip was checked).

## Addendum (found while building the hook server and MCP endpoint)

- `SessionStart` http hooks do not fire at all. The canary uses the first `UserPromptSubmit` or `PreToolUse` instead.
- `UserPromptSubmit` http hooks fire on every prompt, and `additionalContext` in `hookSpecificOutput` reaches the model. This is how a decision from the user is told to it once.
- An http MCP server (`{"type":"http","name":"claudiu","url":...,"headers":[]}` in `session/new` `mcpServers`) works. Claude Code sends `server/discover` (answer -32601), `initialize`, `notifications/initialized`, `tools/list` and `tools/call`. The tools show up as `mcp__claudiu__<name>`, but Claude Code defers them behind `ToolSearch`, which costs the model one extra call.
- http hooks emit no `hook_started` or `hook_response` events over ACP, so end-to-end hook latency is not observable from the client side.
