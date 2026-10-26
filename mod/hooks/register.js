// Loaded per process through `--plugin-dir`. Claudiu writes nothing to the user's Claude Code config.
//
// What this mod does:
// - Model and effort: answers Claudiu's `set_model` tool in-process, so the override applies from the next request,
//   even mid-turn. It then runs /model and /effort once Claude Code is idle, so the session (and what /model shows)
//   switches too. Claude Code saves those as the user's default.
// - Session name: applies the name Claudiu writes to `CLAUDIU_NAME_FILE` with /rename, which Claude Code queues
//   until the session is idle.
// - Sidebar settings: applies auto-compact, background compaction and advisor changes sent through
//   `CLAUDIU_CTL_FILE`, the same as the user choosing them in /config or typing /advisor (which Claude Code saves).
// - Messages: sends a message Claudiu asks for through the same file (the editor's Ask Claude) as the user's own words,
//   when the session is idle. Nothing is typed into the terminal.
// - Usage and status: reports context, the 5-hour and 7-day limits, auto-compact, background compaction, advisor and
//   the last command to `CLAUDIU_USAGE_DIR/<claude session id>.json`. No tokens are spent.
// - Visual panel: answers `show_visual` in-process and, while the model is still writing the call, streams its
//   Markdown to `CLAUDIU_VISUAL_FILE` so the panel fills live.
// - One-shot completions: runs requests from `CLAUDIU_ASK_FILE` (commit messages, session titles) with
//   `$.model.complete`, on this session's own client, so Claudiu needs no `claude -p`.
// - Background compaction, where Claude Code does not offer its own: with Claudiu's setting on (`CLAUDIU_BG_COMPACT`,
//   then the ctl file), drafts the compaction summary ahead of time and swaps it in at the automatic compaction.

// ---- Constants ----------------------------------------------------------------------------------------------------

// The aliases `set_model` accepts. A full `claude-…` id is accepted too.
const MODELS = {
  haiku: 'claude-haiku-5-5',
  sonnet: 'claude-sonnet-5-5',
  opus: 'claude-opus-5-5',
  fable: 'claude-fable-5-1',
}
const PROMPT_MAX = 20000 // characters of a message Claudiu asks the session to send (the editor's Ask Claude)
const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max']
const ASK_MODELS = ['haiku', 'sonnet', 'opus']
const VISUAL_TOOL = 'mcp__claudiu__show_visual'
// The /config row for background compaction ("Precompute compaction"). Claude Code lists it only while a server-side
// flag enables the feature; without the row the setting does nothing, so Claudiu offers the switch only with it.
const PRECOMPUTE_ROW = 'precomputeCompactionEnabled'
// Claudiu's own background compaction drafts once the context passes this share of the auto-compact threshold, and
// throws the draft away when more than DRAFT_TAIL of the threshold arrived after it: compacting would then shrink too
// little, and the engine's own compaction does better.
const DRAFT_AT = 0.85
const DRAFT_TAIL = 0.35
const DRAFT_TRIES = 2 // drafts that may fail between two compactions before the fallback waits for the next one
const DRAFT_PROMPT = `This conversation is about to be compacted: everything above will be replaced by your summary, \
and work continues from the summary alone. Write that summary now. Do not call any tools.

Cover, in this order:
1. What the user asked for and why, quoting them where the wording matters.
2. Decisions made and the reasons for them.
3. Files, functions and commands involved, with paths and the key code or output.
4. Errors met and how they were resolved.
5. What is done, what is in progress, and the next step.

Be complete about facts and brief in wording. Reply with the summary only.`
const DRAFT_HEADER = 'This session continues an earlier conversation that ran out of context. The summary below covers ' +
  'the earlier part; the messages after it are kept as they were.\n\n'

// ---- Per-process state --------------------------------------------------------------------------------------------

