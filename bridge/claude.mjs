// Claude Code for m4code, through Anthropic's Claude Agent SDK. m4code speaks ACP (newline-delimited JSON-RPC on
// stdin and stdout) to every agent; this script answers it for Claude Code by driving the SDK, which runs the user's
// own `claude` (CLAUDE_CODE_EXECUTABLE) with their settings, plugins and sign-in.
//
// Only the part of ACP m4code uses is here: initialize, session/new, session/load, session/list, session/prompt,
// session/cancel, session/set_mode and session/set_config_option, and session/request_permission back to m4code.
// `_m4code/steer` is m4code's own: a message for the turn that is running, which ACP has no request for.
// stdout carries the protocol only, so anything logged goes to stderr.

import { randomUUID } from "node:crypto";
import { createInterface } from "node:readline";
import { BackgroundShells, liveOutput, readOutput } from "./background-shells.mjs";

import { getSessionMessages, listSessions, query } from "@anthropic-ai/claude-agent-sdk";

console.log = console.error;
console.info = console.error;

const send = (message) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);
const update = (sessionId, body) => send({ method: "session/update", params: { sessionId, update: body } });

function parseEpochSeconds(value) {
  if (typeof value === "number" && Number.isFinite(value) && value > 0) return Math.floor(value);
  if (typeof value === "string" && value) {
    const ms = Date.parse(value);
    if (Number.isFinite(ms) && ms > 0) return Math.floor(ms / 1000);
  }
  return null;
}

function clampFraction(value) {
  if (typeof value !== "number" || !Number.isFinite(value)) return null;
  return Math.max(0, Math.min(1, value));
}

/** Normalize the SDK's `get_usage` response (utilization is 0..100, resets_at is RFC3339). */
function usageFromControlResponse(response, seenAt = Math.floor(Date.now() / 1000)) {
  if (!response?.rate_limits_available || !response.rate_limits) return null;
  const pick = (w) => {
    if (!w || typeof w.utilization !== "number" || !Number.isFinite(w.utilization)) return null;
    return {
      utilization: clampFraction(w.utilization / 100),
      resets_at: parseEpochSeconds(w.resets_at),
      seen_at: seenAt,
    };
  };
  const five_hour = pick(response.rate_limits.five_hour);
  const seven_day = pick(response.rate_limits.seven_day);
  if (!five_hour && !seven_day) return null;
  return { five_hour, seven_day };
}

/** Normalize a streamed `rate_limit_event` (utilization is 0..1, resetsAt is epoch seconds). */
function usageFromRateLimitEvent(info, seenAt = Math.floor(Date.now() / 1000)) {
  if (!info || typeof info.utilization !== "number" || !Number.isFinite(info.utilization)) return null;
  const entry = {
    utilization: clampFraction(info.utilization),
    resets_at: parseEpochSeconds(info.resetsAt),
    seen_at: seenAt,
  };
  if (info.rateLimitType === "five_hour") return { five_hour: entry, seven_day: null };
  if (info.rateLimitType === "seven_day") return { five_hour: null, seven_day: entry };
  return null;
}

function queryUsage(q) {
  const fn = q?.usage_EXPERIMENTAL_MAY_CHANGE_DO_NOT_RELY_ON_THIS_API_YET;
  if (typeof fn !== "function") return Promise.resolve(null);
  return fn.call(q, { skipBehaviors: true }).catch(() => null);
}

/** Standalone zero-token usage probe (`node claude.mjs --probe-usage`), matching t3code's probe. */
async function runUsageProbe() {
  const abort = new AbortController();
  const timer = setTimeout(() => abort.abort(), 25_000);
  timer.unref?.();
  const waitAbort = () => new Promise((resolve) => {
    if (abort.signal.aborted) return resolve();
    abort.signal.addEventListener("abort", resolve, { once: true });
  });
  let q;
  try {
    q = query({
      prompt: (async function* () {
        await waitAbort();
      })(),
      options: {
        persistSession: false,
        pathToClaudeCodeExecutable: process.env.CLAUDE_CODE_EXECUTABLE || undefined,
        settingSources: ["user", "project", "local"],
        abortController: abort,
        stderr: () => {},
      },
    });
    await q.initializationResult();
    const res = await queryUsage(q);
    const limits = usageFromControlResponse(res);
    process.stdout.write(`${JSON.stringify({
      available: Boolean(res?.rate_limits_available),
      five_hour: limits?.five_hour ?? null,
      seven_day: limits?.seven_day ?? null,
    })}\n`);
  } catch (error) {
    process.stderr.write(`${error?.message ?? String(error)}\n`);
    process.exitCode = 1;
  } finally {
    clearTimeout(timer);
    if (!abort.signal.aborted) abort.abort();
    try { q?.close?.(); } catch {}
    process.exit(process.exitCode ?? 0);
  }
}

