import { expect, mock, test } from 'claude-code/testing'

const usage = { input_tokens: 0, output_tokens: 0, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 }
const STEP = { turnId: 't', index: 0, answer: '', toolUses: [], stopReason: 'end_turn' as const, usage: null }
const SHOWN = { isFullscreen: true, columns: 120 }
const said = (effort: string, reason = 'Because.') => ({ isAnswered: true, text: JSON.stringify({ effort, reason }), usage })
// The host hands paths back resolved, with backslashes on Windows.
const slashed = (path: string) => path.replace(/\\/g, '/')

type Options = { env?: Record<string, string>; files?: Record<string, string>; git?: string }

// A session whose layers below the mod are all the test's: Haiku answers with `s.answer`, the transcript has `s.turns`
// prompts, and every request's effort, every slash command and every report is kept.
function setup(on: any, { env = { CLAUDIU_AUTO_EFFORT: 'low-high' }, files = {}, git = '' }: Options = {}) {
  const clock = mock.clock(on)
  mock.env(on, { CLAUDIU_USAGE_DIR: 'usage', ...env })
  const s = {
    clock,
    turns: 0,
    answer: (_e: any): any => said('high', 'Big change.'),
    asked: [] as any[],
    efforts: [] as (string | number | undefined)[],
    ran: [] as string[],
    reports: [] as any[],
    report: () => s.reports.at(-1),
  }
  on('fs.read', (_$: any, e: any) => {
    const hit = Object.keys(files).find(k => slashed(e.path).endsWith(k))
    if (hit === undefined) throw new Error('no such file')
    return { value: files[hit] }
  })
  on('fs.write', (_$: any, e: any) => {
    if (slashed(e.path).endsWith('usage/s1.json')) s.reports.push(JSON.parse(e.text))
    return { value: undefined }
  })
  on('session.id', () => ({ value: 's1' }))
  on('session.model', () => ({ value: 'claude-opus-5-5' }))
  on('session.cwd', () => ({ value: '/repo' }))
  on('session.turns', () => ({ value: s.turns }))
  on('config.list', () => ({ value: [] }))
  on('settings.read', () => ({ value: {} }))
  on('session.start', (_$: any, e: any) => ({ cwd: e.cwd }))
  on('process.run', () => ({ value: { exitCode: 0, stdout: git, stderr: '' } }))
  on('model.complete', (_$: any, e: any) => {
    s.asked.push(e)
    return Promise.resolve(s.answer(e)).then(value => ({ value }))
  })
  on('command.run', (_$: any, e: any) => {
    s.ran.push(`${e.command} ${e.args}`.trim())
    return {}
  })
  on('prompt.submit', (_$: any, e: any) => ({ text: e.text }))
  on('turn.step', async function* (_$: any, e: any) {
    s.efforts.push(e.effort)
    return STEP
  })
  on('turn.complete', (_$: any, e: any) => ({ text: e.answer }))
  on('turn.start', (_$: any, e: any) => ({ turnId: e.turnId }))
  return s
}

const typed = (text: string, more: object = {}) => ({ text, wait: false, origin: { kind: 'composer' as const }, ...more })

// One request of the main thread (or of a subagent), at the session's own effort.
async function step($: any, agentId?: string) {
  for await (const _ of $.turn.step({ turnId: 't', index: 0, model: 'claude-opus-5-5', effort: 'medium', messageCount: 1, agentId })) void _
}

async function answered($: any, answer: string) {
  await $.turn.complete({ turnId: 't', answer, durationMs: 1, isAborted: false, reason: 'answer' })
}

test('judges the first prompt from what it asks, the files it names and git, and applies the pick to the main thread', async ($, on) => {
  const s = setup(on, { files: { '/repo/src/a.rs': 'one\ntwo\nthree' }, git: ' M src/a.rs\n?? notes.md\n' })
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  expect(s.report()).toMatchObject({ auto_effort: 'on', effort: null })

  await $.prompt.submit(typed('@src/a.rs#L1-2: split this up, and @docs too'))
  expect(s.asked.length).toBe(1)
  const ask = s.asked[0]
  expect(ask).toMatchObject({ model: 'haiku', effort: 'medium' })
  expect(ask.system).toContain('xhigh: long-running agentic and coding tasks')
  expect(ask.prompt).toContain('src/a.rs (3 lines)')
  expect(ask.prompt).toMatch(/^docs$/m) // a folder or missing path: its name only
  expect(ask.prompt).toContain('notes.md')
  expect(ask.prompt).toContain('Allowed levels: low, medium, high.')
  expect(s.report()).toMatchObject({ effort: 'high', auto_effort: 'on', effort_reason: 'Big change.' })

  await step($)
  await step($, 'sub')
  expect(s.efforts).toEqual(['high', 'medium']) // a subagent keeps its own
})