let want = {} // { model?, effort? } from Claudiu, applied to each turn step until Claude Code has run /model and /effort
let named = null // `${claude session id}\n${name}` last applied with /rename
let namePoll // watches CLAUDIU_NAME_FILE
let ctlPoll // watches CLAUDIU_CTL_FILE
let ctlSeen = null // highest command number handled; null until the file was first read (older ones are not replayed)
let pendingAdvisor = null // an advisor change Claude Code refused (see setAdvisor), tried again until it goes through
let askPoll // watches CLAUDIU_ASK_FILE
let askSeen = null // id of the last request looked at
let asking = false // a completion is running; a poll that finds another request waits for it
let visualChain = Promise.resolve() // visual writes in order, without holding up the stream
let lastVisual = null // id of the show_visual call being written, or last written

// ---- Usage report -------------------------------------------------------------------------------------------------

let measured = null // { context, limits: { five_hour, seven_day } } from the last session.measure
let written = '' // what the report file holds, minus times; an unchanged report is not rewritten (a write is cheap, a redraw is not)
let activeAdvisor = null // the advisor the current conversation declared; a model change waits for /clear or /compact
let activeSession = null // the Claude session id activeAdvisor belongs to (/clear starts a new one)

// The token count auto-compaction runs at (null while it is off). It only changes with settings, the model or the
// environment, and the breakdown it comes from is a local estimate, so it is read again at most once a minute or
// after a setting changed.
let compactAt = null
let compactRead = 0
let compactedAt = null // when this session last compacted (seconds), so Claudiu can finish a hand-off

// Background compaction fallback.
let bgCompact = null // Claudiu's setting; null until the first session.start reads the one it launched with
let draft = null // { session, model, prints, tokens, summary }: a summary of the first prints.length messages
let drafting = false
let draftTries = 0 // since the last compaction

// A session id as a file name.
const stem = id => String(id).replace(/[^A-Za-z0-9_-]/g, '_')

// An ISO time as whole seconds, or null when it does not parse.
function secs(iso) {
  const t = Date.parse(iso)
  return Number.isFinite(t) ? Math.floor(t / 1000) : null
}

async function threshold($, fresh) {
  if (!fresh && Date.now() - compactRead < 60_000) return compactAt
  compactRead = Date.now()
  const usage = await $.session.usage({ breakdown: 'summary' }).catch(() => null)
  const b = usage?.context.breakdown
  if (b) compactAt = b.isAutoCompactEnabled ? (b.autoCompactThreshold ?? null) : null
  return compactAt
}

// Reads one setting through the plugin API. The call normally returns the value itself; older hosts may hand back
// the settings object, so both shapes are accepted.
async function setting($, key) {
  const v = await $.settings.read({ key }).catch(() => undefined)
  if (typeof v === 'string' || typeof v === 'boolean') return v
  if (v && typeof v === 'object' && key in v) return v[key]
  return undefined
}

// The menu speaks aliases; settings may hold a resolved id such as `claude-opus-5-5`. Returns null when the advisor
// is off.
function advisorAlias(value) {
  if (typeof value !== 'string') return null
  const v = value.trim()
  if (!v || v.toLowerCase() === 'off') return null
  const lower = v.toLowerCase()
  if (lower === 'opus' || lower.includes('opus')) return 'opus'
  if (lower === 'sonnet' || lower.includes('sonnet')) return 'sonnet'
  return v
}

