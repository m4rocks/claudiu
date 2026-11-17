import { expect, mock, test } from 'claude-code/testing'

// What `claude/subagents.rs` lists for a type.
function type(name: string, model: string) {
  return { name, model, description: `Runs the task on ${model}.`, prompt: 'Say the mod lost track.' }
}

const SUBAGENTS = JSON.stringify({ codex: { edits: false, types: [type('codex-gpt-5-5', 'gpt-5.5')] } })

// What the engine answers beneath the mod for a session with Codex on. `helper` is what `claudiu --acp` writes;
// `requests` collects what the mod sent it.
function engine(on: any, helper: string[], requests: unknown[], subagents = SUBAGENTS) {
  mock.env(on, { CLAUDIU_SUBAGENTS: subagents, CLAUDIU_EXE: '/apps/claudiu' })
  mock.clock(on)
  on('fs.read', () => ({ value: '' }))
  on('fs.write', () => ({ value: undefined }))
  on('session.id', () => ({ value: 's1' }))
  on('session.model', () => ({ value: 'claude-haiku-5-5' }))
  on('session.cwd', () => ({ value: '/work/sub' }))
  on('config.list', () => ({ value: [] }))
  on('settings.read', () => ({ value: {} }))
  on('session.start', (_$: any, e: any) => ({ cwd: e.cwd }))
  const registered: any[] = []
  on('agent.register', (_$: any, e: any) => {
    registered.push(e)
    return { value: undefined }
  })
  on('agent.spawn', () => ({ model: 'haiku', agentId: 'a1' }))
  on('process.spawn', async function* (_$: any, e: any) {
    requests.push({ argv: e.argv, input: JSON.parse(e.input) })
    // Pieces cut mid-line, as a pipe delivers them.
    const out = helper.join('\n') + '\n'
    yield { stream: 'stdout', text: out.slice(0, 7) }
    yield { stream: 'stdout', text: out.slice(7) }
    return { value: { code: 0, signal: null } }
  })
  // Claude, for any agent the mod leaves alone.
  on('turn.step', async function* (_$: any, e: any) {
    yield { kind: 'text', index: 0, text: 'Claude answered' }
    return { turnId: e.turnId, index: e.index, answer: 'Claude answered', toolUses: [], stopReason: 'end_turn', usage: null }
  })
  return registered
}

async function step($: any, turnId: string, index: number, agentId?: string) {
  const chunks: any[] = []
  const stream = $.turn.step({ turnId, index, model: 'haiku', messageCount: 1, agentId })
  for await (const c of stream) chunks.push(c)
  return chunks
}

test('a Codex subagent runs the task in Codex and hands back its last message', async ($, on) => {
  const requests: any[] = []
  const registered = engine(on, [
    JSON.stringify({ kind: 'text', text: 'Looking.' }),
    JSON.stringify({ kind: 'step', text: 'Read src/a.rs' }),
    JSON.stringify({ kind: 'report', text: 'The bug is on line 12.' }),
  ], requests)

  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  // Registered as Claudiu lists it; Claude Code names it Claudiu:codex-gpt-5-5.
  expect(registered).toEqual([{ name: 'codex-gpt-5-5', description: 'Runs the task on gpt-5.5.', prompt: 'Say the mod lost track.', model: 'haiku', tools: [] }])
  await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')

  expect(requests).toEqual([{ argv: ['/apps/claudiu', '--acp'], input: { agent: 'codex', model: 'gpt-5.5', edits: false, cwd: '/work/sub', prompt: 'Find the bug' } }])
  // What Codex writes and does is the agent's own text, which its transcript keeps; Claude gets only the report.
  const text = chunks.filter(c => c.kind === 'text').map(c => c.text).join('')
  expect(text).toBe('Codex on gpt-5.5, read-only\n\nLooking.\n▸ Read src/a.rs\n')
  const tool = chunks.find(c => c.kind === 'tool')
  expect(tool.name).toBe('SubagentHandback')
  // The report names the steps, since Claude Code counts none of Codex's tool calls as the subagent's.
  const report = 'The bug is on line 12.\n\n---\nSteps Codex ran (1):\n- Read src/a.rs'
  const input = chunks.filter(c => c.kind === 'input').map(c => c.json).join('')
  expect(JSON.parse(input)).toEqual({ message: report })

  // A second step of the same turn (a refused hand-back) hands back the same report without running Codex again.
  const again = await step($, 't1', 1, 'a1')
  expect(requests.length).toBe(1)
  expect(JSON.parse(again.filter(c => c.kind === 'input').map(c => c.json).join(''))).toEqual({ message: report })

  // A hand-back refused again ends the turn with the report as text instead of asking forever.
  const last = await step($, 't1', 2, 'a1')
  expect(last.some(c => c.kind === 'tool')).toBe(false)
  expect(last.filter(c => c.kind === 'text').map(c => c.text).join('')).toBe(report)
  expect(requests.length).toBe(1)
})

test('a report with many steps lists the first ones, each cut short', async ($, on) => {
  const steps = Array.from({ length: 32 }, (_, i) => JSON.stringify({ kind: 'step', text: `Read src/f${i + 1}.rs` }))
  steps[0] = JSON.stringify({ kind: 'step', text: 'x'.repeat(130) })
  steps[1] = JSON.stringify({ kind: 'step', text: 'bash cat <<EOF\n  a\nEOF' })
  engine(on, [...steps, JSON.stringify({ kind: 'report', text: 'Done.' })], [])
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')
  const lines = JSON.parse(chunks.filter(c => c.kind === 'input').map(c => c.json).join('')).message.split('\n')
  expect(lines.slice(0, 6)).toEqual(['Done.', '', '---', 'Steps Codex ran (32):', '- ' + 'x'.repeat(119) + '…', '- bash cat <<EOF a EOF'])
  expect(lines.slice(-2)).toEqual(['- Read src/f30.rs', '- and 2 more'])
})