if (process.argv.includes("--probe-usage")) {
  await runUsageProbe();
}

// Requests to m4code (permission questions) and their answers.
let nextRequest = 1;
const answers = new Map();
function ask(method, params) {
  const id = `claude-${nextRequest++}`;
  send({ id, method, params });
  return new Promise((resolve) => answers.set(id, resolve));
}

/** What the user types, as the SDK reads it: an async stream that ends when the session closes. */
class Inbox {
  items = [];
  wake = null;
  closed = false;
  push(item) {
    this.items.push(item);
    this.wake?.();
  }
  close() {
    this.closed = true;
    this.wake?.();
  }
  async *[Symbol.asyncIterator]() {
    for (;;) {
      while (this.items.length) yield this.items.shift();
      if (this.closed) return;
      await new Promise((resolve) => (this.wake = resolve));
      this.wake = null;
    }
  }
}

const sessions = new Map();

const MODES = [
  { id: "default", name: "Ask Before Editing" },
  { id: "acceptEdits", name: "Accept Edits" },
  { id: "plan", name: "Plan Mode" },
  { id: "dontAsk", name: "Don't Ask" },
  { id: "bypassPermissions", name: "Full Access" },
];
const EFFORTS = ["low", "medium", "high", "xhigh", "max"];

const AUTO = { id: "auto", name: "Auto" };

/** Auto mode is listed only when Claude Code offers it: a model of this account supports it (the "default" entry
 * included), or the session already started in it. Without that, choosing it would fail. */
function modes(session) {
  const offersAuto = session.mode === AUTO.id || session.offersAuto;
  const available = offersAuto ? [MODES[0], MODES[1], AUTO, ...MODES.slice(2)] : MODES;
  return { currentModeId: session.mode, availableModes: available };
}

function configOptions(session) {
  const realModels = session.models.filter((m) => m.value !== "default");
  const sourceModels = realModels.length ? realModels : session.models;
  const models = sourceModels.map((m) => ({ value: m.value, name: m.displayName || m.value }));
  return [
    { id: "model", name: "Model", category: "model", type: "select", currentValue: session.model, options: models },
    {
      id: "effort",
      name: "Effort",
      category: "thought_level",
      type: "select",
      currentValue: session.effort,
      options: EFFORTS.map((e) => ({ value: e, name: e })),
    },
  ];
}

/** ACP's MCP servers (`env` as a list of names and values) as the SDK takes them. */
function mcpServers(list = []) {
  const servers = {};
  for (const s of list) {
    if (s.command) {
      servers[s.name] = { type: "stdio", command: s.command, args: s.args ?? [], env: Object.fromEntries((s.env ?? []).map((e) => [e.name, e.value])) };
    } else if (s.url) {
      servers[s.name] = { type: s.type === "sse" ? "sse" : "http", url: s.url, headers: Object.fromEntries((s.headers ?? []).map((h) => [h.name, h.value])) };
    }
  }
  return servers;
}

function kindOf(name) {
  if (name === "Read") return "read";
  if (["Edit", "MultiEdit", "Write", "NotebookEdit"].includes(name)) return "edit";
  if (["Bash", "PowerShell"].includes(name)) return "execute";
  if (["Grep", "Glob", "LSP"].includes(name)) return "search";
  if (["WebFetch", "WebSearch"].includes(name)) return "fetch";
  if (["Agent", "Task"].includes(name)) return "think";
  if (["ExitPlanMode", "EnterPlanMode"].includes(name)) return "switch_mode";
  return "other";
}