test('a first prompt is judged however short; later ones read the current effort and the end of the last reply', async ($, on) => {
  const s = setup(on, { env: { CLAUDIU_AUTO_EFFORT: 'low-medium' } })
  s.answer = () => said('max') // outside the range: held to its top
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await $.prompt.submit(typed('hi'))
  expect(s.asked.length).toBe(1)
  await step($)
  expect(s.efforts).toEqual(['medium'])

  s.turns = 1
  await answered($, 'x'.repeat(3000) + 'THE END')
  s.answer = () => said('low', 'A quick question.')
  await $.prompt.submit(typed('what does that function return'))
  const later = s.asked[1].prompt
  expect(later).toContain('<current_effort>medium</current_effort>')
  expect(later).toContain('THE END')
  expect(later).not.toContain('x'.repeat(2001)) // the last 2,000 characters only
  expect(later).not.toContain('changed_files')
  await step($)
  expect(s.efforts).toEqual(['medium', 'low'])
})

test('short replies, notifications, meta prompts and prompts joining a running turn keep the effort', async ($, on) => {
  const s = setup(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  s.turns = 3
  await $.prompt.submit(typed('yes do it'))
  await $.prompt.submit(typed('/compact', { command: { name: 'compact', args: '' } }))
  await $.prompt.submit(typed('also handle the empty case please', { turnId: 'running' }))
  await $.prompt.submit({ text: 'Background task finished with output', wait: false, origin: { kind: 'task-notification' } })
  await $.prompt.submit(typed('a note the person does not see', { isMeta: true }))
  expect(s.asked.length).toBe(0)

  await $.prompt.submit(typed('then add tests for the parser', { turnId: 'running', wait: true })) // waits for its own turn
  await $.prompt.submit({ text: '@src/a.rs#L3-9: Explain this code.', wait: false, origin: { kind: 'plugin', name: 'Claudiu', asUser: true } })
  expect(s.asked.length).toBe(2)
})

test('when Haiku fails or is too slow the prompt goes on at the effort it had, and a late answer is dropped', async ($, on) => {
  const s = setup(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  s.answer = () => ({ isAnswered: false, reason: 'api-error', status: 529, error: 'overloaded', usage })
  await $.prompt.submit(typed('refactor the store'))
  expect(s.report()).toMatchObject({ auto_effort: 'fallback', effort_reason: null })
  await step($)
  expect(s.efforts).toEqual(['medium']) // the launch effort

  s.turns = 1
  s.answer = () => said('not a level')
  await $.prompt.submit(typed('now the other module please'))
  expect(s.report().auto_effort).toBe('fallback')

  let late = (_: any) => {}
  s.answer = () => new Promise(resolve => { late = resolve })
  const submitted = $.prompt.submit(typed('and the third one as well'))
  await s.clock.advance(5000)
  await submitted
  expect(s.report().auto_effort).toBe('fallback')
  late(said('high'))
  await s.clock.advance(10)
  await step($)
  expect(s.efforts).toEqual(['medium', 'medium'])

  s.answer = () => said('low', 'Small.')
  await $.prompt.submit(typed('rename the helper in the store'))
  expect(s.report()).toMatchObject({ auto_effort: 'on', effort: 'low', effort_reason: 'Small.' })
})

test('picking an effort by hand turns auto off until /clear or Auto, and saves no default', async ($, on) => {
  const files = { 'ctl/me.json': JSON.stringify({ cmds: [] }) }
  const s = setup(on, { env: { CLAUDIU_AUTO_EFFORT: 'low-high', CLAUDIU_CTL_FILE: 'ctl/me.json' }, files })
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await $.prompt.submit(typed('build the settings page'))

  await $.command.run({ command: 'effort', args: 'low', origin: { kind: 'composer' }, presentation: SHOWN })
  expect(s.report()).toMatchObject({ auto_effort: 'manual', effort: 'low', effort_reason: null })
  s.turns = 1
  await $.prompt.submit(typed('and wire it into the app now'))
  expect(s.asked.length).toBe(1) // not judged while off
  await step($)
  expect(s.efforts).toEqual(['medium']) // the session's own effort, which /effort set

  await $.command.run({ command: 'clear', args: '', origin: { kind: 'composer' }, presentation: SHOWN })
  expect(s.report().auto_effort).toBe('on')

  const r = await $.tool.call({ tool: 'mcp__claudiu__set_model', effort: 'xhigh' })
  expect(r.isError).toBeFalsy()
  await s.clock.advance(10)
  expect(s.ran.filter(c => c.startsWith('effort'))).toEqual(['effort low']) // set_model ran no /effort
  expect(s.report()).toMatchObject({ auto_effort: 'manual', effort: 'xhigh' })
  await step($)
  expect(s.efforts.at(-1)).toBe('xhigh')

  files['ctl/me.json'] = JSON.stringify({ cmds: [{ seq: 1, key: 'effort', value: 'auto' }] })
  await s.clock.advance(1000)
  expect(s.report().auto_effort).toBe('on')
  files['ctl/me.json'] = JSON.stringify({ cmds: [{ seq: 1, key: 'effort', value: 'auto' }, { seq: 2, key: 'effort', value: 'medium' }] })
  await s.clock.advance(1000)
  expect(s.report()).toMatchObject({ auto_effort: 'manual', effort: 'medium' })
})

test('a resumed session reads the last reply of the transcript it continues', async ($, on) => {
  const transcript = [
    JSON.stringify({ type: 'user', message: { content: 'hello' } }),
    JSON.stringify({ type: 'assistant', message: { content: [{ type: 'text', text: 'I changed the parser.' }] } }),
    JSON.stringify({ type: 'assistant', message: { content: [{ type: 'tool_use', name: 'Bash' }] } }),
    '',
  ].join('\n')
  const s = setup(on, { env: { CLAUDIU_AUTO_EFFORT: 'low-high', CLAUDIU_RESUMED_TRANSCRIPT: 'old.jsonl' }, files: { 'old.jsonl': transcript } })
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  s.turns = 4
  await $.prompt.submit(typed('now run the tests for it'))
  expect(s.asked[0].prompt).toContain('I changed the parser.')
})

test('a session launched without auto effort asks Haiku nothing', async ($, on) => {
  const s = setup(on, { env: {} })
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await $.prompt.submit(typed('refactor the whole store please'))
  await step($)
  await s.clock.advance(10) // the report after a request is not waited for
  expect(s.asked).toEqual([])
  expect(s.efforts).toEqual(['medium'])
  expect(s.report()).toMatchObject({ auto_effort: 'off', effort: 'medium' })
})

test('a prompt typed during a turn gets its effort when its own turn starts', async ($, on) => {
  const s = setup(on)
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await $.prompt.submit(typed('build the import screen'))
  s.turns = 1
  s.answer = () => said('low', 'Docs only.')
  await $.prompt.submit(typed('then write the docs for it', { turnId: 'running', wait: true }))
  await step($)
  expect(s.efforts).toEqual(['high']) // the running turn keeps its effort
  await $.turn.start({ text: 'then write the docs for it', turnId: 't2' })
  await step($)
  expect(s.efforts).toEqual(['high', 'low'])
  expect(s.report()).toMatchObject({ effort: 'low', effort_reason: 'Docs only.' })
})

test('a level picked by hand before Claudiu restarted the session still holds', async ($, on) => {
  const s = setup(on, { env: { CLAUDIU_AUTO_EFFORT: 'low-high', CLAUDIU_HAND_EFFORT: 'xhigh' } })
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  expect(s.report()).toMatchObject({ auto_effort: 'manual', effort: 'xhigh' })
  s.turns = 2
  await $.prompt.submit(typed('keep going with the migration'))
  await step($)
  expect(s.asked).toEqual([])
  expect(s.efforts).toEqual(['xhigh'])
})
