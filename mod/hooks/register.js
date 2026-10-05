// Loaded per process through `--plugin-dir`; nothing is written to the user's Claude Code config.
// It answers Claudiu's `set_model` MCP tool in-process so the override applies from the next request, even
// mid-turn (not saved as the user's default). Renaming can't be done this way: a plugin-run /rename queues until idle.
const MODELS = {
  haiku: 'claude-haiku-4-5-20251001',
  sonnet: 'claude-sonnet-5-5',
  opus: 'claude-opus-5-5',
  fable: 'claude-fable-5-1',
}
const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max']

let want = {} // { model?, effort? }, per process

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
    return { result: 'Applied to the next request: ' + [model, e.effort != null && 'effort ' + effort].filter(Boolean).join(', ') }
  })

  on('turn.step', async function* ($, e, next) {
    if (e.agentId || (!want.model && !want.effort)) return yield* next(e)
    return yield* next({ ...e, model: want.model ?? e.model, effort: want.effort ?? e.effort })
  })
}