function titleOf(name, input = {}) {
  const path = input.file_path ?? input.notebook_path ?? input.path;
  switch (name) {
    case "Bash":
    case "PowerShell":
      return input.description || input.command || name;
    case "Read":
    case "Edit":
    case "MultiEdit":
    case "Write":
    case "NotebookEdit":
      return path ? `${name} ${path}` : name;
    case "Grep":
    case "Glob":
      return input.pattern ? `${name} ${input.pattern}` : name;
    case "WebFetch":
      return input.url ? `Fetch ${input.url}` : name;
    case "WebSearch":
      return input.query ? `Search ${input.query}` : name;
    case "Agent":
    case "Task":
      return input.description || name;
  }
  // mcp__server__tool reads as "server: tool".
  const mcp = /^mcp__(.+?)__(.+)$/.exec(name);
  return mcp ? `${mcp[1]}: ${mcp[2]}` : name;
}

function toolCall(block, status) {
  const path = block.input?.file_path ?? block.input?.notebook_path;
  const offset = block.name === "Read" ? block.input?.offset : undefined;
  const limit = block.input?.limit;
  const endLine = Number.isSafeInteger(offset) && offset > 0 && Number.isSafeInteger(limit) && limit > 0
    ? offset + limit - 1 : undefined;
  const mcpServer = /^mcp__(.+?)__/.exec(block.name)?.[1];
  return {
    sessionUpdate: "tool_call",
    toolCallId: block.id,
    title: titleOf(block.name, block.input),
    kind: kindOf(block.name),
    status,
    rawInput: block.input,
    content: [],
    locations: path ? [{ path, ...(Number.isSafeInteger(offset) && offset > 0 ? { line: offset } : {}) }] : [],
    _meta: { m4code: { endLine, mcpServer } },
  };
}

function resultText(content) {
  if (typeof content === "string") return content;
  return (content ?? []).filter((c) => c.type === "text").map((c) => c.text).join("\n");
}

/** The todo list Claude keeps (TodoWrite) is the session's plan. */
function plan(input) {
  return { sessionUpdate: "plan", entries: (input?.todos ?? []).map((t) => ({ content: t.content, status: t.status, priority: "medium" })) };
}

/** One message of the conversation as updates: what the user said, the reply, its thinking and its tools. */
function* updatesOf(session, role, content) {
  const blocks = typeof content === "string" ? [{ type: "text", text: content }] : (content ?? []);
  for (const b of blocks) {
    if (b.type === "text" && b.text) yield { sessionUpdate: role === "user" ? "user_message_chunk" : "agent_message_chunk", content: { type: "text", text: b.text } };
    else if (b.type === "thinking" && b.thinking) yield { sessionUpdate: "agent_thought_chunk", content: { type: "text", text: b.thinking } };
    else if (b.type === "tool_use") {
      session.tools.set(b.id, { ...b, startedAt: performance.now() });
      yield b.name === "TodoWrite" ? plan(b.input) : toolCall(b, "in_progress");
    } else if (b.type === "tool_result") {
      const call = session.tools.get(b.tool_use_id);
      if (call?.name === "TodoWrite") continue;
      const failed = b.is_error === true;
      const contents = [{ type: "content", content: { type: "text", text: resultText(b.content) } }];
      if (Array.isArray(b.content)) {
        for (const part of b.content) {
          if (part.type === "image" && part.source?.type === "base64") {
            contents.push({ type: "content", content: { type: "image", mimeType: part.source.media_type, data: part.source.data } });
          }
        }
      }
      yield {
        sessionUpdate: "tool_call_update",
        toolCallId: b.tool_use_id,
        status: failed ? "failed" : "completed",
        ...(call ? { _meta: { m4code: { elapsedMs: Math.round(performance.now() - call.startedAt) } } } : {}),
        content: contents,
      };
    }
  }
}

