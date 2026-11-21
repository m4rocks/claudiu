// Claude Code for Claudiu, through Anthropic's Claude Agent SDK. Claudiu speaks ACP (newline-delimited JSON-RPC on
// stdin and stdout) to every agent; this script answers it for Claude Code by driving the SDK, which runs the user's
// own `claude` (CLAUDE_CODE_EXECUTABLE) with their settings, plugins and sign-in.
//
// Only the part of ACP Claudiu uses is here: initialize, session/new, session/load, session/list, session/prompt,
// session/cancel, session/set_mode and session/set_config_option, and session/request_permission back to Claudiu.
// stdout carries the protocol only, so anything logged goes to stderr.

import { randomUUID } from "node:crypto";
import { createInterface } from "node:readline";

import { getSessionMessages, listSessions, query } from "@anthropic-ai/claude-agent-sdk";

console.log = console.error;
console.info = console.error;

const send = (message) => process.stdout.write(`${JSON.stringify({ jsonrpc: "2.0", ...message })}\n`);
const update = (sessionId, body) => send({ method: "session/update", params: { sessionId, update: body } });

// Requests to Claudiu (permission questions) and their answers.
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
  { id: "default", name: "Default" },
  { id: "acceptEdits", name: "Accept edits" },
  { id: "plan", name: "Plan mode" },
];
const BYPASS = { id: "bypassPermissions", name: "Bypass permissions" };
const EFFORTS = ["low", "medium", "high", "xhigh", "max"];

/** The modes to cycle through. Bypass is listed only while it is on, so Shift+Tab never lands on it. */
function modes(session) {
  const available = session.mode === BYPASS.id ? [...MODES, BYPASS] : MODES;
  return { currentModeId: session.mode, availableModes: available };
}