// Writes this session's usage and status report for Claudiu's sidebar. `fresh` re-reads the compaction threshold now.
// `override` carries a value Claudiu just asked for, so the sidebar updates before the host has refreshed its cache.
// Every read is best effort: a failed one is left out, and reporting never fails a hook.
async function report($, fresh = false, override = {}) {
  try {
    const dir = await $.env.get('CLAUDIU_USAGE_DIR')
    if (!dir) return
    const id = await $.session.id()
    if (!id) return

    const ctx = measured?.context
    const fraction = ctx?.tokens != null && ctx?.window > 0 ? Math.min(1, ctx.tokens / ctx.window) : null

    const rows = await $.config.list().catch(() => [])
    let autoCompact = rows.find(r => r.key === 'autoCompact')?.value
    if (typeof autoCompact !== 'boolean') {
      const fromSettings = await setting($, 'autoCompactEnabled')
      autoCompact = typeof fromSettings === 'boolean' ? fromSettings : null
    }
    const precompute = rows.find(r => r.key === PRECOMPUTE_ROW)?.value
    let precomputeCompact = typeof precompute === 'boolean' ? precompute : null
    let advisor = advisorAlias(await setting($, 'advisorModel'))
    if ('auto_compact' in override) autoCompact = override.auto_compact
    if ('precompute_compact' in override) precomputeCompact = override.precompute_compact
    if ('advisor' in override) advisor = override.advisor

    // A different model only reaches the running conversation on /clear or /compact. Off and on take effect at once,
    // so a null or newly named advisor replaces the active one; a model-to-model change keeps the old one.
    if (activeSession !== id || advisor == null || activeAdvisor == null) {
      activeSession = id
      activeAdvisor = advisor
    }

    const snap = {
      session_id: id,
      five_hour: measured?.limits.five_hour ?? null,
      seven_day: measured?.limits.seven_day ?? null,
      context_fraction: fraction,
      context_tokens: ctx?.tokens ?? null,
      context_window: ctx?.window ?? null,
      model: (await $.session.model().catch(() => null)) ?? null,
      account: (await $.env.get('CLAUDIU_ACCOUNT')) ?? null,
      auto_compact: autoCompact,
      precompute_compact: precomputeCompact,
      compact_at: await threshold($, fresh),
      advisor,
      advisor_active: activeAdvisor,
      compacted_at: compactedAt,
    }
    // `seen_at` (when a limit was read) is left out, so reading the same value again is not a change.
    const key = JSON.stringify(snap, (k, v) => (k === 'seen_at' ? undefined : v))
    if (key === written) return
    written = key

    const path = dir + '/' + stem(id) + '.json'
    await $.fs.write(path, JSON.stringify({ ...snap, captured_at: Math.floor(Date.now() / 1000) }))
  } catch {
    // Reporting must never disturb the session.
  }
}

// ---- Sidebar commands ---------------------------------------------------------------------------------------------

// Applies what Claudiu's sidebar asked for since the last look. The file holds the latest few `{ seq, key, value }`
// commands; only known keys and plainly shaped values are acted on.
async function applyCtl($) {
  try {
    const file = await $.env.get('CLAUDIU_CTL_FILE')
    if (!file) return
    const cmds = JSON.parse(await $.fs.read(file).catch(() => '{}')).cmds ?? []
    if (ctlSeen === null) {
      // First look: what is already in the file was sent before this session started, so it is not replayed.
      ctlSeen = cmds.reduce((top, c) => Math.max(top, c.seq), 0)
      return
    }
    const override = {}
    if (pendingAdvisor !== null) await setAdvisor($, pendingAdvisor, override)
    const pending = cmds.filter(c => c.seq > ctlSeen).sort((a, b) => a.seq - b.seq)
    for (const c of pending) {
      ctlSeen = c.seq
      await applyCtlCommand($, c, override)
    }
    if (Object.keys(override).length > 0) await report($, true, override)
  } catch {
    // A bad file must not disturb the session.
  }
}

async function applyCtlCommand($, c, override) {
  if (c.key === 'auto_compact' && typeof c.value === 'boolean') {
    await $.config.set({ key: 'autoCompact', value: c.value }).catch(() => {})
    override.auto_compact = c.value
  } else if (c.key === 'precompute_compact' && typeof c.value === 'boolean') {
    // Fails while Claude Code does not list the row; the report then keeps saying it is not offered.
    const set = await $.config.set({ key: PRECOMPUTE_ROW, value: c.value }).then(() => true, () => false)
    if (set) override.precompute_compact = c.value
  } else if (c.key === 'background_compact' && typeof c.value === 'boolean') {
    bgCompact = c.value
    if (!bgCompact) draft = null
  } else if (c.key === 'prompt' && typeof c.value === 'string' && c.value.trim() !== '' && c.value.length <= PROMPT_MAX) {
    // A turn of its own once the session is idle, which can be a whole turn away: not awaited, so the commands
    // behind it in the file are not held up.
    void $.prompt.submit({ text: c.value, asUser: true }).catch(() => {})
  } else if (c.key === 'compact' && c.value === true) {
    await compactForHandoff($)
  } else if (c.key === 'advisor' && typeof c.value === 'string' && /^[A-Za-z0-9._-]{1,60}$/.test(c.value)) {
    await setAdvisor($, c.value, override)
  }
}