// The agent's transcript as `$.session.messages` gives it after the first turn: the task, the streamed text, the
// hand-back and its result.
const FIRST_TURN = [
  { role: 'user', text: 'Find the bug', toolUses: [] },
  { role: 'assistant', text: 'Codex on gpt-5.5, read-only\n\nThe bug is on line 12.', toolUses: [] },
  { role: 'assistant', text: '', toolUses: [{ tool_use_id: 'toolu_custom_t1_0', tool: 'SubagentHandback', input: { message: 'The bug is on line 12.\n\n---\nCodex ran no steps.' }, text: '{"success":true}' }] },
  { role: 'user', text: '', toolUses: [], toolResults: [{ tool_use_id: 'toolu_custom_t1_0', text: '{"success":true}', isError: false }] },
]
const FOLLOW_UP = 'This follows up on an earlier task of yours. That session is gone, so what came before is below.\n\n' +
  'The task:\nFind the bug\n\nYour report:\nThe bug is on line 12.\n\n---\nCodex ran no steps.\n\nThe new message:\nQuote the line.'

// Seen live: Claude Code spawns the agent again for a message sent to it, with that message as the prompt.
test('a message to a finished subagent reaches Codex with the task and the report before it', async ($, on) => {
  const requests: any[] = []
  engine(on, [JSON.stringify({ kind: 'report', text: 'The bug is on line 12.' })], requests)
  let transcript: any[] = [{ role: 'user', text: 'Find the bug', toolUses: [] }]
  on('session.messages', () => ({ value: transcript }))
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug' })
  await step($, 't1', 0, 'a1')
  transcript = [...FIRST_TURN, { role: 'user', text: 'Quote the line.', toolUses: [] }]
  await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Quote the line.' })
  await step($, 't2', 0, 'a1')
  expect(requests.map(r => r.input.prompt)).toEqual(['Find the bug', FOLLOW_UP])
})

test('a message to a subagent this module did not see start reaches Codex with the task and the report too', async ($, on) => {
  const requests: any[] = []
  engine(on, [JSON.stringify({ kind: 'report', text: 'Line 1.' })], requests)
  on('agent.list', () => ({ value: [{ id: 'a1', type: 'Claudiu:codex-gpt-5-5' }] }))
  on('session.messages', () => ({ value: [...FIRST_TURN, { role: 'user', text: 'Quote the line.', toolUses: [] }] }))
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await step($, 't2', 0, 'a1')
  expect(requests.map(r => r.input.prompt)).toEqual([FOLLOW_UP])
})

test('an OpenCode subagent runs the task in OpenCode and names it', async ($, on) => {
  const requests: any[] = []
  const subagents = JSON.stringify({
    codex: { edits: false, types: [type('codex-gpt-5-5', 'gpt-5.5')] },
    opencode: { edits: true, types: [type('opencode-openai-gpt-6-1-sol', 'openai/gpt-6.1-sol')] },
  })
  engine(on, [JSON.stringify({ kind: 'failed', reason: 'opencode was not found on PATH. OpenCode subagents need OpenCode installed.' })], requests, subagents)
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'Claudiu:opencode-openai-gpt-6-1-sol', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')

  expect(requests.map(r => r.input)).toEqual([{ agent: 'opencode', model: 'openai/gpt-6.1-sol', edits: true, cwd: '/work/sub', prompt: 'Find the bug' }])
  expect(chunks.filter(c => c.kind === 'text').map(c => c.text).join('')).toBe('OpenCode on openai/gpt-6.1-sol, edits allowed\n\n')
  const input = JSON.parse(chunks.filter(c => c.kind === 'input').map(c => c.json).join(''))
  expect(input.message).toBe('OpenCode failed: opencode was not found on PATH. OpenCode subagents need OpenCode installed.\n\n---\nOpenCode ran no steps.')
})

test('a failure is handed back as the report', async ($, on) => {
  engine(on, [JSON.stringify({ kind: 'failed', reason: "Codex isn't signed in. Run `codex login`, then try again." })], [])
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')
  const input = JSON.parse(chunks.filter(c => c.kind === 'input').map(c => c.json).join(''))
  expect(input.message).toBe("Codex failed: Codex isn't signed in. Run `codex login`, then try again.\n\n---\nCodex ran no steps.")
})

test('a helper that ends without an answer is a failure too', async ($, on) => {
  engine(on, ['not json'], [])
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')
  const input = JSON.parse(chunks.filter(c => c.kind === 'input').map(c => c.json).join(''))
  expect(input.message).toBe('Codex failed: the helper stopped without an answer.\n\n---\nCodex ran no steps.')
})

test('other subagents still run on Claude', async ($, on) => {
  const requests: unknown[] = []
  engine(on, [], requests)
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'general-purpose', prompt: 'Look around' })
  const chunks = await step($, 't1', 0, 'a1')
  expect(chunks.map(c => c.kind)).toEqual(['text'])
  expect(requests).toEqual([])
})

test('without a list from Claudiu, nothing is registered and an agent named like a Codex type still runs on Claude', async ($, on) => {
  const requests: unknown[] = []
  const registered = engine(on, [], requests, '') // an empty list reads as none, like a missing one
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  expect(registered).toEqual([])
  await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')
  expect(chunks.map(c => c.kind)).toEqual(['text'])
  expect(requests).toEqual([])
})
