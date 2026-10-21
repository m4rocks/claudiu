# Research: Claude design validity in other harnesses

Resolves GitHub issue #15 (parent map #2). Researched 2026-10-21 against primary sources: the `openai/codex`, `anomalyco/opencode` (formerly `sst/opencode`) and `google-gemini/gemini-cli` repositories, and the first-party docs of Codex, OpenCode, Gemini CLI and Cursor. Cursor is closed source, so for Cursor the docs and changelog are the primary source.

The question: which parts of the Claude Code design hold in Codex, OpenCode, Gemini CLI and Cursor's CLI/SDK, and where does m4code need a fallback? Survey depth only. v1 stays Claude-only, and the map caps the product at three harnesses.

## Versions and how this was read

| Thing | Version / pin | Where it was read |
|---|---|---|
| Codex | `rust-v0.163.0` (2026-10-15); source read at `main` `d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4` | `codex-rs/**` raw files via the GitHub API; docs at `learn.chatgpt.com/docs/*` (the old `developers.openai.com/codex/*` URLs 308-redirect there) |
| OpenCode | `v1.19.3` (2026-10-16), tag commit `e3c4eb242585dcd77be1a867391b7c25fd47ce52` | `packages/plugin/src/index.ts`, `packages/opencode/src/session/tools.ts`, `.../session/message.ts`, `.../provider/transform.ts` at the tag; docs at `opencode.ai/docs/*` |
| Gemini CLI | `v0.65.0` (2026-10-20); source read at `main` `c1740d29301a73a47dc3501c1aecc7eb00c0ee58` | `packages/cli/src/gemini.tsx`; docs at `geminicli.com/docs/*`; Google blog post on the Antigravity transition |
| Cursor | CLI docs and changelog through `v2026.10.08`; `@cursor/sdk` TypeScript doc | `cursor.com/docs/*` (closed source, no code read) |
| Claude Code baseline | not re-derived | `docs/research/structured-claude-surface.md` (#3), `docs/research/sidekick-cache-sharing.md` (#4) and the #5 comment, all on local `research/*` branches |

Method note. Doc pages were fetched through a tool that summarises each page with a small model, so quoted doc text is as the tool returned it. Every cell tagged `source` below was decided from a raw source file, not from a doc summary. Nothing here was run: no harness was installed or executed, so behaviour is read, not observed.

Evidence tags used in the matrix: **source** (read in code), **docs** (first-party documentation, not checked against code), **inferred** (follows from code or docs but is not stated), **unverified** (not found in a primary source).

## Verdict

- **The design mostly transfers; the context firewall does not transfer equally.** Codex and OpenCode can rewrite tool input and replace what the model sees of a tool result, so a firewall is buildable on both. Gemini CLI and Cursor can rewrite input, but output replacement is partial (Gemini, through deny or a tail call) or MCP-only (Cursor).
- **ACP is available on all four, but it carries none of the control surface.** Hooks, system prompt, tool allowlists, skills and sandbox mode are set through each harness's own config, env vars or files at spawn time. m4code needs a per-harness spawn-config writer, not just a protocol client. (This repeats the #3 finding for Claude: the capability is the harness's, not the surface's.)
- **The sandbox row is not harness-dependent if the #5 decision holds.** #5 decided the jail wraps the whole harness process from outside. That works the same for every harness. The per-harness question is only whether the harness still works inside that jail (credential location, model-API egress) and whether its own sandbox conflicts with it. Only Codex and Cursor have a documented native Windows story, and Gemini's would clash with the jail design.
- **OpenCode has no OS sandbox at all** (confirmed in tree, plus two open feature requests). It relies entirely on the outer jail.
- **Gemini CLI is a poor third-harness candidate right now.** Consumer access ended on 2026-06-18 (details below). Antigravity CLI is its successor and was not surveyed.
- **Cursor is the weakest fit.** Models are Cursor-hosted and billed to a Cursor account, the prompt cache is opaque, fork is undocumented, and hooks are file-based only.
- **Suggested order if a second adapter is added (a judgement, not a research result):** Codex first (closest to Claude on hooks, sandbox, events and fork), OpenCode second (best hook surface, no sandbox).

## Capability matrix

Y = yes, P = partial, N = no. The Claude column is the baseline, cited from the earlier tickets.

| # | Capability | Claude Code (baseline) | Codex | OpenCode | Gemini CLI | Cursor CLI / SDK |
|---|---|---|---|---|---|---|
| 1 | Hooks or plugins that rewrite tool input and output (context firewall) | Y: `updatedInput`, `updatedToolOutput` (#3) | **Y/P** (source): input rewrite yes; output replace only through a block that returns hook text | **Y** (source): in-process plugin mutates args and result | **P** (docs): input merge yes; output replace via deny or tail call | **P** (docs): input yes; output replace for MCP tools only |
| 2 | Custom tools next to built-ins | Y: MCP, plus hooks | **Y** (source/docs): MCP; experimental `dynamicTools` | **Y** (docs): MCP; plugin `tool` | **Y** (docs): MCP, extensions | **Y** (docs): MCP; SDK `customTools` |
| 3 | Per-run system-prompt overlay and tool allowlist | Y: append prompt, `--tools`, `--allowedTools` (#3) | **Y/P** (source/docs): `developerInstructions`; allowlist for MCP, partial for built-ins | **P** (docs/inferred): agent `prompt`, `system` body field, permission rules | **P** (docs): full replacement only; allowlists good | **P** (docs): SDK `systemPrompt` replaces; permission rules |
| 4 | Structured events: tool calls, sub-agents, approvals, usage | Y (#3) | **Y** (source) | **Y** (source/docs) | **P** (docs) | **P** (docs) |
| 5 | Session fork and prompt-cache behaviour | Y: fork reads parent cache (#4) | **Y/P** (source, from #4): `thread/fork`, shared cache key | **P** (source/inferred): fork endpoint; 5-minute cache only | **N/P** (docs): no fork; implicit cache | **N** (docs): fork undocumented; cache opaque |
| 6 | Sandbox the app enforces regardless of permission mode | P: shell only, not native Windows; outer jail decided in #5 | **Y** (docs): sandbox is a separate parameter; Windows native | **N** (source): none | **P** (source/docs): opt-in whole-process sandbox | **P** (docs): macOS, Linux; Windows undocumented |
| 7 | Per-profile skills | Y: `--plugin-dir` and `--setting-sources` (#3; not re-checked) | **P** (docs/unverified) | **P** (docs) | **P** (docs/unverified) | **P** (docs) |
| 8 | Headless / ACP / SDK entry point | Y: ACP, SDK, `-p` stream-json (#3) | **Y** (docs/source): `app-server`, `exec --json`, ACP | **Y** (docs): `serve`, SDK, `acp` | **Y** (docs): `-p`, stream-json, `--acp` | **Y** (docs): `-p`, `agent acp`, SDK |

## Codex

Codex is OpenAI's agent (`openai/codex`, Rust). The structured entry point is `codex app-server`. Models are OpenAI's by default.

### Context firewall (hooks)

- Events (docs `learn.chatgpt.com/docs/hooks`, matching `HookEventName` in `app-server-protocol/src/protocol/v2/hook.rs`): `SessionStart`, `SessionEnd`, `SubagentStart`, `SubagentStop`, `PreToolUse`, `PermissionRequest`, `PostToolUse`, `PreCompact`, `PostCompact`, `UserPromptSubmit`, `Stop`, `Interrupt`, plus model-request events in the enum.
- **Input rewrite works.** `PreToolUse` output has `hookSpecificOutput.updatedInput` and `permissionDecision` (`codex-rs/hooks/schema/generated/pre-tool-use.command.output.schema.json`). The docs say rewriting requires `permissionDecision: "allow"` with `updatedInput`. The source tests agree: `allow` without `updatedInput` and `ask` both "fail open" as `unsupported permissionDecision` (`hooks/src/events/pre_tool_use.rs`). The last completed hook's `updatedInput` wins (`last_completed_updated_input_wins`).
- **Output rewrite is lossy and indirect.** The `PostToolUse` schema advertises `updatedMCPToolOutput`, but the source rejects it: `unsupported_updated_mcp_tool_output_fails_open` in `hooks/src/events/post_tool_use.rs` produces the error entry "PostToolUse hook returned unsupported updatedMCPToolOutput". What does work: `decision: "block"` with a `reason`. In `core/src/tools/registry.rs`, a blocked result becomes `FunctionCallError::RespondToModel(message)`, so the model sees only the hook's text and never the original output. A non-blocking `feedback_message` wraps the result in `PostToolUseFeedbackOutput { original, model_visible }`. So a hook can hand the model a compacted summary instead of the raw output, but the call is reported to the model as an error. Whether the app-server's own item stream still carries the original output was not checked.
- **Coverage.** Docs: hooks fire for shell commands (matched as `Bash`), `exec_command`, `apply_patch` (matchable as `Edit` or `Write`), MCP tools and local function tools. They do not fire for hosted tools such as `WebSearch`. The docs call hooks "a useful guardrail, not a complete enforcement boundary".
- **No process per call is possible.** Handler types are `command` and `mcp_tool`; `prompt` and `agent` are "parsed but skipped". An `mcp_tool` handler answers a hook through an MCP server m4code hosts, which is the Codex equivalent of Claude's `http` hook.
- **Trust gate.** Non-managed hooks "must be reviewed and trusted before they run" (docs). In source, `discovery.rs` carries a `bypass_hook_trust` flag; the CLI exposes it as `--dangerously-bypass-hook-trust` (`utils/cli/src/shared_options.rs`: "Intended only for automation that already vets hook sources"), and the app-server's `config_manager.rs` accepts a `bypass_hook_trust` boolean in request overrides. Managed hooks (`requirements.toml`, MDM, session flags) have a `Managed` trust status. Unverified: whether an app-server client can pass the override on `thread/start` end to end without a user prompt. Spike item.

### Custom tools

- MCP servers in `config.toml` with `enabled_tools`, `disabled_tools`, `default_tools_approval_mode` and per-tool `approval_mode` (docs, config reference). `codex mcp add` writes them.
- `thread/start.dynamicTools` (marked `#[experimental]`) lets the client declare tools that the app answers through the server request `item/tool/call` (`app-server-protocol/.../thread.rs`, `common.rs`). Experimental, so it can change.
- `codex mcp-server` was removed; the CLI doc says to use the app server instead.

### System prompt and allowlists

- `thread/start` takes `baseInstructions`, `developerInstructions`, `sandbox`, `approvalPolicy`, `config` (arbitrary overrides), `permissions` (named profile), `ephemeral` (source: `ThreadStartParams`). Config also has `developer_instructions` (additive) and `model_instructions_file` (replaces built-in instructions).
- Built-in tools can only be partly removed by config: `features.shell_tool`, `web_search = "disabled"`, `tools.view_image`, `agents.enabled` / `features.multi_agent` (docs). No `apply_patch` toggle appears in the config reference. A `PreToolUse` deny hook covers the rest. MCP tools have a real allowlist.

### Events, approvals, usage

- Source `common.rs`: `item/started`, `item/completed`, `item/agentMessage/delta`, `item/commandExecution/outputDelta`, `item/fileChange/patchUpdated`, `item/mcpToolCall/progress`, `turn/diff/updated`, `turn/plan/updated`, `hook/started`, `hook/completed`, `thread/tokenUsage/updated`, `account/rateLimits/updated`.
- Item types (`v2/item.rs`): `CommandExecution`, `FileChange`, `McpToolCall`, `DynamicToolCall`, `CollabAgentToolCall`, `SubAgentActivity`, `Plan`, `Reasoning`, `ContextCompaction`, and more.
- Approvals are server requests the app answers: `item/commandExecution/requestApproval`, `item/fileChange/requestApproval`, `item/permissions/requestApproval`.
- `ThreadUsage` (`v2/thread_usage.rs`) reports per model: `net_new_input_tokens`, `cached_input_tokens`, `input_tokens`, `output_tokens`, `estimated_usage_usd_micros`. This is richer than Claude's ACP usage.

### Fork and cache

From the #4 research, read from source: `thread/fork` exists; the Codex client sends a session-derived prompt cache key, and internal subagent sessions send `"{source}:{parent_thread_id}"`, so they share a key with their parent. Whether a plain (non-prediction) fork reuses the parent's key was not verified in #4. There is no TTL knob equal to Claude's `1h` setting in what was read; OpenAI's own guide gives about 5 to 10 minutes of inactivity for `in_memory` and `24h` retention as an option. Treat the 1-hour-TTL wedge as Claude-specific.

### Sandbox

- `sandbox_mode` is `read-only`, `workspace-write` or `danger-full-access`, passed per thread or turn by the client. Docs: `approval_policy = never` "still respects the chosen sandbox"; only `--dangerously-bypass-approvals-and-sandbox` or `danger-full-access` disables it. So if m4code never sends `danger-full-access`, the harness's permission mode cannot switch the sandbox off. `requirements.toml` can pin `allowed_sandbox_modes` for admins.
- Platforms (docs): macOS Seatbelt, Linux `bwrap` plus seccomp, Windows MXC (recommended where the OS supports it), "elevated" and "unelevated" legacy fallbacks, WSL2 uses the Linux sandbox. Unelevated has weaker network isolation and "doesn't support denied read paths". Protected paths stay read-only inside writable roots: `.git`, `.agents`, `.codex`, `.aws`.
- `codex sandbox` runs arbitrary commands under Codex's sandbox on macOS, Linux and Windows (CLI docs, details truncated in the fetch).
- Under `on-request`, the model can ask to run outside the sandbox; that arrives as an approval request, so m4code can refuse it. Under `never` it cannot ask.

### Skills

User skills live at `$HOME/.agents/skills`, repo skills at `.agents/skills` up to the repo root, admin at `/etc/codex/skills` (docs). `SKILL.md` needs `name` and `description`. `[[skills.config]]` entries in `config.toml` enable or disable a skill by path. `CODEX_HOME` moves the config directory (`utils/home-dir/src/lib.rs`), but the docs do not say it moves the `$HOME/.agents/skills` location, so a per-profile skill set needs either `skills.config` overrides per thread or a spike on relocating `HOME`. Unverified.

### Entry points

`codex app-server` (stdio JSONL by default; `ws://`, `unix://`; the CLI docs mark it Experimental and "may change without notice"), `codex exec --json` / `--output-schema` / `resume`, and ACP through `agentclientprotocol/codex-acp` (from #4, which implements `session/fork` on top of `thread/fork`).

## OpenCode

OpenCode (`anomalyco/opencode`, TypeScript on Bun; the `sst/opencode` URL redirects) is provider-agnostic and runs as a local HTTP server with a TUI client.

### Context firewall (plugins)

- Plugins are in-process TypeScript, not subprocess hooks. Hook list at `v1.19.3` (`packages/plugin/src/index.ts`): `tool.execute.before` (`output: { args }`), `tool.execute.after` (`output: { title, output, metadata }`), `tool.definition` (rewrite description and parameters sent to the model), `permission.ask`, `chat.params`, `chat.headers`, `shell.env`, `experimental.chat.system.transform` (`output: { system: string[] }`), `experimental.chat.messages.transform`, `experimental.session.compacting`.
- **Hooks are wired for built-in, MCP and MCP-resource tools** in `session/tools.ts`. For built-ins, the hook mutates the same `output` object the function returns, so output rewriting takes effect. The docs page for plugins only shows `tool.execute.before` mutating `output.args`, and states nothing about `after`; the code is the evidence here.
- Two details the code shows (inferred from reading it, not tested):
  - `tool.execute.before` receives `{ args }` and the tool then runs with the local `args` variable. Mutate `output.args` in place; reassigning `output.args` would be dropped.
  - For MCP tools the after-hook receives the MCP `result`, whose model-visible text comes from `result.content[]`, not a documented `output` string. A firewall over MCP tools must rewrite `content`.
- Blocking is done by throwing in `before` (docs example). A plugin tool with the same name as a built-in "takes precedence" (docs), which is the way to replace a built-in outright.
- Plugins load from `.opencode/plugins/` and `~/.config/opencode/plugins/`, or from an npm `plugin` array in `opencode.json` (docs). `OPENCODE_CONFIG_DIR` adds another directory. No process spawn per call.

### Custom tools

MCP servers (`local` and `remote`) via the `mcp` config key, and plugin tools through the `tool` helper from `@opencode-ai/plugin` (docs).

### System prompt and allowlists

- Agents (`opencode.json` `agent` section or `.opencode/agents/*.md`) take a `prompt` file, `permission`, `mode`, `steps`, `model`, `permission.task` (docs). The older `tools` boolean map is deprecated and merged into `permission`.
- `POST /session/:id/message` lists `system` and `tools` in its body (docs `server`), but the page does not define their formats, and the SDK page documents neither. Unverified.
- Permissions are `allow` / `ask` / `deny` per tool name with wildcards, "last matching rule wins". An allowlist is therefore a `"*": "deny"` rule followed by allows (inferred from the rule semantics).
- Per-run config can be injected with `OPENCODE_CONFIG_CONTENT` (precedence level 6; managed config still wins) or `OPENCODE_CONFIG` (docs).

### Events, approvals, usage

- SSE `GET /event` (first event `server.connected`, then bus events) and `GET /global/event`. Event names from the plugin docs: `message.part.updated`, `message.updated`, `permission.asked`, `permission.replied`, `session.status`, `session.idle`, `session.diff`, `session.compacted`, `file.edited`, `lsp.client.diagnostics`.
- Approval reply: `POST /session/:id/permissions/:permissionID` with `{ response, remember? }` (docs; allowed values not listed).
- Sub-agents run as child sessions (`parentID` on `POST /session`; session navigation commands in the agents docs), invoked by the Task tool.
- Usage per assistant message (source, `session/message.ts`): `cost`, `tokens.input`, `tokens.output`, `tokens.reasoning`, `tokens.cache.read`, `tokens.cache.write`.
- OpenAPI 3.1 spec at `/doc`; HTTP basic auth through `OPENCODE_SERVER_PASSWORD`.

### Fork and cache

- `POST /session/:id/fork` with `{ messageID? }` (docs). Not in the SDK page.
- Caching (source, `provider/transform.ts`, `applyCaching`): marks the first two system messages and the last two messages with `cacheControl: { type: "ephemeral" }` (Anthropic, OpenRouter, Alibaba), `cachePoint` (Bedrock) or the equivalent for other providers. **No TTL field is set in that file, so the Anthropic cache is the default 5-minute one.** That is a gap for the 1-hour-TTL wedge. For OpenAI-family providers it sets `promptCacheKey = sessionID`, so a forked session probably gets a new key and does not read the parent's cache (inferred; not tested). Whether child sessions share a prefix with their parent is unverified.

### Sandbox

None. A search of the `v1.19.3` file tree finds no sandbox code (only two icon files named `folder-sandbox`). Docs describe only prompt/allow/deny rules and `external_directory`. Open issues ask for one: [#48411](https://github.com/anomalyco/opencode/issues/48411) "Better execution permissions and sandboxing" and [#2242](https://github.com/anomalyco/opencode/issues/2242) "Is there a way to sandbox the agent ?". OpenCode must run inside m4code's outer jail.

### Skills

Discovery: `.opencode/skills`, `~/.config/opencode/skills`, and Claude- and agent-compatible `.claude/skills`, `~/.claude/skills`, `.agents/skills`, `~/.agents/skills` (docs). Per-agent `permission.skill` allow/deny patterns, and `tools: { skill: false }` removes the catalog. Global directories still load, so a per-profile set is done by per-agent permission, not by relocating the directory (inferred; `OPENCODE_CONFIG_DIR` relocation of skills was not checked).

### Entry points

`opencode serve` (HTTP + SSE), the `@opencode-ai/sdk` (`createOpencode()`), `opencode acp` (stdio JSON-RPC; `/undo` and `/redo` unsupported over ACP, docs). Structured output through `format: { type: "json_schema" }` (SDK docs; the page is inconsistent about `format` versus `outputFormat`).

## Gemini CLI

**Status first.** The repository README and the docs both say "Gemini CLI was replaced by Antigravity CLI on June 18th, 2026" for unpaid-tier and Google One users. The Google blog post says consumer Gemini CLI (Google AI Pro/Ultra and free Code Assist) stopped serving requests on 2026-06-18; enterprise access "remains unchanged"; paid Gemini and Enterprise Agent Platform API keys still work. The README also says Gemini CLI "has not been optimized for Gemini 4 Argon". The repo is not archived and is still releasing (`v0.65.0`, 2026-10-20). Antigravity CLI (Go, shares the harness of Antigravity 2.0) is the successor; the blog says Skills, Hooks, Subagents and Extensions carry over, with no 1:1 parity at launch. It was not surveyed here. Everything below applies to Gemini CLI itself, which in practice means API-key or enterprise users.

### Context firewall (hooks)

- 11 events, `type: "command"` only (docs `hooks`): `SessionStart`, `SessionEnd`, `BeforeAgent`, `AfterAgent`, `BeforeModel`, `AfterModel`, `BeforeToolSelection`, `BeforeTool`, `AfterTool`, `PreCompress`, `Notification`. Each call spawns a process.
- `BeforeTool` can rewrite input: `hookSpecificOutput.tool_input` "merges with and overrides" the model's arguments, so it can set and overwrite keys but not delete them. `decision: "deny"` sends `reason` to the model as a tool error.
- `AfterTool` cannot rewrite input. It can replace what the model sees: `decision: "deny"` makes its `reason` "replace the tool result"; `hookSpecificOutput.tailToolCallRequest` runs another tool whose result "will replace the original tool's response"; `additionalContext` is appended; exit 2 "hides the tool result" and uses stderr as the replacement (docs, `hooks/reference`). Docs only; not checked in code.
- `BeforeModel` can override parts of the request (model, temperature); the docs never say it can edit the system instruction. `BeforeToolSelection` filters the toolset.
- Failure mode: exit 0 parsed as JSON, exit 2 blocks (fail closed), any other code or malformed output is a fail-open warning. Timeout behaviour is not documented.

### Custom tools

MCP servers in `settings.json` with `includeTools` / `excludeTools` / `trust`; extensions; and in ACP mode the client registers its own MCP server at `initialize` (docs `acp-mode`).

### System prompt and allowlists

- `GEMINI_SYSTEM_MD` is a full replacement: "This is a full replacement, not a merge", and "none of the original core instructions will apply unless you include them yourself". `${AgentSkills}`, `${SubAgents}`, `${AvailableTools}` substitutions exist. There is no documented append. Since it is an environment variable, it can be set per spawned process. `GEMINI.md` files add context, and `SessionStart` `additionalContext` is injected into the first turn.
- Allowlists are strong: `tools.core` (built-in allowlist), `tools.exclude`, MCP `includeTools` / `excludeTools`, and a TOML policy engine under `~/.gemini/policies/` with `allow` / `deny` / `ask_user` rules and tiers up to Admin. A denied rule removes the tool from the model's view. In non-interactive mode `ask_user` acts as `deny`.

### Events and approvals

- Headless `-p` with `--output-format json` (one object with `response`, `stats`, optional `error`) or streaming JSON with event types `init`, `message`, `tool_use`, `tool_result`, `error`, `result`. Exit codes 0, 1, 42, 53 (docs `headless`). The page does not name the stream flag value.
- ACP (`gemini --acp`): methods listed are `initialize`, `authenticate`, `newSession`, `loadSession`, `prompt`, `cancel`, `setSessionMode`, `unstable_setSessionModel`. The page does not mention `session/request_permission` or `session/fork`. The file system is proxied through the client. Approval handling over ACP, sub-agent events and per-turn usage fields are unverified.

### Fork and cache

No fork or rewind: session management offers `--resume`, `--list-sessions` and named chat checkpoints (docs). Caching is the Gemini API's implicit caching: on by default for 2.5 and newer, minimum 2,048 or 4,096 input tokens by model, hits reported in `usage.total_cached_tokens`; explicit caching is not supported for Gemini 4 Argon and later (Google API docs). The app has no TTL or breakpoint control.

### Sandbox

Opt-in: `-s`, `GEMINI_SANDBOX`, or `tools.sandbox`. In `packages/cli/src/gemini.tsx` (about line 685) the CLI decides before anything else whether to relaunch itself inside the sandbox (`start_sandbox`), so the sandbox is chosen independently of the approval mode (inferred from that ordering; the docs do not say). macOS Seatbelt profiles, Linux Docker / Podman / gVisor / LXC, Windows native. The Windows implementation "uses the `icacls` command to set a 'Low Mandatory Level'" and the page warns these changes "are persistent on the filesystem". That conflicts with the #5 decision to pass `allowDaclMutation: false`. `security.toolSandboxing` (per-tool isolation) is off by default. `security.disableYoloMode` and `admin.secureModeEnabled` block YOLO.

### Skills

`~/.gemini/skills` and `~/.agents/skills` (user), `.gemini/skills` and `.agents/skills` (workspace), extension skills, built-ins; `/skills enable|disable` with `--scope`; `admin.skills.enabled` (docs). `GEMINI_CLI_HOME` moves the `.gemini` root, but whether it also moves `~/.agents/skills` is not documented. Unverified.

## Cursor CLI and SDK

Cursor's agent is closed source, with the same loop in the IDE, the CLI (`agent`) and the SDK (`@cursor/sdk`, Node 22.13+, plus a Python SDK page). Inference uses Cursor-hosted models: the SDK doc says "Local" means "where the agent loop and filesystem access run, not where the model runs", and it needs `CURSOR_API_KEY`; it "does not auto-discover credentials from a local Cursor app installation". So the account, the billing and the cache all sit with Cursor.

### Context firewall (hooks)

- Hook events (docs `hooks`): `preToolUse`, `postToolUse`, `postToolUseFailure`, `beforeShellExecution`, `beforeMCPExecution`, `afterShellExecution`, `afterMCPExecution`, `afterFileEdit`, `beforeReadFile`, `beforeSubmitPrompt`, `stop`, `subagentStart`, `subagentStop`, `sessionStart`, `sessionEnd`, `preCompact`, and more.
- `preToolUse` can return `permission`, `user_message`, `agent_message` and `updated_input`; "`ask` is accepted by the schema but not enforced for `preToolUse` today".
- `postToolUse` returns `updated_mcp_tool_output` (MCP tools only) and `additional_context`. Rewriting output of native tools is not documented, so the firewall has no hook route for native tool output.
- Config is file-based only (`.cursor/hooks.json`, user, project, enterprise, team). The SDK says "Hooks are file-based only ... No programmatic callbacks". Each call is a command, so a process spawn per call.
- Failure mode: exit 2 blocks; other non-zero codes fail open unless `failClosed: true`; invalid JSON on permission hooks blocks.
- CLI coverage: the hooks page gives no per-hook CLI table. The CLI changelog confirms hooks run in the CLI ("afterAgentThought/afterAgentResponse events emit in the CLI", April 2026; "Plugin hooks run from installed plugins", v2026.08.11). Which hooks fire in headless CLI and on Windows is unverified.

### Custom tools

MCP servers from `.cursor/mcp.json` (project and user). In the SDK, `mcpServers` inline and `local.customTools`, a map of `{ description, inputSchema, execute }` served to the agent as an MCP server named `custom-user-tools`; `execute` runs in the caller's process, skips interactive approval, and reaches sub-agents. Inline `mcpServers` are not persisted across `Agent.resume()`. Team-level MCP servers from the dashboard are not supported in ACP mode.

### System prompt and allowlists

SDK `systemPrompt` "replaces Cursor's built-in system prompt for the main loop"; rules and skills still load; it is local-only and not persisted across resume. Sub-agent definitions take `tools` and `disallowedTools`; disallowing `"task"` blocks sub-agents. CLI permissions: `Shell(cmd)`, `Read(glob)`, `Write(glob)`, `WebFetch(domain)`, `Mcp(server:tool)` with "deny rules take precedence over allow rules", in `~/.cursor/cli-config.json` or `<project>/.cursor/cli.json`. A main-agent built-in allowlist is not documented.

### Events, approvals, usage

- SDK `run.stream()` yields `SDKMessage` of type `system`, `user`, `assistant`, `thinking`, `tool_call` (running, then completed), `status`, `task`, `request`, `usage`. The doc warns that `args` and `result` shapes "are not stable". `usage` fires once per turn.
- CLI `--output-format stream-json` events: `system`/`init`, `user`, `assistant`, `tool_call` (`started` / `completed`), `result`. The output-format page documents no usage or token fields.
- ACP (`agent acp`): `session/request_permission` (allow-once / allow-always / reject-once), Cursor extension methods `cursor/ask_question`, `cursor/create_plan`, `cursor/update_todos`, `cursor/task`, and from v2026.09.15 "ACP clients can show subagents as their own sessions". MCP servers passed in over ACP since v2026.06.22.

### Fork and cache

Resume yes (`agent resume`, `--resume`, `Agent.resume(agentId)`); fork is not described and prompt caching is not described anywhere found. Treat both as unavailable.

### Sandbox

Docs (`agent/security/run-modes`): macOS Seatbelt; Linux Landlock plus seccomp (kernel 6.2+, otherwise it falls back to asking for approval); **Windows is not mentioned**. Modes: Auto-review (default), Allowlist, and Run Everything, where the sandbox is "No". The SDK's `sandboxOptions: { enabled }` defaults to `false` for local agents; the SDK also says "Local runs are not sandboxed by default". Admin or team policy can disable it, and the changelog notes "admin-disabled sandboxing is respected by auto-run". "Local mode requires ... platform sandbox-helper support." The relevant changelog line for jail nesting: "Run Cursor in sandboxes without macOS Keychain" (v2026.06.29).

### Skills

`.cursor/skills` plus `.claude/skills`, `.agents/skills`, `.codex/skills` discovered (changelog, March 2026); skills load in interactive, headless and editor-integration modes (May 2026). The SDK exposes `settingSources` (project, user, plugins) to choose which on-disk layers load, which is the nearest thing to a per-profile switch.

## Gaps and fallbacks

"Needed?" is relative to adding that harness as an adapter after v1. For v1 (Claude only) none is needed.

| Harness | Gap | Fallback needed? | Shape |
|---|---|---|---|
| Codex | Native-tool output cannot be rewritten in place; only replaced through a blocking `PostToolUse` that reports the call as an error | Yes, for the firewall | Use a `PostToolUse` block with a compacted `reason` for tools m4code wants to shrink, or prefer an m4code MCP tool and deny the native one with `PreToolUse`. Test whether the model reacts badly to "error" results |
| Codex | Hook trust must be granted before hooks run; app-server route unproven | Yes, until the spike passes | Pass the `bypass_hook_trust` override from `thread/start.config`, or ship hooks as managed hooks; spike |
| Codex | Built-in tools cannot all be removed (no `apply_patch` toggle found) | Partly | `PreToolUse` deny on the ones to remove |
| Codex | Per-profile skills location unverified | Maybe | Per-thread `skills.config` overrides, or run with a profile-specific `HOME` |
| Codex | `dynamicTools` and app-server are experimental | Watch | Pin `codex` version; use MCP for custom tools until stable |
| Codex | No 1-hour cache TTL equivalent found | Drop the TTL feature | Keep sidekick cache sharing (shared key) but drop the TTL wedge for Codex |
| OpenCode | No OS sandbox | Yes | Outer jail from #5 around the whole process; nothing inside the harness to rely on |
| OpenCode | Cache is 5-minute only on Anthropic; fork gets a new key on OpenAI-family providers (inferred) | Yes, if sidekicks matter | Drop sidekick cache sharing and the TTL claim for OpenCode, or add a plugin using `chat.params` / `chat.headers` (unproven) |
| OpenCode | Plugins are TypeScript running on Bun inside the server | Cost, not a gap | Ship m4code's firewall as an npm plugin, loaded through `OPENCODE_CONFIG_DIR`; a Rust shim cannot be a hook |
| OpenCode | MCP result shape differs from built-in shape | Yes | Firewall plugin branches on `content[]` versus `{ output }` |
| OpenCode | `system` and `tools` request-body fields undocumented | Yes | Use `experimental.chat.system.transform` plus permission rules via `OPENCODE_CONFIG_CONTENT` instead; `experimental.*` can break between releases |
| Gemini CLI | Product in transition; consumer access ended | Yes, before choosing it | Do not pick as one of the three until Antigravity CLI is surveyed; only API-key or enterprise users could use it |
| Gemini CLI | Output rewrite only through deny, tail call or exit 2; `tool_input` can only merge | Yes | Same as Codex: replace text through deny, or an MCP proxy plus `tools.core` allowlist that removes built-ins |
| Gemini CLI | System prompt is replace-only | Yes | Do not replace it; put overlays in `GEMINI.md` or `SessionStart` `additionalContext` (user-turn position, not system) |
| Gemini CLI | No fork; implicit cache only, no TTL control | Drop | No sidekick cache sharing for Gemini |
| Gemini CLI | Windows sandbox mutates ACLs persistently | Yes | Disable the native sandbox inside the outer jail; use the jail only |
| Gemini CLI | Approvals, usage and sub-agent events over ACP unverified | Spike | Use `-p --output-format stream-json` plus hooks for events |
| Cursor | Native-tool output cannot be rewritten (MCP only); hooks are command-spawn and file-based | Yes | MCP proxy with native tools denied by `Shell(...)` / `Write(...)` permission rules; or drop the firewall for native tools |
| Cursor | Fork and cache undocumented; hosted models; Cursor account required | Yes | Drop sidekick cache sharing, usage-protocol token numbers and cost accounting for Cursor (no usage fields in CLI output; SDK has a `usage` message) |
| Cursor | Windows sandbox undocumented; `Run Everything` and admin policy disable the sandbox | Yes | Outer jail from #5; do not rely on `sandboxOptions` |
| Cursor | SDK and CLI event shapes "not stable" | Watch | Prefer ACP or the SDK `SDKMessage` union with a version pin |
| All four | ACP carries no hooks, system prompt, allowlist, skills or sandbox control | Yes | Per-harness spawn-config writer in the adapter: files (`hooks.json`, `settings.json`, `opencode.json`, `.cursor/hooks.json`), env vars (`CODEX_HOME`, `GEMINI_SYSTEM_MD`, `OPENCODE_CONFIG_CONTENT`) and `thread/start` params |
| All four | Harness inside the outer jail needs credentials and model-API egress (the open tension from #5) | Spike | Per-harness credential path and egress allowlist; disable the harness's own sandbox while inside the jail |
| All four | Hooks fail open or are best-effort (Codex "guardrail, not a complete enforcement boundary"; Gemini and Cursor fail open on most errors) | Yes | Never use a hook as the enforcement layer; enforce in the outer jail, use hooks only to save tokens (matches the Claude finding in #3) |

### Latency of the firewall by harness

A firewall that spawns a process per tool call pays a process start each time (not measured here). Which harnesses can avoid that: OpenCode (in-process plugin), Codex (`mcp_tool` handler), Claude (`http` hook, from #3). Gemini CLI and Cursor spawn a command per hook event. The cost was not measured.

## Not verified, and spike candidates

1. Codex: an app-server client passing `bypass_hook_trust` on `thread/start` with no user prompt; whether the original output still reaches the item stream when `PostToolUse` blocks; whether `CODEX_HOME` or `HOME` relocates user skills.
2. Codex native sandbox nested inside the MXC jail (no source says it works; #5 flagged cargo, git and TLS under MXC as unverified for any child).
3. OpenCode: that mutating `output` in `tool.execute.after` changes the model-visible text for MCP tools end to end; the `system` / `tools` body fields; whether fork or child sessions share a cache prefix.
4. Gemini CLI: approvals, sub-agent events and usage fields over `--acp`; whether the sandbox choice really ignores `--yolo` (read from startup ordering only); relocation of `~/.agents/skills`.
5. Cursor: which hooks fire in headless CLI and on Windows; Windows sandbox support; any cache or fork behaviour (nothing documented).
6. Not surveyed at all: Antigravity CLI, Cursor's cloud agents, Codex cloud tasks, and any harness's behaviour with a Claude subscription login (a policy matter for the product owner, not a capability).

## Sources

Codex
- Docs (hooks, config reference, sandbox for Windows, approvals and security, CLI reference, skills): `learn.chatgpt.com/docs/hooks`, `/docs/config-file/config-reference`, `/docs/windows/windows-sandbox`, `/docs/agent-approvals-security`, `/docs/developer-commands?surface=cli`, `/docs/build-skills`.
- Source at `d138398`: `codex-rs/hooks/schema/generated/{pre,post}-tool-use.command.output.schema.json`, `codex-rs/hooks/src/events/{pre,post}_tool_use.rs`, `codex-rs/hooks/src/engine/discovery.rs`, `codex-rs/core/src/tools/registry.rs`, `codex-rs/utils/cli/src/shared_options.rs`, `codex-rs/app-server/src/config_manager.rs`, `codex-rs/utils/home-dir/src/lib.rs`, `codex-rs/app-server-protocol/src/protocol/common.rs`, `.../v2/{thread,turn,item,hook,thread_usage}.rs`.

OpenCode
- Docs: `opencode.ai/docs/{plugins,permissions,server,sdk,acp,agents,skills,config}/`.
- Source at `v1.19.3`: `packages/plugin/src/index.ts`, `packages/opencode/src/session/tools.ts`, `.../session/message.ts`, `.../provider/transform.ts`; issues #48411 and #2242 on `anomalyco/opencode`.

Gemini CLI
- Docs: `geminicli.com/docs/{hooks,hooks/reference,cli/sandbox,cli/headless,cli/acp-mode,cli/skills,cli/session-management,cli/system-prompt,reference/policy-engine,reference/configuration}/`; `ai.google.dev/gemini-api/docs/caching`; Google Developers Blog, "An important update: transitioning Gemini CLI to Antigravity CLI".
- Source at `c1740d2`: `packages/cli/src/gemini.tsx`; repository README.

Cursor
- Docs: `cursor.com/docs/{hooks,cli/overview,cli/acp,cli/changelog,cli/reference/permissions,cli/reference/output-format,agent/security/run-modes,sdk/typescript}`; `cursor.com/llms.txt` for the page index. The `.md` variants listed in `llms.txt` returned 404; the extension-less URLs worked.

Earlier tickets
- `docs/research/structured-claude-surface.md` (#3), `docs/research/sidekick-cache-sharing.md` (#4), and the #5 findings comment (jail wraps the whole harness; MXC on Windows, Seatbelt on macOS).
