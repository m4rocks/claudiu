// Loaded per process through `--plugin-dir`; Claudiu itself writes nothing to the user's Claude Code config.
// It answers Claudiu's `set_model` MCP tool in-process so the override applies from the next request, even
// mid-turn, then runs /model and /effort once idle so the session itself (what /model shows) switches too;
// Claude Code saves those as the user's default. It also applies the session name Claudiu writes to
// `CLAUDIU_NAME_FILE` with `/rename`, which Claude Code queues until the session is idle.
// It also applies the settings the user changes in Claudiu's sidebar (auto-compact, advisor), sent through
// `CLAUDIU_CTL_FILE`: the same as the user choosing them in `/config` or typing `/advisor`, which Claude Code saves.
// It reports usage and status (context, 5-hour / 7-day limits, auto-compact, advisor, the command the user
// last ran) to `CLAUDIU_USAGE_DIR/<claude session id>.json` for Claudiu's sidebar; no tokens are spent.
// It answers Claudiu's `show_visual` tool in-process and, while the model is still writing the call, streams
// its Markdown to `CLAUDIU_VISUAL_FILE` so the Visual panel fills live.
// Last, it makes the one-shot text completions Claudiu asks for through `CLAUDIU_ASK_FILE` (commit messages, session
// titles) with `$.model.complete`, so they run on this session's own client and Claudiu needs no `claude -p`.
const MODELS = {
  haiku: 'claude-haiku-4-5-20251001',
  sonnet: 'claude-sonnet-5-5',
  opus: 'claude-opus-5-5',
  fable: 'claude-fable-5-1',
}
const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max']

let want = {} // { model?, effort? }, per process
let named = null // `${claude session id}\n${name}` last applied
let poll
let ctlPoll
let ctlSeen = null // highest command number handled; null until the file was first read (older ones are not replayed)
let askPoll
let askSeen = null // id of the last request looked at
let asking = false
let visualChain = Promise.resolve() // visual writes in order, without holding up the stream
let lastVisual = null // id of the show_visual call being written or last written
const VISUAL_TOOL = 'mcp__claudiu__show_visual'
const ASK_MODELS = ['haiku', 'sonnet', 'opus']

// ---- usage report -------------------------------------------------------------------------------------------
const stem = id => String(id).replace(/[^A-Za-z0-9_-]/g, '_')
const secs = iso => { const t = Date.parse(iso); return Number.isFinite(t) ? Math.floor(t / 1000) : null }

let measured = null // { context, limits: { five_hour, seven_day } } from the last session.measure
let written = '' // what the file holds, minus times, so an unchanged report is not rewritten (a write is cheap, a redraw is not)
let activeAdvisor = null // advisor the current conversation declared; a model change waits for /clear or /compact
let activeSession = null // Claude session id activeAdvisor belongs to (/clear starts a new one)

// The token count auto-compaction runs at (null while it is off). It only changes with settings, the model or the
// environment, and the breakdown it comes from is a local estimate, so it is read again at most once a minute or after
// a setting changed.
let compactAt = null
let compactRead = 0
async function threshold($, fresh) {
  if (!fresh && Date.now() - compactRead < 60_000) return compactAt
  compactRead = Date.now()
  const b = (await $.session.usage({ breakdown: 'summary' }).catch(() => null))?.context.breakdown
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

// The menu speaks aliases; settings may hold a resolved id such as `claude-opus-5-5`.
function advisorAlias(value) {
  if (typeof value !== 'string') return null
  const v = value.trim()
  if (!v || v.toLowerCase() === 'off') return null
  const lower = v.toLowerCase()
  if (lower === 'opus' || lower.includes('opus')) return 'opus'
  if (lower === 'sonnet' || lower.includes('sonnet')) return 'sonnet'
  return v
}

// Reads the status Claude Code keeps; every read is best effort, a failed one is left out rather than failing a hook.
// `override` carries a value Claudiu just asked for, so the sidebar updates before the host has refreshed its cache.
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
    let advisor = advisorAlias(await setting($, 'advisorModel'))
    if ('auto_compact' in override) autoCompact = override.auto_compact
    if ('advisor' in override) advisor = override.advisor
    // A different model only reaches the running conversation on /clear or /compact. Off and on take effect at
    // once, so null / newly-named advisors replace the active one; a model-to-model change keeps the old one.
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
      compact_at: await threshold($, fresh),
      advisor,
      advisor_active: activeAdvisor,
    }
    const key = JSON.stringify(snap, (k, v) => (k === 'seen_at' ? undefined : v))
    if (key === written) return
    written = key
    await $.fs.write(dir + '/' + stem(id) + '.json', JSON.stringify({ ...snap, captured_at: Math.floor(Date.now() / 1000) }))
  } catch {
    // Reporting must never disturb the session.
  }
}