async function permission(session, name, input, options) {
  if (name === "AskUserQuestion") return askQuestions(session, input, options);
  const id = options.toolUseID ?? randomUUID();
  const choices = [{ optionId: "allow", name: "Yes", kind: "allow_once" }];
  if (options.suggestions?.length) choices.push({ optionId: "always", name: "Yes, and don't ask again", kind: "allow_always" });
  choices.push({ optionId: "reject", name: "No", kind: "reject_once" });
  const title = options.title ?? titleOf(name, input);
  const answer = await ask("session/request_permission", {
    sessionId: session.id,
    toolCall: { toolCallId: id, title, kind: kindOf(name), status: "pending", rawInput: input },
    options: choices,
  });
  const chosen = answer?.outcome?.outcome === "selected" ? answer.outcome.optionId : null;
  if (chosen === "allow" || chosen === "always") {
    return { behavior: "allow", updatedInput: input, ...(chosen === "always" ? { updatedPermissions: options.suggestions } : {}) };
  }
  return { behavior: "deny", message: "The user declined this.", interrupt: chosen === null };
}

/**
 * Claude's questions to the user, one at a time, each with its options. ACP has no free-text or multi-select answer,
 * so `_meta.m4code` advertises `freeText` and `multiSelect`, and m4code sends back `_meta.m4code.text` and/or
 * `_meta.m4code.optionIds`.
 */
async function askQuestions(session, input, options) {
  const answers = {};
  for (const q of input?.questions ?? []) {
    const multiSelect = Boolean(q.multiSelect);
    const meta = { freeText: true, ...(multiSelect ? { multiSelect: true } : {}) };
    const choices = q.options.map((o, i) => ({
      optionId: String(i),
      name: o.label,
      ...(o.description ? { description: o.description } : {}),
      kind: "allow_once",
    }));
    const answer = await ask("session/request_permission", {
      _meta: { m4code: meta },
      sessionId: session.id,
      toolCall: { toolCallId: `${options.toolUseID}-${q.question}`, title: q.question, kind: "other", status: "pending" },
      options: choices,
    });
    const chosen = answer?.outcome?.outcome === "selected" ? answer.outcome.optionId : null;
    if (chosen === null) return { behavior: "deny", message: "The user dismissed the question.", interrupt: true };
    const own = typeof answer?._meta?.m4code?.text === "string" ? answer._meta.m4code.text.trim() : "";
    const selectedIds = answer?._meta?.m4code?.optionIds;
    if (Array.isArray(selectedIds)) {
      const parts = selectedIds.map((id) => q.options[Number(id)]?.label).filter(Boolean);
      if (own) parts.push(own);
      if (parts.length) answers[q.question] = parts.join(", ");
    } else if (own) {
      answers[q.question] = own;
    } else if (chosen !== "skip" && q.options[Number(chosen)]) {
      answers[q.question] = q.options[Number(chosen)].label;
    }
  }
  return { behavior: "allow", updatedInput: { ...input, answers } };
}

function stopReason(session, result) {
  if (session.cancelled) return "cancelled";
  if (result.subtype === "error_max_turns") return "max_turn_requests";
  if (result.stop_reason === "max_tokens") return "max_tokens";
  if (result.stop_reason === "refusal") return "refusal";
  return "end_turn";
}

/** Per-turn token and prompt-cache counts in ACP's `PromptResponse.usage` shape. */
function turnUsage(result) {
  const u = result?.usage;
  if (!u || typeof u !== "object") return undefined;
  const uncachedInput = Number(u.input_tokens ?? 0);
  const output = Number(u.output_tokens ?? 0);
  const cachedRead = Number(u.cache_read_input_tokens ?? 0);
  const cachedWrite = Number(u.cache_creation_input_tokens ?? 0);
  const input = uncachedInput + cachedRead + cachedWrite;
  const total = input + output;
  if (!total) return undefined;
  return { inputTokens: input, outputTokens: output, cachedReadTokens: cachedRead, cachedWriteTokens: cachedWrite, totalTokens: total };
}

/** The uuids of the messages a result answered: every one Claude took into that turn. */
function answeredBy(result) {
  if (Array.isArray(result.user_message_uuids)) return result.user_message_uuids;
  return result.user_message_uuid ? [result.user_message_uuid] : null;
}

/**
 * A result ends the turn m4code sees once it has answered every message sent in it. A steer Claude takes between tool
 * calls is answered by the same result; one it takes after the turn gets a result of its own, so the turn stays open
 * for it. A Claude Code too old to say which messages a result answered ends the turn at its first result, as before.
 */