// Claude Code refuses a plugin's slash command after the person stopped a turn, until they type again
// ("refused: the session was stopped and nobody has sent input since"). That change is kept and tried again on the
// next look, and the new advisor is only reported once it went through. Any other failure drops the change.
async function setAdvisor($, value, override) {
  pendingAdvisor = null
  try {
    await $.command.run({ command: 'advisor', args: value })
  } catch (e) {
    if (String(e?.message ?? e).includes('refused')) pendingAdvisor = value
    return
  }
  override.advisor = advisorAlias(value)
}

// Claudiu's hand-off waits for this compaction. Between turns it compacts at once: `/compact` from a plugin would be
// refused there after the person stopped the last turn, until they type again. During a turn the direct call is
// refused, and `/compact` waits for the turn to end; the session.compact hook reports that one.
async function compactForHandoff($) {
  const result = await $.session.compact().catch(() => null)
  if (result === null) {
    await $.command.run({ command: 'compact' }).catch(() => {})
  } else if (!('skip' in result)) {
    // This plugin's own hooks don't see its own call.
    draft = null
    draftTries = 0
    await noteCompaction($)
  }
}

// ---- Visual panel -------------------------------------------------------------------------------------------------

// The text of a string property in JSON that may still be being written. It can end anywhere, even inside an escape.
function partialString(json, key) {
  const m = new RegExp('"' + key + '"\\s*:\\s*"').exec(json)
  if (!m) return null
  const ESCAPES = { n: '\n', t: '\t', r: '\r', b: '\b', f: '\f' }
  let out = ''
  let i = m.index + m[0].length
  while (i < json.length) {
    const ch = json[i]
    if (ch === '"') break
    if (ch !== '\\') {
      out += ch
      i += 1
      continue
    }
    const n = json[i + 1]
    if (n === undefined) break
    if (n === 'u') {
      const hex = json.slice(i + 2, i + 6)
      if (!/^[0-9a-fA-F]{4}$/.test(hex)) break
      out += String.fromCharCode(parseInt(hex, 16))
      i += 6
    } else {
      out += ESCAPES[n] ?? n
      i += 2
    }
  }
  return out
}

// Queues a write of the Visual panel's file. Writes run one after another, and a failed one is dropped.
function writeVisual($, file, call, title, content, done) {
  const body = JSON.stringify({
    call,
    title: String(title ?? '').slice(0, 80),
    content: String(content ?? '').slice(0, 200_000),
    done,
    at: Date.now(),
  })
  visualChain = visualChain.then(() => $.fs.write(file, body)).catch(() => {})
}

// ---- One-shot completions -----------------------------------------------------------------------------------------