// Applies what Claudiu's sidebar asked for since the last look. The file holds the latest few `{ seq, key, value }`
// commands; only known keys and plainly shaped values are acted on.
async function applyCtl($) {
  try {
    const file = await $.env.get('CLAUDIU_CTL_FILE')
    if (!file) return
    const cmds = JSON.parse(await $.fs.read(file).catch(() => '{}')).cmds ?? []
    if (ctlSeen === null) {
      ctlSeen = cmds.reduce((top, c) => Math.max(top, c.seq), 0)
      return
    }
    const override = {}
    for (const c of cmds.filter(c => c.seq > ctlSeen).sort((a, b) => a.seq - b.seq)) {
      ctlSeen = c.seq
      if (c.key === 'auto_compact' && typeof c.value === 'boolean') {
        await $.config.set({ key: 'autoCompact', value: c.value }).catch(() => {})
        override.auto_compact = c.value
      } else if (c.key === 'advisor' && typeof c.value === 'string' && /^[A-Za-z0-9._-]{1,60}$/.test(c.value)) {
        await $.command.run({ command: 'advisor', args: c.value }).catch(() => {})
        override.advisor = advisorAlias(c.value)
      }
    }
    if (Object.keys(override).length > 0) await report($, true, override)
  } catch {
    // A bad file must not disturb the session.
  }
}

// ---- Visual panel ---------------------------------------------------------------------------------------------
// The text of a string property of JSON that is still being written; it may end anywhere, even inside an escape.
function partialString(json, key) {
  const m = new RegExp('"' + key + '"\\s*:\\s*"').exec(json)
  if (!m) return null
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
      out += { n: '\n', t: '\t', r: '\r', b: '\b', f: '\f' }[n] ?? n
      i += 2
    }
  }
  return out
}

function writeVisual($, file, call, title, content, done) {
  const body = JSON.stringify({ call, title: String(title ?? '').slice(0, 80), content: String(content ?? '').slice(0, 200_000), done, at: Date.now() })
  visualChain = visualChain.then(() => $.fs.write(file, body)).catch(() => {})
}

// ---- one-shot completions for Claudiu ---------------------------------------------------------------------------
// The request file holds `{ id, model, system, prompt, deadline }`; the answer goes next to it as `<name>.res.json`
// (`working` as soon as the request is picked up, then `done` with the text or `error`).
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
    const out = r.isAnswered ? { id, state: 'done', text: r.text } : { id, state: 'error', error: 'The model call failed (' + r.reason + (r.status ? ' ' + r.status : '') + ').' }
    await $.fs.write(reply, JSON.stringify(out))
  } catch (err) {
    if (reply) await $.fs.write(reply, JSON.stringify({ id, state: 'error', error: 'The model call failed: ' + String(err?.message ?? err).slice(0, 200) })).catch(() => {})
  } finally {
    asking = false
  }
}