async function finishTurn(session, result) {
  const turn = session.turn;
  if (turn && !session.cancelled && !result.is_error) {
    const answered = answeredBy(result);
    // A Claude Code that never said it started a steer has answered it by now, so it shows before the turn ends.
    showWaitingSteers(session, answered);
    if (answered) for (const uuid of answered) turn.waiting.delete(uuid);
    else turn.waiting.clear();
    if (turn.waiting.size) {
      turn.held = result;
      return;
    }
  }
  session.turn = null;
  // The receipt lists the messages Claude Code still means to run, which is how `unreadSteers` tells a steer it
  // dropped from one it already answered. An older Claude Code sends none, and its steers are left to run.
  const receipt = session.cancelled ? await session.interrupted : undefined;
  if (turn) {
    const failed = result.is_error && !session.cancelled;
    if (failed) turn.reject(new Error(result.result || (result.errors ?? []).join("\n") || "Claude Code stopped with an error."));
    else {
      if (session.cancelled) {
        for (const uuid of receipt?.still_queued ?? []) {
          if (turn.steers.has(uuid) && !turn.shown.has(uuid)) session.dropping.add(uuid);
        }
      }
      const usage = turnUsage(result);
      turn.resolve({ stopReason: stopReason(session, result), ...(usage ? { usage } : {}) });
    }
  }
  // The context size and usage limits are only shown, so the turn ends without waiting for them.
  session.query
    .getContextUsage()
    .then((usage) => update(session.id, { sessionUpdate: "usage_update", used: usage.totalTokens, size: usage.maxTokens }))
    .catch(() => {});
  queryUsage(session.query).then((res) => {
    const limits = usageFromControlResponse(res);
    if (limits) update(session.id, { sessionUpdate: "_m4code/usage_limits", limits });
  });
}

/** Claude Code says it took a steer off its queue without reading it (the states t3code treats the same way). */
function steerDropped(session, m) {
  const turn = session.turn;
  if (!turn || session.cancelled || !["cancelled", "discarded", "refused"].includes(m.state) || !turn.steers.has(m.command_uuid)) return;
  if (!turn.waiting.delete(m.command_uuid)) return;
  turn.steers.delete(m.command_uuid);
  if (turn.waiting.size === 0 && turn.held) return finishTurn(session, turn.held);
}

function publishShells(session) {
  const shells = [...session.shells.active.values()].map(({ id, label }) => ({ id, label }));
  update(session.id, { sessionUpdate: "_m4code/background_shells", shells });
}

async function finishShell(session, message) {
  const shell = session.shells.finish(message);
  if (!shell?.backgrounded) return;
  publishShells(session);
  const output = await readOutput(message.output_file);
  update(session.id, {
    sessionUpdate: "tool_call",
    toolCallId: `background-shell-${shell.id}`,
    kind: "execute",
    _meta: { m4code: { backgroundShell: true } },
    title: shell.label,
    status: message.status === "completed" ? "completed" : "failed",
    content: [{ type: "content", content: { type: "text", text: [message.summary, output].filter(Boolean).join("\n\n") } }],
  });
}