// The request file holds `{ id, model, system, prompt, deadline }`. The answer goes next to it as `<name>.res.json`:
// `working` as soon as the request is picked up, then `done` with the text, or `error`.
async function answerAsk($) {
  if (asking) return
  let reply
  let id
  try {
    const file = await $.env.get('CLAUDIU_ASK_FILE')
    if (!file) return
    const text = await $.fs.read(file).catch(() => '')
    if (!text) return
    const req = JSON.parse(text)
    if (typeof req.id !== 'string' || req.id === askSeen) return
    askSeen = req.id
    id = req.id
    if (!(req.deadline > Date.now() / 1000)) return // nobody is waiting any more

    asking = true
    reply = file.replace(/\.json$/, '.res.json')
    if (typeof req.prompt !== 'string' || !req.prompt.trim()) {
      await $.fs.write(reply, JSON.stringify({ id, state: 'error', error: 'The request had no prompt.' }))
      return
    }
    await $.fs.write(reply, JSON.stringify({ id, state: 'working' }))

    const r = await $.model.complete({
      model: ASK_MODELS.includes(req.model) ? req.model : 'haiku',
      system: typeof req.system === 'string' ? req.system.slice(0, 4000) : undefined,
      prompt: req.prompt.slice(0, 120_000),
      maxTokens: 1024,
      timeoutMs: 90_000,
    })
    const out = r.isAnswered
      ? { id, state: 'done', text: r.text }
      : { id, state: 'error', error: 'The model call failed (' + r.reason + (r.status ? ' ' + r.status : '') + ').' }
    await $.fs.write(reply, JSON.stringify(out))
  } catch (err) {
    if (reply) {
      const error = 'The model call failed: ' + String(err?.message ?? err).slice(0, 200)
      await $.fs.write(reply, JSON.stringify({ id, state: 'error', error })).catch(() => {})
    }
  } finally {
    asking = false
  }
}

// ---- Background compaction ----------------------------------------------------------------------------------------

// What identifies a message across `$.session.messages()` and a compaction's input. Tool results are named by id
// alone: the engine may clear their text in place.
function fingerprint(m) {
  const uses = (m.toolUses ?? []).map(u => u.tool_use_id).join(',')
  const results = (m.toolResults ?? []).map(r => r.tool_use_id).join(',')
  return [m.role, uses, results, m.text ?? ''].join('\u0000')
}

// The context's size now. `turn.complete` comes before the turn's last response is measured, so the last
// session.measure can be a response behind.
async function contextTokens($) {
  const usage = await $.session.usage().catch(() => null)
  return usage?.context?.tokens ?? measured?.context?.tokens ?? null
}

// How many leading messages a fork summarises: all but the reply that ended the last turn, since the fork replays
// the request that reply answered.
function coveredCount(rows) {
  let n = rows.length
  while (n > 0 && rows[n - 1].role === 'assistant') n -= 1
  return n
}

// After a turn: draft a summary once the context nears the threshold, unless Claude Code lists its own setting.
// The fork runs from a timer, so the next turn neither waits for it nor aborts it.
async function onTurnComplete($, e, next) {
  const r = await next(e)
  if (e.agentId || !bgCompact || draft || drafting || draftTries >= DRAFT_TRIES) return r
  const tokens = await contextTokens($)
  const at = await threshold($, false)
  if (!(at > 0 && tokens >= at * DRAFT_AT && tokens < at)) return r
  const rows = (await $.config.list().catch(() => [])) ?? []
  if (rows.some(row => row.key === PRECOMPUTE_ROW)) return r

  const messages = await $.session.messages().catch(() => null)
  const count = Array.isArray(messages) ? coveredCount(messages) : 0
  if (count < 2) return r
  drafting = true
  draftTries += 1
  $.clock.after(0, () => writeDraft($, messages.slice(0, count), tokens).finally(() => { drafting = false }))
  return r
}

async function writeDraft($, covered, tokens) {
  try {
    const session = await $.session.id()
    const model = await $.session.model()
    const reply = await $.model.fork({ prompt: DRAFT_PROMPT })
    if (!reply.isAnswered || !reply.text.trim()) return
    if ((await $.session.id()) !== session || !bgCompact) return // /clear, or switched off, while it ran
    draft = { session, model, prints: covered.map(fingerprint), tokens, summary: reply.text.trim() }
  } catch {
    // A failed draft leaves the engine's own compaction to run.
  }
}

