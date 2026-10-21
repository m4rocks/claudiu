# Research: structured Claude Code surface without a PTY

Issue: m4rocks/claudiu#3 (parent map #2). Date of research: 2026-10-21.

**Question.** With the PTY gone, what structured surface does m4code use to drive Claude Code, and what can it control on that surface? Compare ACP, the Agent SDK and the CLI's `stream-json` mode.

## Versions this was checked against

| Thing | Version | Where it was read |
|---|---|---|
| Claude Code CLI (the user's own) | 2.1.306 | `claude --version` on this machine |
| `@agentclientprotocol/claude-agent-acp` (the adapter the Chat tab uses) | 0.94.0 | installed copy, `%LOCALAPPDATA%\Claudiu\data\acp\node_modules\...\dist\*.js` (read as source) |
| `@anthropic-ai/claude-agent-sdk` (the adapter's dependency) | 0.3.303 (`claudeCodeVersion` 2.1.303) | same install: `sdk.d.ts`, `sdk.mjs` |
| `@agentclientprotocol/sdk` | 2.0.0 | `src/chat/adapter/package-lock.json` |
| ACP protocol | v1 is stable and what Claudiu speaks (`protocolVersion: 1`); v2 is published as Draft | agentclientprotocol.com |
| Claude Code docs | pages fetched as `.md` from `code.claude.com/docs/en/` on 2026-10-21 | listed under Sources |

## The one finding that shapes everything

All three surfaces are the same engine. The Agent SDK is a wrapper that spawns the `claude` binary with `--output-format stream-json --verbose --input-format stream-json` (read in `sdk.mjs`) and talks a bidirectional JSON control protocol over it (`control_request` subtypes seen in the bundle include `initialize`, `interrupt`, `set_model`, `set_permission_mode`, `mcp_set_servers`, `can_use_tool`, `hook_callback`, `get_context_usage`; the SDK passes `--permission-prompt-tool=stdio` when a `canUseTool` callback is given). The ACP adapter is a wrapper around the SDK. So for hooks, `--settings`, system prompt, allowlists, bypass behaviour, resume and fork, the **capability is Claude Code's, not the surface's**. What differs between the three is transport, who normalizes events, and what can be passed through.

```
ACP   : Rust app -> (ACP/JSON-RPC) -> Node adapter -> Agent SDK -> claude (stream-json + control protocol)
SDK   : Rust app -> (own IPC)      -> Node/Python host using SDK -> claude
CLI   : Rust app ------------------------------------------------> claude -p --input/output-format stream-json
```

Consequence: do not pick a surface for "features"; pick it for ownership of the boundary (normalization, harness-neutrality, process count, how much undocumented protocol m4code must speak).

## Per-surface findings

### 1. ACP via `claude-agent-acp` (what `src/chat/` uses today)

- **Events.** Normalized `session/update` kinds: message and thought chunks, `tool_call` / `tool_call_update` (with `kind`, `status`, `rawInput`, `rawOutput`, `content` including `diff` and `terminal`), `plan`, `available_commands_update`, `config_option_update`, `current_mode_update`, `session_info_update`, `usage_update`. [ACP tool-calls, v1]
- **Approvals.** `session/request_permission` with options of kind `allow_once` / `allow_always` / `reject_once` / `reject_always`. [ACP tool-calls] The adapter routes the SDK's `canUseTool` to it (`acp-agent.js`, `canUseTool: this.canUseTool(sessionId)`).
- **Sub-agents.** The stable v1 protocol has no sub-agent concept. The "Subagent Sessions" RFD is a draft (initial draft 2026-08-19). The adapter implements it only if the client advertises a `subagents` client capability (`acp-subagents.js`). Without it, a sub-agent's updates arrive in the parent stream, stamped with `_meta.claudeCode.parentToolUseId` (`stampParentToolUseId`, `acp-agent.js`). Sub-agent text and thinking are only forwarded if `_meta.claudeCode.options.forwardSubagentText` is true or the client has the capability.
- **Usage.**
  - `usage_update {used, size}` during a turn; after each result it adds `cost {amount, currency: "USD"}`. The model rides in `_meta["_claude/model"]`; the 5-hour and 7-day windows ride in `_meta["_claude/rateLimit"]` (`acp-agent.js`, ~lines 2927, 5003, 5969).
  - `PromptResponse.usage` carries `inputTokens`, `outputTokens`, `cachedReadTokens`, `cachedWriteTokens`, `totalTokens` (`sessionUsage`, `acp-agent.js:9018`) plus `_meta.quota` with per-model rows. The ACP RFD for end-turn usage is still Draft ("not ready for Preview"), so this shape is adapter-specific for now. `usage_update` itself is stabilized in ACP (RFD "Session Context Size and Cost", Completed 2026-06-05).
- **Resume and fork.** `loadSession: true`; `sessionCapabilities` advertises `additionalDirectories`, `close`, `delete`, `fork`, `list`, `resume`, `systemPrompt` (`initialize`, `acp-agent.js:1402-1411`). `session/load` replays history; `session/resume` and `session/list`/`close`/`delete` are stabilized in ACP. `session/fork` is still `unstable_forkSession` (the RFD is not marked stabilized; it says it was "inspired by the abilities exposed in Claude Agent SDK"). The adapter's fork copies the transcript to a new session id (`fork-session.js`, uses the SDK's `forkSession`).
- **`--settings` and hooks.** The session request takes `_meta.claudeCode.options`, spread into the SDK `Options` (`acp-agent.js:8430-8530`). `settings` (object or file path) is forwarded as the flag-settings layer, so JSON-expressible hooks (`type: "command" | "http" | "mcp_tool" | "prompt" | "agent"`) work. `hooks` (in-process callbacks) is explicitly classified as "cannot cross JSON-RPC" (`OPTION_REBUILDS_SESSION.hooks = false`, `acp-agent.js` ~line 440). The adapter registers its own PreToolUse (Edit and Write preview) and PostToolUse hooks and merges any `userProvidedOptions.hooks` ahead of them, but a JSON-RPC client cannot supply callbacks. It also loads `settingSources: ["user","project","local"]` by default (overridable through the same options).
- **System prompt.** Two routes. (a) Top-level `systemPrompt: {mode: "append" | "override", content: [text...]}` on `session/new`, `load`, `resume`, `fork`; this is what `src/chat/view.rs:566` sends. It is the unstable "Client-Provided System Prompt" RFD, status Draft as of 2026-10-09/12, behind `unstable_session_system_prompt`; the adapter advertises it (`client-system-prompt.js`). `append` maps to the SDK preset `claude_code` plus `append`. (b) `_meta.systemPrompt` as a string or a preset object (`append`, `excludeDynamicSections`).
- **Tool allowlists.** `_meta.claudeCode.options.{allowedTools, disallowedTools, tools}`; `src/chat/view.rs:567` already passes `allowedTools`. `_meta.disableBuiltInTools: true` is the legacy shorthand for `tools: []`. Elicitation is not advertised by Claudiu, so the adapter adds `AskUserQuestion` to `disallowedTools` (`acp-agent.js:8384`): the Chat tab cannot ask the user structured questions today.
- **Escape hatch for raw events.** `_meta.claudeCode.emitRawSDKMessages: true` (or a filter list) makes the adapter forward every SDK message as an ext notification `_claude/sdkMessage` (`acp-agent.js:3922`). That is the full Claude stream (task_*, hook_*, compact boundaries, rate limits) on top of ACP, at the price of leaving the standard.
- **Cost of the chain.** Three processes (Rust, node, claude), a Node dependency (`npm ci --ignore-scripts` of a pinned lock, `src/chat/acp.rs`), and reliance on adapter extensions. The Rust ACP crate `agent-client-protocol` (3.4.0, repo `agentclientprotocol/rust-sdk`, crates.io) exists; `Cargo.toml` does not use it (Claudiu hand-rolls JSON-RPC in `src/chat/acp.rs`).

### 2. Agent SDK (TypeScript or Python) driven by an m4code sidecar

- **Events.** The `SDKMessage` union (`sdk.d.ts:5442`) is the superset: assistant and user messages, partial stream events, `system/init`, `result` (with `total_cost_usd`, `usage`, `modelUsage`, `permission_denials`), task_started/progress/updated/notification, hook_started/progress/response, tool_progress, tool_use_summary, compact_boundary, api_retry, rate_limit_event, permission_denied, session_state_changed, and more.
- **Approvals.** `canUseTool` callback, `PermissionRequest` hook, or `permissionPromptToolName`. `permissionPrompts: 'none'` denies instead of asking (needs Claude Code 2.1.259+).
- **Resume and fork.** `resume`, `continue`, `forkSession`, `resumeSessionAt`, `sessionId`, `persistSession: false` (TS only), plus `forkSession()` and session list/read helpers. Forking "branches the conversation history, not the filesystem". [agent-sdk/sessions]
- **Hooks.** Callbacks (TypeScript and Python) with the same JSON output as command hooks; some events are TypeScript-only (`PostCompact`, `PreModelSwitch`, `SessionStart`/`SessionEnd`, `MessageDisplay`, `TaskCreated`, and more). [agent-sdk/hooks, "Available hooks"] `settings` populates the flag-settings layer and can be changed at runtime with `applyFlagSettings()`.
- **System prompt.** `systemPrompt` as a string, a string array with `SYSTEM_PROMPT_DYNAMIC_BOUNDARY` (static prefix cached, tail dynamic), or `{type:'preset', preset:'claude_code', append, excludeDynamicSections, snapshot}`. Default when omitted is a minimal prompt, not Claude Code's. [agent-sdk/typescript, Options]
- **Tools.** `tools` (restrict built-ins), `allowedTools` (auto-approve only), `disallowedTools` (deny).
- **Costs.** No official Rust SDK (crates.io has community crates such as `claude-agent-sdk-rs`; not vetted here). m4code would need a Node or Python host and its own IPC to Rust, which is the same Node dependency as ACP with a bespoke protocol instead of a standard one.
- **Policy note.** The SDK overview states: "Unless previously approved, Anthropic does not allow third party developers to offer claude.ai login or rate limits for their products, including agents built on the Claude Agent SDK. Use the API key authentication methods." Claudiu sidesteps this by pointing the SDK at the user's own `claude` (`CLAUDE_CODE_EXECUTABLE`) and holding no credentials, but whether that is inside the rule is a question for the product owner, not settled here. The same page restricts branding: "Claude Code" or "Claude Code Agent" is "not permitted" as a product name.

### 3. CLI `-p` with `--input-format stream-json --output-format stream-json`, spawned straight from Rust

- **Events.** Same `SDKMessage` lines as the SDK, newline-delimited. `--include-partial-messages` adds token streaming; `--include-hook-events` adds hook lifecycle; `--replay-user-messages` echoes input. Documented event types in `headless.md` include `system/init`, `system/api_retry`, `permission_denied`, and the `result` message. [cli-reference, headless]
- **Approvals.** Documented: `--permission-prompt-tool mcp__<server>__<tool>`, an MCP tool m4code would host. Undocumented as a CLI contract: `--permission-prompt-tool=stdio` plus `control_request`/`control_response` over stdin and stdout, which is what the SDK uses. Only the SDK reference mentions `control_request` (in `agent-sdk/typescript`); `headless.md` and `cli-reference.md` do not. Treat the control protocol as versioned with the SDK, not as a stable CLI API.
- **Resume and fork.** `--resume <id|name|path>`, `--continue`, `--fork-session`, `--session-id <uuid>`, `--no-session-persistence`. [cli-reference] `--resume-session-at` is passed by the SDK but is not in the CLI reference table.
- **`--settings`.** "Path to a settings JSON file or an inline JSON string"; overrides same keys from settings files; 2 MiB cap; `--setting-sources` limits which files load. [cli-reference] `--plugin-dir` also loads hooks (Claudiu already does this with `mod/`, `src/app.rs:704`).
- **System prompt.** `--append-system-prompt`, `--append-system-prompt-file`, `--system-prompt`, `--system-prompt-file`, `--exclude-dynamic-system-prompt-sections`, `--system-prompt-snapshot off`; the two append forms can be combined from v2.1.283. [cli-reference]
- **Tools.** `--tools` ("restrict which built-in tools Claude can use"), `--allowedTools` (run without prompting), `--disallowedTools` (deny; a bare name also removes the tool from Claude's context), `--agents`, `--mcp-config`, `--strict-mcp-config`.
- **Budget and limits.** `--max-budget-usd`, `--max-turns`, `--json-schema`. Available in all three because the SDK maps them to the same flags.
- **Costs.** No Node, no adapter. m4code must write what the adapter already provides: tool-call rendering and diffs, permission option shaping, history replay, session index, plan/todo extraction, and queueing semantics.

## Cross-cutting answers (apply to all three)

### Hooks: `PreToolUse` `updatedInput`, `PostToolUse` `updatedToolOutput`

- `PreToolUse` `hookSpecificOutput.updatedInput` "replaces the entire input object, so include unchanged fields". Permission rules are evaluated "against the input your hook returns, not the input Claude sent". Combine with `"allow"` or `"ask"`; ignored with `"defer"`. [hooks.md, "PreToolUse decision control"] `defer` needs `-p`.
- `PostToolUse` `updatedToolOutput` "replaces the tool's output with the provided value before it is sent to Claude. The value must match the tool's output shape"; `updatedMCPToolOutput` is the older MCP-only field. It "only changes what Claude sees": the tool has already run and OpenTelemetry/analytics capture the original. [hooks.md, "PostToolUse decision control"]
- Hook types: `command`, `http`, `mcp_tool`, `prompt`, `agent`. An `http` hook POSTs the event JSON to a URL and reads the same JSON back, which lets a Rust process answer hooks without spawning a script per call. [hooks.md, "HTTP hook fields"]
- Hooks from settings files and plugins also fire inside sub-agents, with `agent_id` and `agent_type` in the input. [hooks.md, line ~255]
- **Do the adapter's own hooks collide with m4code's?** Checked in source. The adapter's `PostToolUse` hook (`createPostToolUseHook`, `tools.js:294`) and its task hooks return only `{ continue: true }`; none of its hooks returns `updatedInput`, `updatedToolOutput` or `permissionDecision`. Its `PreToolUse` Edit|Write preview hook (`fileChangePreviewHook`) is registered only when `this.v2` is true (`acp-agent.js:8532`, set from `options.v2`, default false); Claudiu speaks v1, so it is not registered there, and even on v2 it returns `{ continue: true }` and "makes no permission decision". The only `updatedInput` in the adapter is in its `canUseTool` result for elicitation answers (`acp-agent.js:7769`). So a settings-borne rewrite hook from m4code is the only rewriter. Hooks run in parallel on the original output and a competing rewrite is last-write-wins [agent-sdk types, `classifierContext` note], so m4code should keep a single rewriting hook per event. The adapter's PostToolUse callback sees the original `tool_response`, so the Chat UI shows the unredacted output while the model gets the replacement.
- **Can a repo switch the flag-layer hooks off?** `disableAllHooks` is read "after settings precedence applies", the `--settings` layer beats project and local files, and the docs show `--settings '{"disableAllHooks": true}'` overriding a project. Reversed: a project `.claude/settings.json` with `disableAllHooks: true` would kill m4code's hooks unless the flag layer says otherwise, because user and project levels merge by precedence while only the flag/managed layer outranks them. m4code should set `disableAllHooks: false` explicitly in its `settings`. Managed settings still win: `disableAllHooks` set outside managed settings "can't disable managed hooks", `allowManagedHooksOnly: true` leaves only managed hooks plus "Agent SDK hooks and hooks from plugins your managed settings force-enable", and `allowedHttpHookUrls`, if defined anywhere, restricts which `http` hook URLs may run. On a managed machine, m4code's JSON hooks may therefore not run at all; the in-process SDK callback path is exempt from `allowManagedHooksOnly`, which ACP cannot use. [hooks.md "Disable or remove hooks", settings-reference.md]
- **Fail-open by default.** A hook that cannot start, times out, exits with a code other than 0 or 2, or returns invalid output is a non-blocking error unless the hook sets `onFailure: "block"` (v2.1.295+). A policy hook that crashes therefore does not enforce anything by default. [hooks.md, "Block the action when a hook fails"]
- `if` filters are "best-effort" and the docs say to "use the permission system rather than a hook to enforce a hard allow or deny". `/skillname` typed directly bypasses `PreToolUse` (it fires `UserPromptExpansion`).

### Does a `PreToolUse` deny still apply under bypass permissions?

**Per the docs, yes.** Evaluation order is hooks, then deny rules, ask rules, then the permission mode, then allow rules, then `canUseTool`. A hook can "deny the call outright", and "a hook that returns `allow` does not skip the deny and ask rules". For bypass: "Hooks still execute and can block operations if needed", and "Deny rules, explicit `ask` rules, and hooks are evaluated before the mode check and can still block a tool." [agent-sdk/permissions, "How permissions are evaluated" and "Bypass permissions mode"] Exit code 2 blocks even if the hook also prints JSON with `allow`. [hooks.md, "Exit code 2"] Deny rules (`disallowedTools` / settings) also block in bypass, and `allowed_tools` "does not constrain `bypassPermissions`".

**Qualification: an installed mod can override it.** `permissions.md` ("Precedence among rules, hooks, and mods") says a mod that handles the `tool.check` event answers after the `PreToolUse` hooks have run. With managed settings on the machine, or a Team/Enterprise `/login` as the session credential, a deny rule, ask rule or blocked `PreToolUse` call stays in force when a mod approves. "With neither managed settings nor a Team or Enterprise `/login`... the mod can approve a call that a deny rule refuses or a `PreToolUse` hook blocked." A session on an API key or a long-lived `CLAUDE_CODE_OAUTH_TOKEN` does not count as that login. So on a personal login without managed settings, "a hook deny holds under bypass" is true only while no installed mod handles `tool.check`. Claudiu's own mod (`mod/hooks/register.js`) registers `tool.call`, `turn.step` and `session.start`, not `tool.check`, so it does not do this, but a user-installed mod could. m4code cannot assume the hook layer is final on a personal account; another reason the OS sandbox has to carry the guarantee.

Limits that matter for "refuse, don't prompt":
- Hooks and deny rules match tool-call text and arguments. The docs point to OS sandboxing for enforcement "that doesn't depend on the command text" [permissions.md, line ~262]. The OS sandbox ticket owns the hard guarantee; hooks are the policy and refusal-message layer.
- Bypass still prompts for ask rules, tools needing user interaction, and `rm`/`rmdir` of critical paths (a hook `allow` cannot approve those). A refusal flow must not assume bypass means no prompts at all.
- `rm`-style decisions aside, `bypassPermissions` needs `allowDangerouslySkipPermissions` and is refused as root outside a sandbox; `permissions.disableBypassPermissionsMode: "disable"` turns it off. The adapter clamps to `default` in those cases (`permissions/modes.js`, `acp-agent.js:8360`).

### `--append-system-prompt` and caching

- By default the system prompt is built on the first request, recorded in the session, and reused on `--resume`/`--continue` "until the conversation is compacted". Different flag text on a later launch "takes effect once the conversation is compacted or when you start a new conversation". `--system-prompt-snapshot off` rebuilds every request. [cli-reference, "System prompt flags in resumed conversations"; prompt-caching, "Resuming a session"] Consequence for Claudiu today: a changed `ui::PROMPT` does not reach an existing chat until it compacts (documented behaviour, not tested here). For m4code's "numbers in the volatile tail, never the cached prefix" rule, the documented splits are `__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__` (CLI, v2.1.275+; custom replacement prompts only) and `excludeDynamicSections`.
- Cache-invalidating actions the harness must avoid mid-task: model switch, effort change, fast mode, connecting or removing an MCP server, toggling a plugin, a bare-name deny rule (when tool search is unavailable), compaction. [prompt-caching.md]
- TTL: on a subscription within plan usage, Claude Code requests the 1-hour TTL by itself for the main conversation (`-p` and SDK turns included); sub-agents, forks, compaction and titles default to 5 minutes. `CLAUDE_CODE_PROMPT_CACHE_TTL` (`5m`/`1h`, v2.1.242+) and `CLAUDE_CODE_SUBAGENT_PROMPT_CACHE_TTL` override. With usage credits or an API key the default is 5 minutes. These are environment variables, so every surface can set them (the adapter passes `env` through). [prompt-caching "Which TTL each request gets"; env-vars.md]
- Sharing: an in-process fork "inherits the parent's system prompt, tools, and conversation history exactly, so its first request reads the parent's cache"; a `/fork` session copy keeps the original's cache intact; a separate sub-agent "doesn't read the parent's cache". [prompt-caching, "Subagents and the cache"] Whether a programmatic `--fork-session` / SDK `forkSession` / ACP `session/fork` first request hits the parent's cache is **not stated** in these sources.

## What `src/chat/` covers today

Read from `src/chat/acp.rs`, `src/chat/view.rs`, `src/chat/model.rs`, `src/mcp.rs`.

| Area | State |
|---|---|
| Transport | Hand-rolled JSON-RPC over the adapter's stdio, one adapter per chat; `initialize` with `protocolVersion: 1` and `fs` read/write false, no `terminal`, no `elicitation`, no `subagents` capability |
| Session | `session/new` or `session/load` (resume by Claude session id); `cwd`, `mcpServers` (the `claudiu` MCP server), `systemPrompt {mode: append}`, `_meta.claudeCode.options.allowedTools` |
| Turn | `session/prompt`, `session/cancel`, `session/set_config_option` (model, mode, effort); image blocks |
| Rendering | user/agent/thought chunks, `tool_call` + `tool_call_update` (title, kind, status, output only; `rawInput` and diff content not kept), `plan`, `available_commands_update`, `config_option_update`, `current_mode_update`, `session_info_update` |
| Usage | `usage_update` `used`/`size` and `_meta["_claude/rateLimit"].unifiedWindows` (five-hour and seven-day). `cost`, `_claude/model`, `PromptResponse.usage` and `_meta.quota` are ignored |
| Approvals | `session/request_permission` shown to the user; other agent requests rejected |
| Not used | `session/list`, `fork`, `resume`, `close`, `delete`; `_meta.claudeCode.options.settings` (so no hooks), `disallowedTools`, `tools`, `env`; `emitRawSDKMessages`; sub-agent grouping by `parentToolUseId`; elicitation (so `AskUserQuestion` is disabled) |
| Code tab | Separate path: PTY + `--settings` (status-line tee), `--mcp-config`, `--allowedTools`, `--plugin-dir mod/`, `--resume`/`--session-id` (`src/app.rs`, `src/claude.rs`); the PTY side goes away in m4code |

## Capability matrix

`Y` = supported and documented or read in source; `Y*` = supported through a non-standard extension; `~` = partial; `N` = not available on that surface; `?` = not established.

| Capability | ACP (adapter 0.94.0) | Agent SDK (TS/Py host) | CLI stream-json from Rust |
|---|---|---|---|
| Extra processes besides `claude` | node adapter (+ SDK inside it) | node/python host | none |
| Normalized tool calls, diffs, permission options, history replay | Y (standard) | N (raw blocks, you render) | N (raw blocks, you render) |
| Full raw Claude event stream (task_*, hook_*, compact, api_retry) | Y* (`_claude/sdkMessage`, opt-in) | Y | Y |
| Sub-agent attribution | ~ (`_meta.claudeCode.parentToolUseId`; native sessions only with draft `subagents` capability) | Y (`parent_tool_use_id`, task_* messages) | Y (same stream) |
| Context size, cost, rate-limit windows | Y (`usage_update`, `_meta`) | Y (`result`, `rate_limit_event`, `get_context_usage`) | Y (`result`, `rate_limit_event`) |
| Per-turn token split incl. cache read/write | Y* (adapter `PromptResponse.usage`; ACP RFD still Draft) | Y (`result.usage`, `modelUsage`) | Y (same) |
| Interactive approvals | Y (`session/request_permission`) | Y (`canUseTool`, `PermissionRequest` hook) | ~ (`--permission-prompt-tool` MCP tool documented; `stdio` control protocol undocumented) |
| Resume | Y (stable `load`, `resume`) | Y | Y |
| Fork | ~ (`session/fork` unstable; adapter implements it) | Y (`forkSession`) | Y (`--fork-session`) |
| Session list, close, delete | Y (stable) | Y (helpers) | ~ (files on disk; `--resume` picker) |
| `--settings`-equivalent (flag layer) | Y* (`_meta.claudeCode.options.settings`) | Y (`settings`) | Y (`--settings`) |
| Hooks as JSON (`command`, `http`, `mcp_tool`) incl. `updatedInput` / `updatedToolOutput` | Y (via `settings`; adapter's own hooks return no rewrites, checked in source) | Y | Y |
| Hooks as in-process callbacks | N (cannot cross JSON-RPC) | Y | ~ (only by speaking `hook_callback` control protocol yourself) |
| `PreToolUse` deny under bypass | Y per docs, unless a `tool.check` mod overrides on a personal login; not run | same | same |
| Append to system prompt | Y (`systemPrompt` unstable RFD; `_meta.systemPrompt`) | Y (`systemPrompt` preset `append`) | Y (`--append-system-prompt[-file]`) |
| Replace system prompt, dynamic-boundary caching | Y* (`override` mode; `_meta.systemPrompt` preset options) | Y (string[], boundary) | Y (`--system-prompt`, boundary line) |
| Tool allow, deny, restrict | Y* (`_meta.claudeCode.options.{allowedTools, disallowedTools, tools}`) | Y (`allowedTools`, `disallowedTools`, `tools`) | Y (`--allowedTools`, `--disallowedTools`, `--tools`) |
| Budget and turn caps | Y* (`maxBudgetUsd`, `maxTurns` options) | Y | Y (`--max-budget-usd`, `--max-turns`) |
| Cache TTL control | Y (env) | Y (env) | Y (env) |
| Harness-neutral for a future Codex/OpenCode adapter | Y (the point of ACP; adapters for other harnesses not checked) | N | N |

## Recommendation

1. **Keep ACP as the v1 boundary, behind the harness trait.** It is already built and covers the normalized layer (tool calls, diffs, approvals, replay, session list) that a CLI-direct design would force m4code to rewrite, and it is the only one of the three that is harness-neutral. Claude-specific power goes through `_meta.claudeCode.options` and, where needed, `_claude/sdkMessage`, kept inside the Claude adapter implementation, not in the trait.
2. **Deliver the context firewall and refusals as JSON hooks, not callbacks.** Pass them in `_meta.claudeCode.options.settings` (flag-settings layer, so no edits to the user's own settings files). Prefer `type: "http"` hooks to a local m4code listener, or a `command` hook that runs the `claudiu` binary (the existing `--mcp-title` mode shows the pattern), with `onFailure: "block"` so a dead policy process fails closed. In-process callbacks are unavailable over ACP; that is acceptable because every needed decision (`deny`, `updatedInput`, `updatedToolOutput`) has a JSON form.
3. **Treat hooks as policy and refusal text; get the hard guarantee from the OS sandbox.** The docs themselves say text-matching hooks are not the enforcement layer, a `tool.check` mod can override them on a personal login, and managed settings can switch them off. Set `disableAllHooks: false` in the flag-layer `settings`.
4. **Use the raw stream-json CLI as the planned fallback, not the start.** Adopt it only if the spike below shows the Node chain is unreliable (the `.cmd` shim issue flagged in `src/chat/acp.rs`, orphaned `claude` children on Windows, adapter lag behind Claude Code releases). Its cost is owning normalization and, for in-process approval, an undocumented control protocol.
5. **Rewrite the `AGENTS.md` principle** from "run the real `claude` in a PTY" to "drive the real `claude` through a structured adapter; never reimplement its agent loop, auth or tools". All three surfaces still run the user's own `claude` binary, so that part of the principle survives.
6. **Resolve the policy question** (SDK overview note on claude.ai login and rate limits, and the naming restriction) before shipping, since it applies to both ACP and SDK routes.

### Spike to turn "per the docs" into "confirmed" (go/no-go inputs)

Run through the real adapter on Windows and macOS with `settings` carrying hooks and the session in `bypassPermissions`:

1. `PreToolUse` `deny` on a Bash command and on Edit/Write: blocked, and the reason reaches the model.
2. `PreToolUse` `updatedInput` rewrites a path; `PostToolUse` `updatedToolOutput` redacts a Read result; the transcript on disk shows the original while the model saw the replacement.
3. A hook that crashes or times out: confirm fail-open by default and fail-closed with `onFailure: "block"`.
4. An `http` hook round-trip latency per tool call.
5. Fork a session via `session/fork` and via `--fork-session`, then read `cache_read_input_tokens` on the first request to see whether the parent's cache is hit.
6. Resume with a changed `systemPrompt` and confirm the recorded prompt is kept until compaction.
7. Kill the app mid-turn on Windows and look for orphaned `claude` processes.
8. With a project `.claude/settings.json` that sets `disableAllHooks: true`, confirm m4code's flag-layer hooks still run when it sets `false`, and that a `tool.check` mod can or cannot approve a hook-denied call on a personal login.

## Confirmed, documented-only, and unverified

**Confirmed by reading primary source (code or docs):**
- The SDK spawns `claude` with `--input-format stream-json --output-format stream-json --verbose`, and with `--permission-prompt-tool=stdio` for `canUseTool` (`sdk.mjs`).
- The adapter forwards `_meta.claudeCode.options` (including `settings`, `allowedTools`, `disallowedTools`, `tools`, `env`, `extraArgs`) into the SDK options, cannot forward `hooks` callbacks, and offers `emitRawSDKMessages` (`acp-agent.js`).
- The adapter advertises `fork`, `list`, `resume`, `close`, `delete`, `systemPrompt`; fork is `unstable_forkSession`.
- The adapter reports cost, per-turn cache read and write tokens, the model, and rate-limit windows.
- `updatedInput`, `updatedToolOutput`, the hook types, `--settings`, the append and replace system-prompt flags, `--tools` / `--allowedTools` / `--disallowedTools` exist as described in the current docs (2.1.x).
- The client-provided `systemPrompt` that `src/chat/view.rs` sends is an unstable ACP feature (RFD in Draft).
- `src/chat/` does not use hooks, settings, fork, list, cost, or sub-agent grouping.

**Documented but not run by this research:**
- `PreToolUse` deny (and exit code 2) applying under `bypassPermissions`. A nested `claude --dangerously-skip-permissions` run was attempted to test it; this environment's permission classifier refused it before it ran, and it was not retried by other means. The qualification above (a `tool.check` mod on a personal login) is documented, also not run.
- `updatedInput` and `updatedToolOutput` behaviour end to end, including through the ACP adapter.
- Hook `onFailure` semantics and the fail-open default.
- Whether a project `disableAllHooks: true` is really outranked by the flag layer when it arrives through the adapter's `settings` option (docs say yes for `--settings`; the SDK `settings` option is documented as populating the same flag-settings layer).
- System-prompt recording across resume.

**Unverified or not established:**
- Whether a programmatic fork (CLI flag, SDK, ACP) shares the parent's prompt cache on its first request.
- Stability of the `control_request` protocol for a Rust client speaking it directly; it is not documented for CLI users.
- Existence and quality of ACP adapters for Codex or OpenCode (outside this ticket).
- Whether Anthropic's third-party-login rule covers the user's-own-`claude` design.
- Windows process-tree cleanup of the adapter's `claude` child (a `shortcut:` comment in `src/chat/acp.rs` already flags it).
- Community Rust SDK crates (not evaluated).

## Sources

Local primary sources (read as installed, versions above):
- `%LOCALAPPDATA%\Claudiu\data\acp\node_modules\@agentclientprotocol\claude-agent-acp\dist\` (`acp-agent.js`, `client-system-prompt.js`, `fork-session.js`, `acp-subagents.js`, `permissions\modes.js`, `package.json`)
- `%LOCALAPPDATA%\Claudiu\data\acp\node_modules\@anthropic-ai\claude-agent-sdk\` (`sdk.d.ts`, `sdk.mjs`, `package.json`)
- Repo: `src/chat/acp.rs`, `src/chat/view.rs`, `src/chat/model.rs`, `src/mcp.rs`, `src/app.rs`, `src/claude.rs`, `src/chat/adapter/package.json`

Claude Code docs (fetched 2026-10-21 as Markdown):
- https://code.claude.com/docs/en/hooks.md
- https://code.claude.com/docs/en/permissions.md
- https://code.claude.com/docs/en/permission-modes.md
- https://code.claude.com/docs/en/cli-reference.md
- https://code.claude.com/docs/en/headless.md
- https://code.claude.com/docs/en/prompt-caching.md
- https://code.claude.com/docs/en/env-vars.md
- https://code.claude.com/docs/en/settings.md
- https://code.claude.com/docs/en/agent-sdk/overview.md
- https://code.claude.com/docs/en/agent-sdk/hooks.md
- https://code.claude.com/docs/en/agent-sdk/permissions.md
- https://code.claude.com/docs/en/agent-sdk/sessions.md
- https://code.claude.com/docs/en/agent-sdk/typescript.md
- https://code.claude.com/docs/en/agent-sdk/streaming-output.md

ACP docs and RFDs (fetched 2026-10-21):
- https://agentclientprotocol.com/llms.txt (index and stabilization announcements)
- https://agentclientprotocol.com/protocol/v1/tool-calls.md, `session-setup.md`, `prompt-turn.md`, `extensibility.md`
- https://agentclientprotocol.com/rfds/session-fork.md, `session-resume.md`, `session-usage.md`, `end-turn-token-usage.md`, `subagents.md`, `client-system-prompt.md`
- https://crates.io/api/v1/crates/agent-client-protocol (3.4.0, `agentclientprotocol/rust-sdk`)
