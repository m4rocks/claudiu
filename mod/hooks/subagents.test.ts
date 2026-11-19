import { expect, mock, test } from 'claude-code/testing'

// What `claude/subagents.rs` lists for a type.
function type(name: string, model: string) {
  return { name, model, description: `Runs the task on ${model}.`, prompt: 'Say the mod lost track.' }
}

const SUBAGENTS = JSON.stringify({ codex: { name: 'Codex', edits: false, types: [type('codex-gpt-5-5', 'gpt-5.5')] } })

// What the engine answers beneath the mod for a session with Codex on. `helper` is what `claudiu --acp` writes;
// `requests` collects what the mod sent it.
function engine(on: any, helper: string[] | (() => AsyncGenerator<{ stream: string, text: string }, unknown>), requests: unknown[], subagents = SUBAGENTS) {
  mock.env(on, { CLAUDIU_SUBAGENTS: subagents, CLAUDIU_EXE: '/apps/claudiu' })
  mock.clock(on)
  on('fs.read', () => ({ value: '' }))
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
    if (typeof helper === 'function') return yield* helper()
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

// The subagent's own view streams its activity; the main conversation gets none of it, since one line per step
// buried the person's own conversation.
test('activity streams in the subagent and leaves the main conversation alone', async ($, on) => {
  const log: string[] = []
  on('ui.log', (_$: any, e: any) => { log.push(e.text); return { value: undefined } })
  engine(on, async function* () {
    yield { stream: 'stdout', text: '{"kind":"progress","tex' }
    yield { stream: 'stdout', text: 't":"Session ready."}\n{"kind":"thought","text":"Checking "}\n' }
    yield { stream: 'stdout', text: '{"kind":"thought","text":"the parser."}\n{"kind":"step","text":"Run tests"}\n' }
    yield { stream: 'stdout', text: '{"kind":"progress","text":"Run tests · failed"}\n{"kind":"output","text":"\\u001b[31mtest failed\\u001b[0m"}\n' }
    yield { stream: 'stdout', text: '{"kind":"report","text":"Parser test failed."}\n' }
    return { value: { code: 0, signal: null } }
  }, [])
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')

  expect(log).toEqual([])
  const text = chunks.filter(c => c.kind === 'text').map(c => c.text).join('')
  expect(text).toContain('▸ Session ready.')
  expect(text).toContain('Thinking\nChecking the parser.')
  expect(text).toContain('▸ Run tests · failed')
  expect(text).toContain('Tool output\ntest failed')
  expect(JSON.parse(chunks.find(c => c.kind === 'input').json).message).toBe('Parser test failed.\n\n---\nSteps Codex ran (1), outside Claude Code, which counts them as 0 tool uses:\n- Run tests')
})

async function step($: any, turnId: string, index: number, agentId?: string) {
  const chunks: any[] = []
  const stream = $.turn.step({ turnId, index, model: 'haiku', messageCount: 1, agentId })
  for await (const c of stream) chunks.push(c)
  return chunks
}

function gate<T>() {
  let resolve!: (value: T) => void
  const promise = new Promise<T>(r => { resolve = r })
  return { promise, resolve }
}

const INBOX_ID = 'be4bebed-a8ee-4afb-b7a1-c06d33ff3083'
const INBOX_PATH = `/data/acp/${INBOX_ID}.json`
const helperChunk = (event: object) => ({ stream: 'stdout', text: JSON.stringify(event) + '\n' })
const messageTo = (agentId: string, text: string) => ({ agentId, origin: { kind: 'coordinator' as const }, text })

test('SendMessage reaches only the running Codex helper and its reaction streams before handback', async ($, on) => {
  const ready = gate<void>()
  const first = gate<void>()
  const second = gate<void>()
  const finish = gate<void>()
  const reaction = gate<void>()
  const writes: any[] = []
  let finished = false
  engine(on, async function* () {
    yield helperChunk({ kind: 'inbox', id: INBOX_ID, path: INBOX_PATH })
    yield helperChunk({ kind: 'progress', text: 'Waiting for input.' })
    ready.resolve()
    await first.promise
    yield helperChunk({ kind: 'accepted', seq: 1 })
    await second.promise
    yield helperChunk({ kind: 'accepted', seq: 2 })
    yield helperChunk({ kind: 'progress', text: 'Codex received the message in its current turn.' })
    yield helperChunk({ kind: 'text', text: 'I will check both files.' })
    reaction.resolve()
    await finish.promise
    yield helperChunk({ kind: 'report', text: 'Checked both files.' })
    return { value: { code: 0, signal: null } }
  }, [])
  on('fs.write', (_$: any, e: any) => {
    writes.push({ path: e.path.replace(/\\/g, '/').replace(/^[A-Za-z]:/, ''), body: JSON.parse(e.text) })
    const written = writes.length === 1 ? first : second
    written.resolve()
    return { value: undefined }
  })
  on('session.receive', (_$: any, e: any) => ({ text: e.text }))
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug' })
  const running = step($, 't1', 0, 'a1').then(chunks => { finished = true; return chunks })
  await ready.promise
  expect(await $.session.receive(messageTo('someone-else', 'Other task'))).toEqual({ text: 'Other task' })
  expect(await $.session.receive(messageTo('a1', 'x'.repeat(20_001)))).toEqual({ text: 'x'.repeat(20_001) })
  const deliveries = await Promise.all([
    $.session.receive(messageTo('a1', 'Check the parser.')),
    $.session.receive(messageTo('a1', 'Check the tests too.')),
  ])
  expect(deliveries).toEqual([{ consumed: 'Delivered to the subagent helper' }, { consumed: 'Delivered to the subagent helper' }])
  expect(writes).toEqual([
    { path: INBOX_PATH, body: { seq: 1, text: 'Check the parser.' } },
    { path: INBOX_PATH, body: { seq: 2, text: 'Check the tests too.' } },
  ])
  await reaction.promise
  expect(finished).toBe(false)
  finish.resolve()
  const chunks = await running
  expect(chunks.filter(c => c.kind === 'text').map(c => c.text).join('')).toContain('I will check both files.')
  expect(await $.session.receive(messageTo('a1', 'Now summarize.'))).toEqual({ text: 'Now summarize.' })
})

test('a delivery racing helper exit is queued by Claude and its inbox is cleaned up', async ($, on) => {
  const ready = gate<void>()
  const wrote = gate<void>()
  const cleanup: string[][] = []
  engine(on, async function* () {
    yield helperChunk({ kind: 'inbox', id: INBOX_ID, path: INBOX_PATH })
    yield helperChunk({ kind: 'progress', text: 'Waiting for input.' })
    ready.resolve()
    await wrote.promise
    yield helperChunk({ kind: 'report', text: 'Finished first.' })
    return { value: { code: 0, signal: null } }
  }, [])
  on('fs.write', () => { wrote.resolve(); return { value: undefined } })
  on('process.run', (_$: any, e: any) => {
    cleanup.push(e.argv)
    return { value: { exitCode: 0, stdout: '', stderr: '', isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('session.receive', (_$: any, e: any) => ({ text: e.text }))
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug' })
  const running = step($, 't1', 0, 'a1')
  await ready.promise
  expect(await $.session.receive(messageTo('a1', 'Check one more file.'))).toEqual({ text: 'Check one more file.' })
  await running
  expect(cleanup).toEqual([['/apps/claudiu', '--acp', '--cleanup', INBOX_ID]])
})

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

  expect(requests).toEqual([{ argv: ['/apps/claudiu', '--acp'], input: { agent: 'codex', model: 'gpt-5.5', effort: null, edits: false, cwd: '/work/sub', prompt: 'Find the bug', live_messages: true } }])
  // What Codex writes and does is the agent's own text, which its transcript keeps; Claude gets only the report.
  const text = chunks.filter(c => c.kind === 'text').map(c => c.text).join('')
  expect(text).toBe('Codex on gpt-5.5, read-only\n\nLooking.\n▸ Read src/a.rs\n')
  const tool = chunks.find(c => c.kind === 'tool')
  expect(tool.name).toBe('SubagentHandback')
  // The report names the steps, since Claude Code counts none of Codex's tool calls as the subagent's.
  const report = 'The bug is on line 12.\n\n---\nSteps Codex ran (1), outside Claude Code, which counts them as 0 tool uses:\n- Read src/a.rs'
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

test('the effort Claude asks for in its Agent call reaches Codex', async ($, on) => {
  const requests: any[] = []
  engine(on, [JSON.stringify({ kind: 'report', text: 'Done.' })], requests)
  // Claude Code's Agent tool spawns the agent while the call runs.
  let spawned = gate<void>()
  let callId = ''
  on('tool.call', { tool: 'Agent' }, async (_$: any, e: any) => {
    callId = e.tool_use_id
    await spawned.promise
    return { result: 'started' }
  })
  async function agentCall(effort: string) {
    callId = ''
    spawned = gate<void>()
    const call = $.tool.call({ tool: 'Agent', subagent_type: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug', description: 'Find it', effort })
    while (!callId) await new Promise(r => setTimeout(r, 1))
    await $.agent.spawn({ subagentType: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug', tool_use_id: callId } as any)
    spawned.resolve()
    await call
  }
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await agentCall('high')
  await step($, 't1', 0, 'a1')
  // The same agent spawned again asks for its own effort.
  await agentCall('low')
  await step($, 't2', 0, 'a1')
  expect(requests.map(r => r.input.effort)).toEqual(['high', 'low'])
})

test('a first turn that starts before its spawn is noted still gets the effort its Agent call asked for', async ($, on) => {
  const requests: any[] = []
  engine(on, [JSON.stringify({ kind: 'report', text: 'Done.' })], requests)
  on('agent.list', () => ({ value: [{ id: 'a1', type: 'Claudiu:codex-gpt-5-5', description: 'Find it' }] }))
  const finished = gate<void>()
  on('tool.call', { tool: 'Agent' }, async () => {
    await finished.promise
    return { result: 'done' }
  })
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  const call = $.tool.call({ tool: 'Agent', subagent_type: 'Claudiu:codex-gpt-5-5', prompt: 'Find the bug', description: 'Find it', effort: 'high' })
  // The agent's turn runs while the call does, before this module has seen its spawn.
  await new Promise(r => setTimeout(r, 1))
  await step($, 't1', 0, 'a1')
  finished.resolve()
  await call
  expect(requests.map(r => r.input.effort)).toEqual(['high'])
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
  expect(lines.slice(0, 6)).toEqual(['Done.', '', '---', 'Steps Codex ran (32), outside Claude Code, which counts them as 0 tool uses:', '- ' + 'x'.repeat(119) + '…', '- bash cat <<EOF a EOF'])
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
    opencode: { name: 'OpenCode', edits: true, types: [type('opencode-openai-gpt-6-1-sol', 'openai/gpt-6.1-sol')] },
  })
  engine(on, [JSON.stringify({ kind: 'failed', reason: 'opencode was not found on PATH. OpenCode subagents need OpenCode installed.' })], requests, subagents)
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'Claudiu:opencode-openai-gpt-6-1-sol', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')

  expect(requests.map(r => r.input)).toEqual([{ agent: 'opencode', model: 'openai/gpt-6.1-sol', effort: null, edits: true, cwd: '/work/sub', prompt: 'Find the bug', live_messages: true }])
  expect(chunks.filter(c => c.kind === 'text').map(c => c.text).join('')).toBe('OpenCode on openai/gpt-6.1-sol, edits allowed\n\n')
  const input = JSON.parse(chunks.filter(c => c.kind === 'input').map(c => c.json).join(''))
  expect(input.message).toBe('OpenCode failed: opencode was not found on PATH. OpenCode subagents need OpenCode installed.\n\n---\nOpenCode ran no steps.')
})

test('Antigravity uses the display name supplied by Claudiu', async ($, on) => {
  const requests: any[] = []
  const registered = engine(on, [JSON.stringify({ kind: 'report', text: 'Done.' })], requests, JSON.stringify({
    antigravity: { name: 'Antigravity', edits: false, types: [type('antigravity-gemini-4-neon', 'gemini-4-neon')] },
  }))
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  expect(registered.map(r => r.name)).toEqual(['antigravity-gemini-4-neon'])
  await $.agent.spawn({ subagentType: 'Claudiu:antigravity-gemini-4-neon', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')
  expect(requests[0].input.agent).toBe('antigravity')
  expect(chunks.filter(c => c.kind === 'text').map(c => c.text).join('')).toBe('Antigravity on gemini-4-neon, read-only\n\n')
  expect(JSON.parse(chunks.find(c => c.kind === 'input').json).message).toContain('Antigravity ran no steps.')
})

test('a provider without a display name falls back to its key', async ($, on) => {
  engine(on, [JSON.stringify({ kind: 'report', text: 'Done.' })], [], JSON.stringify({
    fourth: { edits: false, types: [type('fourth-model', 'model')] },
  }))
  await $.session.start({ cwd: '/work', surface: 'terminal', isInteractive: true })
  await $.agent.spawn({ subagentType: 'Claudiu:fourth-model', prompt: 'Find the bug' })
  const chunks = await step($, 't1', 0, 'a1')
  expect(chunks.filter(c => c.kind === 'text').map(c => c.text).join('')).toBe('fourth on model, read-only\n\n')
  expect(JSON.parse(chunks.find(c => c.kind === 'input').json).message).toContain('fourth ran no steps.')
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
  engine(on, ['not json', '{"kind":"thought"}', '{"kind":"report","text":null}'], [])
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