// The draft as the automatic compaction's result, or null to let the engine compact. The draft is used only when the
// conversation still starts with exactly what it summarised, under the same session and model, and what arrived
// after it is small; the messages after it keep their handles, so they stand as the engine's own.
async function swapInDraft($, e) {
  if (e.trigger !== 'auto' || e.agentId || !draft) return null
  const d = draft
  if ((await $.session.id().catch(() => null)) !== d.session) return null
  if ((await $.session.model().catch(() => null)) !== d.model) return null
  const n = d.prints.length
  if (e.messages.length < n || !d.prints.every((p, i) => p === fingerprint(e.messages[i]))) return null
  const first = e.messages[n]
  if (first && first.role === 'user' && first.toolResults?.length) return null // would answer a summarised tool use
  const at = compactAt ?? 0
  const now = (await contextTokens($)) ?? Infinity
  if (!(at > 0 && now - d.tokens <= at * DRAFT_TAIL)) return null
  const summary = { role: 'user', text: DRAFT_HEADER + d.summary, toolUses: [] }
  return { messages: [summary, ...e.messages.slice(n)] }
}

// ---- Session name -------------------------------------------------------------------------------------------------

// Applies the name Claudiu writes to CLAUDIU_NAME_FILE with /rename: once per name and Claude session.
async function watchSessionName($) {
  const file = await $.env.get('CLAUDIU_NAME_FILE')
  if (!file) return
  const read = async () => (await $.fs.read(file).catch(() => '')).trim()
  const key = async name => (await $.session.id()) + '\n' + name

  // What the file holds now is the name the session was launched with (`--name`): don't re-run it.
  const initial = await read()
  named = initial ? await key(initial) : null
  namePoll?.cancel()
  namePoll = $.clock.every(1000, async () => {
    const name = await read()
    if (!name) return
    const k = await key(name) // a new Claude session id (/clear) gets the name again
    if (k === named) return
    named = k
    await $.command.run({ command: 'rename', args: name }).catch(() => { named = null })
  })
}

// ---- Hook handlers ------------------------------------------------------------------------------------------------

async function onMeasure($, e, next) {
  const seen = Math.floor(Date.now() / 1000)
  const limit = kind => {
    const r = e.rateLimits.find(l => l.kind === kind)
    if (!r) return null
    return { utilization: Math.min(1, Math.max(0, r.percentUsed / 100)), resets_at: secs(r.resetsAt), seen_at: seen }
  }
  measured = { context: e.context, limits: { five_hour: limit('five_hour'), seven_day: limit('seven_day') } }
  const r = await next(e)
  await report($)
  return r
}

async function onCommandRun($, e, next) {
  const r = await next(e) // the command has run: a setting it changed is read after it
  await report($, true)
  return r
}

async function onConfigSet($, e, next) {
  const r = await next(e)
  await report($, true)
  return r
}

// Compaction rebuilds the request, so the saved advisor becomes the one this conversation declares. A precompute
// installs nothing, so it is not a compaction here.
async function onSessionCompact($, e, next) {
  if (e.trigger === 'precompute') return next(e)
  const r = (await swapInDraft($, e).catch(() => null)) ?? (await next(e))
  if (!e.agentId) {
    draft = null
    draftTries = 0
  }
  await noteCompaction($)
  return r
}

async function noteCompaction($) {
  activeAdvisor = advisorAlias(await setting($, 'advisorModel'))
  activeSession = await $.session.id().catch(() => null)
  compactedAt = Math.floor(Date.now() / 1000)
  await report($, true)
}

