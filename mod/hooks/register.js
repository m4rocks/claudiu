// Loaded per process through `--plugin-dir`; Claudiu itself writes nothing to the user's Claude Code config.
// It answers Claudiu's `set_model` MCP tool in-process so the override applies from the next request, even
// mid-turn, then runs /model and /effort once idle so the session itself (what /model shows) switches too;
// Claude Code saves those as the user's default. It also applies the session name Claudiu writes to
// `CLAUDIU_NAME_FILE` with `/rename`, which Claude Code queues until the session is idle.
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

export function register(on) {
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