/** The session's messages from the SDK, as updates to m4code, until the session ends. */
async function pump(session) {
  let errorText;
  try {
    for await (const m of session.query) {
      if (m.type === "system") {
        if (session.shells.update(m)) publishShells(session);
        if (m.subtype === "task_notification") await finishShell(session, m);
      }
      if (session.dropping.size && !session.turn) {
        if (m.type === "result") {
          for (const uuid of answeredBy(m) ?? [...session.dropping]) session.dropping.delete(uuid);
          session.dropInterrupted = false;
        } else if (m.type === "command_lifecycle" && ["cancelled", "discarded", "refused"].includes(m.state)) {
          session.dropping.delete(m.command_uuid);
        } else if (!session.dropInterrupted) {
          session.dropInterrupted = true;
          session.query.interrupt().catch(() => {});
        }
        if (!session.dropping.size) session.dropsDone?.();
        continue;
      }
      if (m.type === "rate_limit_event") {
        const limits = usageFromRateLimitEvent(m.rate_limit_info);
        if (limits) update(session.id, { sessionUpdate: "_m4code/usage_limits", limits });
        continue;
      }
      // Not in the SDK's types yet; t3code reads it the same way.
      if (m.type === "command_lifecycle") {
        if (m.state === "started") showSteer(session, m.command_uuid);
        await steerDropped(session, m);
        continue;
      }
      // A subagent's own steps stay inside its tool call.
      if (m.parent_tool_use_id) continue;
      if (m.type === "stream_event") {
        const ev = m.event;
        if (ev.type === "message_start") {
          session.streamed.add(ev.message.id);
          session.speaking = ev.message.id;
          // After a result the turn stayed open for, what Claude writes next answers the oldest steer it still
          // owed; a later one may still be waiting, so it shows when Claude starts or answers it.
          const turn = session.turn;
          if (turn?.held) {
            turn.held = null;
            const oldest = [...turn.waiting].find((uuid) => turn.steers.has(uuid) && !turn.shown.has(uuid));
            if (oldest) showSteer(session, oldest);
          }
        }
        // The message id keeps a message whole when a steer shows while Claude is still writing it.
        if (ev.type === "content_block_delta" && ev.delta.type === "text_delta") update(session.id, { sessionUpdate: "agent_message_chunk", messageId: session.speaking, content: { type: "text", text: ev.delta.text } });
        if (ev.type === "content_block_delta" && ev.delta.type === "thinking_delta") update(session.id, { sessionUpdate: "agent_thought_chunk", content: { type: "text", text: ev.delta.thinking } });
      } else if (m.type === "assistant") {
        // Text already came in pieces when the message streamed; its tools come whole here.
        const streamed = session.streamed.has(m.message.id);
        const blocks = (m.message.content ?? []).filter((b) => b.type === "tool_use" || !streamed);
        for (const u of updatesOf(session, "assistant", blocks)) update(session.id, u);
      } else if (m.type === "user") {
        const blocks = Array.isArray(m.message.content) ? m.message.content.filter((b) => b.type === "tool_result") : [];
        for (const u of updatesOf(session, "user", blocks)) update(session.id, u);
      } else if (m.type === "system" && m.subtype === "init") {
        if (m.permissionMode && m.permissionMode !== session.mode) {
          session.mode = m.permissionMode;
          update(session.id, { sessionUpdate: "current_mode_update", currentModeId: session.mode });
        }
      } else if (m.type === "result") {
        await finishTurn(session, m);
      }
    }
  } catch (error) {
    errorText = error?.message ?? String(error);
    session.turn?.reject(error);
    session.turn = null;
  }
  // Claude Code ended with a turn still open, such as one held for a steer it never answered.
  session.turn?.reject(new Error("Claude Code stopped before it finished the turn."));
  session.turn = null;
  sessions.delete(session.id);
  update(session.id, { sessionUpdate: "_m4code/session_ended", error: errorText });
}

async function open(params, resume) {
  const id = resume ?? randomUUID();
  const extra = params._meta?.claudeCode?.options ?? {};
  const session = { id, cwd: params.cwd, inbox: new Inbox(), tools: new Map(), streamed: new Set(), speaking: undefined, shells: new BackgroundShells(), turn: null, cancelled: false, interrupted: null, dropping: new Set(), dropInterrupted: false, dropsDone: null, mode: extra.permissionMode ?? "default", model: "", effort: "medium", models: [] };
  session.query = query({
    prompt: session.inbox,
    options: {
      ...extra,
      cwd: params.cwd,
      pathToClaudeCodeExecutable: process.env.CLAUDE_CODE_EXECUTABLE || undefined,
      mcpServers: mcpServers(params.mcpServers),
      includePartialMessages: true,
      // Lets the user pick Bypass permissions later; it starts in the user's own default mode.
      allowDangerouslySkipPermissions: true,
      canUseTool: (name, input, options) => permission(session, name, input, options),
      stderr: (text) => process.stderr.write(text),
      ...(resume ? { resume: id } : { sessionId: id }),
    },
  });
  sessions.set(id, session);
  let init;
  try {
    init = await session.query.initializationResult();
  } catch (error) {
    sessions.delete(id);
    session.query.close();
    throw error;
  }
  const rawModels = init.models ?? [];
  session.offersAuto = rawModels.some((m) => m.supportsAutoMode);
  const realModels = rawModels.filter((m) => m.value !== "default");
  session.models = realModels.length ? realModels : rawModels;
  if (!session.models.some((m) => m.value === session.model) && session.models.length) session.model = session.models[0].value;
  update(id, { sessionUpdate: "available_commands_update", availableCommands: (init.commands ?? []).map((c) => ({ name: c.name, description: c.description, input: c.argumentHint ? { hint: c.argumentHint } : null })) });
  queryUsage(session.query).then((res) => {
    const limits = usageFromControlResponse(res);
    if (limits) update(id, { sessionUpdate: "_m4code/usage_limits", limits });
  });
  pump(session);
  return session;
}