async function onSetModel($, e) {
  const alias = String(e.model ?? '').trim().toLowerCase()
  const model = MODELS[alias] ?? (/^claude-[a-z0-9.-]+$/.test(alias) ? alias : undefined)
  const effort = String(e.effort ?? '').trim().toLowerCase()
  if (e.model != null && !model) {
    return { result: 'Unknown model: ' + e.model + '. Use haiku, sonnet, opus, fable or a full claude-… id.', isError: true }
  }
  if (e.effort != null && effort !== 'auto' && !EFFORTS.includes(effort)) {
    return { result: 'Unknown effort: ' + e.effort + '. Use ' + EFFORTS.join(', ') + ' or auto.', isError: true }
  }
  if (!model && e.effort == null) return { result: 'Give a model, an effort, or both.', isError: true }

  if (model) want = { ...want, model }
  if (e.effort != null) want = { ...want, effort: effort === 'auto' ? undefined : effort }

  // turn.step covers the rest of this turn. The session's own model and effort (what /model shows) follow once
  // Claude Code is idle. A command can't run from inside a hook the turn waits on, hence the timer.
  $.clock.after(0, () => {
    if (model) {
      $.command.run({ command: 'model', args: model }).then(
        () => { if (want.model === model) want = { ...want, model: undefined } },
        () => {},
      )
    }
    if (e.effort != null) {
      $.command.run({ command: 'effort', args: effort }).then(
        () => { if (want.effort === (effort === 'auto' ? undefined : effort)) want = { ...want, effort: undefined } },
        () => {},
      )
    }
  })

  const what = [model, e.effort != null && 'effort ' + effort].filter(Boolean).join(', ')
  // Ending the turn lets the queued /model and /effort run, so the session (and /model) switch for good.
  return {
    result: 'Switched to ' + what + ' from your next request. Your system prompt still names the previous model; ignore that. ' +
      'Now stop: end your turn with one short line telling the user the switch is done and asking them to confirm ' +
      '(or say what to do next) so the session itself switches. Do not continue the task in this turn.',
  }
}

// The model's show_visual call streams to the Visual panel while it is written. The call's arguments arrive as pieces
// of JSON; the Markdown read out of them so far goes to the file a few times a second at most. The tool.call hook
// writes the finished card.
async function onShowVisual($, e) {
  const file = await $.env.get('CLAUDIU_VISUAL_FILE')
  if (!file) {
    return { result: "The Visual panel isn't available in this session (Claudiu's mod has no file for it).", isError: true }
  }
  if (typeof e.content !== 'string' || !e.content.trim()) return { result: 'content must be non-empty Markdown.', isError: true }
  writeVisual($, file, e.tool_use_id ?? lastVisual ?? 'v' + Date.now(), e.title, e.content, true)
  return { result: "Shown in Claudiu's Visual panel. Say the key facts in a short reply; don't repeat the content." }
}

// Applies the saved model and effort to this turn's steps, and streams a show_visual call to the Visual panel while
// the model writes it. Sub-agent steps are left alone.
async function* onTurnStep($, e, next) {
  const step = !e.agentId && (want.model || want.effort)
    ? { ...e, model: want.model ?? e.model, effort: want.effort ?? e.effort }
    : e
  const file = e.agentId ? null : await $.env.get('CLAUDIU_VISUAL_FILE')
  if (!file) return yield* next(step)

  const open = new Map() // block index -> the show_visual call being written there
  for await (const c of next(step)) {
    if (c.kind === 'tool' && c.name === VISUAL_TOOL) {
      open.set(c.index, { id: c.id, json: '', at: 0 })
      lastVisual = c.id
    } else if (c.kind === 'input' && open.has(c.index)) {
      const v = open.get(c.index)
      v.json += c.json
      if (Date.now() - v.at >= 150) {
        v.at = Date.now()
        writeVisual($, file, v.id, partialString(v.json, 'title'), partialString(v.json, 'content'), false)
      }
    }
    yield c
  }
}

async function onSessionStart($, e, next) {
  const r = await next(e)
  // Only the first time: a later start (/clear, resume) must keep what Claudiu switched since, as that ctl command
  // is not replayed.
  bgCompact ??= (await $.env.get('CLAUDIU_BG_COMPACT')) === '1'
  await report($)
  await applyCtl($)

  ctlPoll?.cancel()
  ctlPoll = $.clock.every(1000, () => applyCtl($))
  askPoll?.cancel()
  askPoll = $.clock.every(500, () => answerAsk($))
  await watchSessionName($)
  return r
}

export function register(on) {
  on('session.measure', onMeasure)
  on('command.run', onCommandRun)
  on('config.set', onConfigSet)
  on('session.compact', onSessionCompact)
  on('turn.complete', onTurnComplete)
  on('tool.call', { tool: 'mcp__claudiu__set_model' }, onSetModel)
  on('tool.call', { tool: VISUAL_TOOL }, onShowVisual)
  on('turn.step', onTurnStep)
  on('session.start', onSessionStart)
}
