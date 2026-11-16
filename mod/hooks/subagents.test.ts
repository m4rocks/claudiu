import { expect, mock, test } from 'claude-code/testing'

const SUBAGENTS = JSON.stringify({ codex: { edits: false, types: [{ name: 'codex-gpt-5-5', model: 'gpt-5.5' }] } })

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
}

async function step($: any, turnId: string, index: number, agentId?: string) {
  const chunks: any[] = []
  const stream = $.turn.step({ turnId, index, model: 'haiku', messageCount: 1, agentId })
  for await (const c of stream) chunks.push(c)
  return chunks
}

test('a Codex subagent runs the task in Codex and hands back its last message', async ($, on) => {
  const requests: any[] = []
  engine(on, [
    JSON.stringify({ kind: 'text', text: 'Looking.' }),
    JSON.stringify({ kind: 'step', text: 'Read src/a.rs' }),
    JSON.stringify({ kind: 'report', text: 'The bug is on line 12.' }),
  ], requests)

  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'codex-gpt-5-5', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')

  expect(requests).toEqual([{ argv: ['/apps/claudiu', '--acp'], input: { model: 'gpt-5.5', edits: false, cwd: '/work/sub', prompt: 'Find the bug' } }])
  // What Codex writes and does is the agent's own text, which its transcript keeps; Claude gets only the report.
  const text = chunks.filter(c => c.kind === 'text').map(c => c.text).join('')
  expect(text).toBe('Codex on gpt-5.5, read-only\n\nLooking.\n▸ Read src/a.rs\n')
  const tool = chunks.find(c => c.kind === 'tool')
  expect(tool.name).toBe('SubagentHandback')
  const input = chunks.filter(c => c.kind === 'input').map(c => c.json).join('')
  expect(JSON.parse(input)).toEqual({ message: 'The bug is on line 12.' })

  // A second step of the same turn (a refused hand-back) hands back the same report without running Codex again.
  const again = await step($, 't1', 1, 'a1')
  expect(requests.length).toBe(1)
  expect(JSON.parse(again.filter(c => c.kind === 'input').map(c => c.json).join(''))).toEqual({ message: 'The bug is on line 12.' })

  // A hand-back refused again ends the turn with the report as text instead of asking forever.
  const last = await step($, 't1', 2, 'a1')
  expect(last.some(c => c.kind === 'tool')).toBe(false)
  expect(last.filter(c => c.kind === 'text').map(c => c.text).join('')).toBe('The bug is on line 12.')
  expect(requests.length).toBe(1)
})

test('a failure is handed back as the report', async ($, on) => {
  engine(on, [JSON.stringify({ kind: 'failed', reason: "Codex isn't signed in. Run `codex login`, then try again." })], [])
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'codex-gpt-5-5', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')
  const input = JSON.parse(chunks.filter(c => c.kind === 'input').map(c => c.json).join(''))
  expect(input.message).toBe("Codex failed: Codex isn't signed in. Run `codex login`, then try again.")
})

test('a helper that ends without an answer is a failure too', async ($, on) => {
  engine(on, ['not json'], [])
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'codex-gpt-5-5', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')
  const input = JSON.parse(chunks.filter(c => c.kind === 'input').map(c => c.json).join(''))
  expect(input.message).toBe('Codex failed: the helper stopped without an answer.')
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

test('without a list from Claudiu, an agent named like a Codex type still runs on Claude', async ($, on) => {
  const requests: unknown[] = []
  engine(on, [], requests, '') // an empty list reads as none, like a missing one
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'codex-gpt-5-5', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')
  expect(chunks.map(c => c.kind)).toEqual(['text'])
  expect(requests).toEqual([])
})