/** The summary Claude Code writes when it compacts a conversation comes back as a user message, and the SDK's
 * messages carry no mark for it, so its fixed opening is the only way to tell. */
const COMPACT_SUMMARY = "This session is being continued from a previous conversation";
const LOCAL_COMMAND_TAG = /^<(?:command-name|command-message|command-args|local-command-stdout|local-command-stderr|local-command-caveat)\b/;

function isCompactSummary(m) {
  return m.type === "user" && typeof m.message?.content === "string" && m.message.content.startsWith(COMPACT_SUMMARY);
}

function isLocalCommandMessage(m) {
  if (m.type !== "user") return false;
  const text = resultText(m.message?.content).trimStart();
  return LOCAL_COMMAND_TAG.test(text);
}

async function replay(session) {
  let messages = await getSessionMessages(session.id, { dir: session.cwd });
  if (!messages.length) {
    // A session that moved via EnterWorktree may store its transcript under the original checkout's project dir.
    messages = await getSessionMessages(session.id);
  }
  for (const m of messages) {
    if ((m.type !== "user" && m.type !== "assistant") || isCompactSummary(m) || isLocalCommandMessage(m)) continue;
    // A tool's result arrives as a user message; it is not something the user said.
    for (const u of updatesOf(session, m.type, m.message?.content)) update(session.id, u);
  }
}

const handlers = {
  initialize: async () => ({
    protocolVersion: 1,
    agentCapabilities: { loadSession: true, promptCapabilities: { image: false, embeddedContext: false }, mcpCapabilities: { http: true, sse: true }, sessionCapabilities: { list: {} } },
    authMethods: [],
  }),
  "session/new": async (p) => {
    const s = await open(p, null);
    return { sessionId: s.id, modes: modes(s), configOptions: configOptions(s) };
  },
  "session/load": async (p) => {
    const s = await open(p, p.sessionId);
    await replay(s);
    return { modes: modes(s), configOptions: configOptions(s) };
  },
  "session/list": async (p) => {
    const found = await listSessions({ dir: p.cwd });
    return { sessions: found.map((s) => ({ sessionId: s.sessionId, cwd: p.cwd, title: s.customTitle || s.summary, updatedAt: new Date(s.lastModified).toISOString() })) };
  },
  "session/prompt": async (p) => {
    const s = sessions.get(p.sessionId);
    if (!s) throw new Error("That session has ended.");
    if (s.turn || s.preparing) throw new Error("A turn is already running.");
    s.cancelled = false;
    s.preparing = true;
    // Steers the user stopped still run in Claude Code first; this message waits until they are gone, so their
    // output and result can't be taken for its own.
    await afterDropped(s);
    s.preparing = false;
    if (s.cancelled) return { stopReason: "cancelled" };
    const text = (p.prompt ?? []).filter((c) => c.type === "text").map((c) => c.text).join("\n");
    const uuid = randomUUID();
    const done = new Promise((resolve, reject) => (s.turn = { resolve, reject, waiting: new Set([uuid]), steers: new Map(), shown: new Set(), held: null }));
    s.inbox.push(userMessage(s, text, uuid));
    return done;
  },
  "_m4code/steer": async (p) => {
    const s = sessions.get(p.sessionId);
    if (!s) throw new Error("That session has ended.");
    if (!s.turn || s.cancelled) throw new Error("No turn is running.");
    const uuid = p.id;
    s.turn.waiting.add(uuid);
    s.turn.steers.set(uuid, p.text);
    // "next" waits for the tool calls running to finish; "now" would cancel them, as t3code found.
    s.inbox.push(userMessage(s, p.text, uuid, "next"));
    return {};
  },
  "_m4code/background_shell": async (p) => {
    const session = sessions.get(p.sessionId);
    if (!session?.shells.active.has(p.taskId)) throw new Error("That background shell has ended.");
    if (p.action === "stop") {
      await session.query.stopTask(p.taskId);
      return {};
    }
    return { output: await liveOutput(session.cwd, session.id, p.taskId) };
  },
  "session/set_mode": async (p) => {
    const s = sessions.get(p.sessionId);
    await s.query.setPermissionMode(p.modeId);
    s.mode = p.modeId;
    update(s.id, { sessionUpdate: "current_mode_update", currentModeId: s.mode });
    return {};
  },
  "session/set_config_option": async (p) => {
    const s = sessions.get(p.sessionId);
    if (p.configId === "model") {
      await s.query.setModel(p.value === "default" ? undefined : p.value);
      s.model = p.value;
    } else if (p.configId === "effort") {
      await s.query.applyFlagSettings({ effortLevel: p.value === "default" ? null : p.value });
      s.effort = p.value;
      if (s.mode && s.mode !== "default") {
        await s.query.setPermissionMode(s.mode);
      }
    } else {
      throw new Error(`No setting called ${p.configId}.`);
    }
    return { configOptions: configOptions(s) };
  },
};