export function register(on) {
  on('session.measure', async ($, e, next) => {
    const seen = Math.floor(Date.now() / 1000)
    const limit = kind => {
      const r = e.rateLimits.find(l => l.kind === kind)
      return r ? { utilization: Math.min(1, Math.max(0, r.percentUsed / 100)), resets_at: secs(r.resetsAt), seen_at: seen } : null
    }
    measured = { context: e.context, limits: { five_hour: limit('five_hour'), seven_day: limit('seven_day') } }
    const r = await next(e)
    await report($)
    return r
  })

  on('command.run', async ($, e, next) => {
    const r = await next(e) // the command has run: a setting it changed is read after it
    await report($, true)
    return r
  })

  on('config.set', async ($, e, next) => {
    const r = await next(e)
    await report($, true)
    return r
  })

  // Compaction rebuilds the request, so the saved advisor becomes the one this conversation declares.
  on('session.compact', async ($, e, next) => {
    const r = await next(e)
    activeAdvisor = advisorAlias(await setting($, 'advisorModel'))
    activeSession = await $.session.id().catch(() => null)
    await report($, true)
    return r
  })


  on('tool.call', { tool: 'mcp__claudiu__set_model' }, async ($, e) => {
    const alias = String(e.model ?? '').trim().toLowerCase()
    const model = MODELS[alias] ?? (/^claude-[a-z0-9.-]+$/.test(alias) ? alias : undefined)
    const effort = String(e.effort ?? '').trim().toLowerCase()
    if (e.model != null && !model) return { result: 'Unknown model: ' + e.model + '. Use haiku, sonnet, opus, fable or a full claude-… id.', isError: true }
    if (e.effort != null && effort !== 'auto' && !EFFORTS.includes(effort)) return { result: 'Unknown effort: ' + e.effort + '. Use ' + EFFORTS.join(', ') + ' or auto.', isError: true }
    if (!model && e.effort == null) return { result: 'Give a model, an effort, or both.', isError: true }
    if (model) want = { ...want, model }
    if (e.effort != null) want = { ...want, effort: effort === 'auto' ? undefined : effort }
    // turn.step covers the rest of this turn; the session's own model/effort (what /model shows) follow once
    // Claude Code is idle. A command can't run from inside a hook the turn waits on, hence the timer.
    $.clock.after(0, () => {
      if (model) $.command.run({ command: 'model', args: model }).then(() => { if (want.model === model) want = { ...want, model: undefined } }, () => {})
      if (e.effort != null) $.command.run({ command: 'effort', args: effort }).then(() => { if (want.effort === (effort === 'auto' ? undefined : effort)) want = { ...want, effort: undefined } }, () => {})
    })
    const what = [model, e.effort != null && 'effort ' + effort].filter(Boolean).join(', ')
    // Ending the turn lets the queued /model and /effort run, so the session (and /model) switch for good.
    return {
      result: 'Switched to ' + what + ' from your next request. Your system prompt still names the previous model; ignore that. ' +
        'Now stop: end your turn with one short line telling the user the switch is done and asking them to confirm ' +
        '(or say what to do next) so the session itself switches. Do not continue the task in this turn.',
    }
  })

  // The model's `show_visual` call streams to the Visual panel while it is written: the call's arguments arrive as
  // pieces of JSON, and Markdown read out of them so far goes to the file at most a few times a second. The tool.call
  // hook below writes the finished card.
  on('tool.call', { tool: VISUAL_TOOL }, async ($, e) => {
    const file = await $.env.get('CLAUDIU_VISUAL_FILE')
    if (!file) return { result: "The Visual panel isn't available in this session (Claudiu's mod has no file for it).", isError: true }
    if (typeof e.content !== 'string' || !e.content.trim()) return { result: 'content must be non-empty Markdown.', isError: true }
    writeVisual($, file, e.tool_use_id ?? lastVisual ?? 'v' + Date.now(), e.title, e.content, true)
    return { result: "Shown in Claudiu's Visual panel. Say the key facts in a short reply; don't repeat the content." }
  })

  on('turn.step', async function* ($, e, next) {
    const step = !e.agentId && (want.model || want.effort) ? { ...e, model: want.model ?? e.model, effort: want.effort ?? e.effort } : e
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
  })

  on('session.start', async ($, e, next) => {
    const r = await next(e)
    await report($)
    await applyCtl($)
    ctlPoll?.cancel()
    ctlPoll = $.clock.every(1000, () => applyCtl($))
    askPoll?.cancel()
    askPoll = $.clock.every(500, () => answerAsk($))
    const file = await $.env.get('CLAUDIU_NAME_FILE')
    if (!file) return r
    const read = async () => (await $.fs.read(file).catch(() => '')).trim()
    const key = async name => (await $.session.id()) + '\n' + name
    // What the file holds now is the name the session was launched with (`--name`): don't re-run it.
    const initial = await read()
    named = initial ? await key(initial) : null
    poll?.cancel()
    poll = $.clock.every(1000, async () => {
      const name = await read()
      if (!name) return
      const k = await key(name) // a new Claude session id (/clear) gets the name again
      if (k === named) return
      named = k
      await $.command.run({ command: 'rename', args: name }).catch(() => { named = null })
    })
    return r
  })
}