function configOptions(session) {
  const models = session.models.map((m) => ({ value: m.value, name: m.displayName || m.value }));
  return [
    { id: "model", name: "Model", category: "model", type: "select", currentValue: session.model, options: models },
    {
      id: "effort",
      name: "Effort",
      category: "thought_level",
      type: "select",
      currentValue: session.effort,
      options: [{ value: "default", name: "Default" }, ...EFFORTS.map((e) => ({ value: e, name: e }))],
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

/** An edit shows as a diff of what it changes. */
function diffOf(name, input = {}) {
  if (name === "Edit" && input.file_path) return [{ type: "diff", path: input.file_path, oldText: input.old_string ?? "", newText: input.new_string ?? "" }];
  if (name === "Write" && input.file_path) return [{ type: "diff", path: input.file_path, oldText: null, newText: input.content ?? "" }];
  return [];
}

function toolCall(block, status) {
  const path = block.input?.file_path ?? block.input?.notebook_path;
  return {
    sessionUpdate: "tool_call",
    toolCallId: block.id,
    title: titleOf(block.name, block.input),
    kind: kindOf(block.name),
    status,
    rawInput: block.input,
    content: diffOf(block.name, block.input),
    locations: path ? [{ path }] : [],
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
      session.tools.set(b.id, b);
      yield b.name === "TodoWrite" ? plan(b.input) : toolCall(b, "in_progress");
    } else if (b.type === "tool_result") {
      const call = session.tools.get(b.tool_use_id);
      if (call?.name === "TodoWrite") continue;
      const failed = b.is_error === true;
      // An edit keeps its diff; its result text only says it succeeded.
      const keepsDiff = !failed && call && diffOf(call.name, call.input).length;
      yield {
        sessionUpdate: "tool_call_update",
        toolCallId: b.tool_use_id,
        status: failed ? "failed" : "completed",
        ...(keepsDiff ? {} : { content: [{ type: "content", content: { type: "text", text: resultText(b.content) } }] }),
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

/** Claude's questions to the user, one at a time, each with its options. A multiple-choice question takes one answer. */
async function askQuestions(session, input, options) {
  const answers = {};
  for (const q of input?.questions ?? []) {
    const answer = await ask("session/request_permission", {
      sessionId: session.id,
      toolCall: { toolCallId: `${options.toolUseID}-${q.question}`, title: q.question, kind: "other", status: "pending" },
      options: [...q.options.map((o, i) => ({ optionId: String(i), name: o.label, kind: "allow_once" })), { optionId: "skip", name: "Skip", kind: "reject_once" }],
    });
    const chosen = answer?.outcome?.outcome === "selected" ? answer.outcome.optionId : null;
    if (chosen === null) return { behavior: "deny", message: "The user dismissed the question.", interrupt: true };
    if (chosen !== "skip") answers[q.question] = q.options[Number(chosen)].label;
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

function finishTurn(session, result) {
  const turn = session.turn;
  session.turn = null;
  if (turn) {
    const failed = result.is_error && !session.cancelled;
    if (failed) turn.reject(new Error(result.result || (result.errors ?? []).join("\n") || "Claude Code stopped with an error."));
    else turn.resolve({ stopReason: stopReason(session, result) });
  }
  // The context size is only shown, so the turn ends without waiting for it.
  session.query
    .getContextUsage()
    .then((usage) => update(session.id, { sessionUpdate: "usage_update", used: usage.totalTokens, size: usage.maxTokens }))
    .catch(() => {});
}

/** The session's messages from the SDK, as updates to Claudiu, until the session ends. */
async function pump(session) {
  try {
    for await (const m of session.query) {
      // A subagent's own steps stay inside its tool call.
      if (m.parent_tool_use_id) continue;
      if (m.type === "stream_event") {
        const ev = m.event;
        if (ev.type === "message_start") session.streamed.add(ev.message.id);
        if (ev.type === "content_block_delta" && ev.delta.type === "text_delta") update(session.id, { sessionUpdate: "agent_message_chunk", content: { type: "text", text: ev.delta.text } });
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
        finishTurn(session, m);
      }
    }
  } catch (error) {
    session.turn?.reject(error);
    session.turn = null;
  }
  sessions.delete(session.id);
}

async function open(params, resume) {
  const id = resume ?? randomUUID();
  const extra = params._meta?.claudeCode?.options ?? {};
  const session = { id, cwd: params.cwd, inbox: new Inbox(), tools: new Map(), streamed: new Set(), turn: null, cancelled: false, mode: "default", model: "default", effort: "default", models: [] };
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
  session.models = init.models ?? [];
  if (!session.models.some((m) => m.value === session.model) && session.models.length) session.model = session.models[0].value;
  update(id, { sessionUpdate: "available_commands_update", availableCommands: (init.commands ?? []).map((c) => ({ name: c.name, description: c.description, input: c.argumentHint ? { hint: c.argumentHint } : null })) });
  pump(session);
  return session;
}

async function replay(session) {
  const messages = await getSessionMessages(session.id, { dir: session.cwd });
  for (const m of messages) {
    if (m.type !== "user" && m.type !== "assistant") continue;
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
    if (s.turn) throw new Error("A turn is already running.");
    s.cancelled = false;
    const text = (p.prompt ?? []).filter((c) => c.type === "text").map((c) => c.text).join("\n");
    const done = new Promise((resolve, reject) => (s.turn = { resolve, reject }));
    s.inbox.push({ type: "user", message: { role: "user", content: [{ type: "text", text }] }, parent_tool_use_id: null, session_id: s.id, origin: { kind: "human" } });
    return done;
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
    } else {
      throw new Error(`No setting called ${p.configId}.`);
    }
    return { configOptions: configOptions(s) };
  },
};

function cancel(p) {
  const s = sessions.get(p.sessionId);
  if (!s?.turn) return;
  s.cancelled = true;
  s.query.interrupt().catch(() => {});
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
// Claudiu closing stdin ends every session and then this process.
lines.on("close", () => {
  for (const s of sessions.values()) {
    s.inbox.close();
    s.query.close();
  }
  process.exit(0);
});