/**
 * Resolves once the stopped steers (`dropping`) have run and been stopped, or after 10 s: Claude Code's receipt said
 * it would run them, and one that hasn't started by then is taken as gone, so a new message never waits forever.
 */
async function afterDropped(session) {
  if (session.dropping.size) {
    await new Promise((resolve) => {
      session.dropsDone = resolve;
      setTimeout(resolve, 10_000);
    });
  }
  session.dropsDone = null;
  session.dropping.clear();
  session.dropInterrupted = false;
}

function userMessage(session, text, uuid, priority) {
  return { type: "user", uuid, ...(priority ? { priority } : {}), message: { role: "user", content: [{ type: "text", text }] }, parent_tool_use_id: null, session_id: session.id, origin: { kind: "human" } };
}

/**
 * Show a steer in the conversation once Claude reads it, not when it was sent: what Claude was still writing then
 * comes first. m4code knows it by `_meta.m4code.steer` and takes it off the list of steers waiting to be read.
 */
function showSteer(session, uuid) {
  const turn = session.turn;
  const text = turn?.steers.get(uuid);
  if (text === undefined || turn.shown.has(uuid)) return;
  turn.shown.add(uuid);
  update(session.id, { sessionUpdate: "user_message_chunk", content: { type: "text", text }, _meta: { m4code: { steer: uuid } } });
}

/** Every steer not shown yet, oldest first: Claude is about to answer them, or just did. */
function showWaitingSteers(session, only) {
  for (const uuid of session.turn?.steers.keys() ?? []) if (!only || only.includes(uuid)) showSteer(session, uuid);
}

function cancel(p) {
  const s = sessions.get(p.sessionId);
  if (!s) return;
  if (s.turn || s.preparing) {
    s.cancelled = true;
    if (!s.turn) return;
    s.interrupted = s.query.interrupt().catch(() => undefined);
    return;
  }
  // Background continuation (e.g. after a subagent or background shell woke Claude up with no open session/prompt).
  s.query.interrupt().catch(() => undefined);
}

async function handle(m) {
  if (m.method === undefined) {
    answers.get(m.id)?.(m.result);
    answers.delete(m.id);
    return;
  }
  if (m.method === "session/cancel") return cancel(m.params ?? {});
  if (m.id === undefined) return;
  const handler = handlers[m.method];
  if (!handler) return send({ id: m.id, error: { code: -32601, message: `${m.method} is not supported` } });
  try {
    send({ id: m.id, result: await handler(m.params ?? {}) });
  } catch (error) {
    send({ id: m.id, error: { code: -32603, message: error?.message ?? String(error) } });
  }
}

const lines = createInterface({ input: process.stdin });
lines.on("line", (line) => {
  if (!line.trim()) return;
  let message;
  try {
    message = JSON.parse(line);
  } catch {
    return;
  }
  handle(message);
});
// m4code closing stdin ends every session and then this process.
lines.on("close", () => {
  for (const s of sessions.values()) {
    s.inbox.close();
    s.query.close();
  }
  process.exit(0);
});
