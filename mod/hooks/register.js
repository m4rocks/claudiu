// Loaded per process through `--plugin-dir`; Claudiu itself writes nothing to the user's Claude Code config.
// It answers Claudiu's `set_model` MCP tool in-process so the override applies from the next request, even
// mid-turn, then runs /model and /effort once idle so the session itself (what /model shows) switches too;
// Claude Code saves those as the user's default. It also applies the session name Claudiu writes to
// `CLAUDIU_NAME_FILE` with `/rename`, which Claude Code queues until the session is idle.
// It also applies the settings the user changes in Claudiu's sidebar (auto-compact, advisor), sent through
// `CLAUDIU_CTL_FILE`: the same as the user choosing them in `/config` or typing `/advisor`, which Claude Code saves.
// Last, it reports usage and status (context, 5-hour / 7-day limits, auto-compact, advisor, the command the user
// last ran) to `CLAUDIU_USAGE_DIR/<claude session id>.json` for Claudiu's sidebar; no tokens are spent.
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

// ---- usage report -------------------------------------------------------------------------------------------
const stem = id => String(id).replace(/[^A-Za-z0-9_-]/g, '_')
const secs = iso => { const t = Date.parse(iso); return Number.isFinite(t) ? Math.floor(t / 1000) : null }

let measured = null // { context, limits: { five_hour, seven_day } } from the last session.measure
let written = '' // what the file holds, minus times, so an unchanged report is not rewritten (a write is cheap, a redraw is not)

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

// Reads the status Claude Code keeps; every read is best effort, a failed one is left out rather than failing a hook.
async function report($, fresh = false) {
  try {
    const dir = await $.env.get('CLAUDIU_USAGE_DIR')
    if (!dir) return
    const id = await $.session.id()
    if (!id) return
    const ctx = measured?.context
    const fraction = ctx?.tokens != null && ctx?.window > 0 ? Math.min(1, ctx.tokens / ctx.window) : null
    const rows = await $.config.list().catch(() => [])
    const advisor = await $.settings.read({ key: 'advisorModel' }).catch(() => undefined)
    const snap = {
      session_id: id,
      five_hour: measured?.limits.five_hour ?? null,
      seven_day: measured?.limits.seven_day ?? null,
      context_fraction: fraction,
      context_tokens: ctx?.tokens ?? null,
      context_window: ctx?.window ?? null,
      model: (await $.session.model().catch(() => null)) ?? null,
      account: (await $.env.get('CLAUDIU_ACCOUNT')) ?? null,
      auto_compact: rows.find(r => r.key === 'autoCompact')?.value ?? null,
      compact_at: await threshold($, fresh),
      advisor: typeof advisor === 'string' ? advisor : null,
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
    for (const c of cmds.filter(c => c.seq > ctlSeen).sort((a, b) => a.seq - b.seq)) {
      ctlSeen = c.seq
      if (c.key === 'auto_compact' && typeof c.value === 'boolean') await $.config.set({ key: 'autoCompact', value: c.value }).catch(() => {})
      else if (c.key === 'advisor' && typeof c.value === 'string' && /^[A-Za-z0-9._-]{1,60}$/.test(c.value)) await $.command.run({ command: 'advisor', args: c.value }).catch(() => {})
      await report($, true)
    }
  } catch {
    // A bad file must not disturb the session.
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

  on('turn.step', async function* ($, e, next) {
    if (e.agentId || (!want.model && !want.effort)) return yield* next(e)
    return yield* next({ ...e, model: want.model ?? e.model, effort: want.effort ?? e.effort })
  })

  on('session.start', async ($, e, next) => {
    const r = await next(e)
    await report($)
    await applyCtl($)
    ctlPoll?.cancel()
    ctlPoll = $.clock.every(1000, () => applyCtl($))
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
