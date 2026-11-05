import { expect, mock, test } from 'claude-code/testing'

const TOOL = 'mcp__claudiu__show_visual'
const STEP = { turnId: 't', index: 0, answer: '', toolUses: [], stopReason: 'tool_use' as const, usage: null }

test('streams the Markdown of a show_visual call while it is written, then writes the finished card', async ($, on) => {
  mock.env(on, { CLAUDIU_VISUAL_PREFIX: 'visual/s--' })
  const writes: { path: string; card: any }[] = []
  on('fs.write', (_$, e) => {
    writes.push({ path: e.path, card: JSON.parse(e.text) })
    return { value: undefined }
  })
  on('turn.step', async function* () {
    yield { kind: 'tool', index: 1, id: 'toolu_1', name: TOOL }
    yield { kind: 'input', index: 1, json: '{"title":"Pla' }
    yield { kind: 'input', index: 1, json: 'n","content":"# Hi\\n\\nsome **bo' }
    yield { kind: 'input', index: 1, json: 'ld** \\u00e9' }
    return STEP
  })

  const chunks: string[] = []
  for await (const c of $.turn.step({ turnId: 't', index: 0, model: 'm', messageCount: 1 })) chunks.push(c.kind)
  expect(chunks).toEqual(['tool', 'input', 'input', 'input']) // nothing is held back or changed

  expect(writes.length).toBeGreaterThanOrEqual(1)
  const first = writes[0]!
  expect(first.path.replace(/\\/g, '/').endsWith('visual/s--toolu_1.json')).toBe(true) // Windows paths use backslashes
  expect(first.card.call).toBe('toolu_1')

  const result = await $.tool.call({ tool: TOOL, tool_use_id: 'toolu_1', title: 'Plan', content: '# Hi\n\nsome **bold** é' })
  expect(result.isError).toBeFalsy()
  expect(writes.at(-1)!.card).toEqual({ call: 'toolu_1', content: '# Hi\n\nsome **bold** é' })
})

test('refuses an empty card and says so when Claudiu gave no file', async ($, on) => {
  mock.env(on, { CLAUDIU_VISUAL_PREFIX: 'visual/s--' })
  on('fs.write', () => ({ value: undefined }))
  const empty = await $.tool.call({ tool: TOOL, tool_use_id: 'x', content: '  ' })
  expect(empty.isError).toBe(true)
})

test('other tools and subagents are not streamed', async ($, on) => {
  mock.env(on, { CLAUDIU_VISUAL_PREFIX: 'visual/s--' })
  let writes = 0
  on('fs.write', () => {
    writes += 1
    return { value: undefined }
  })
  on('turn.step', async function* () {
    yield { kind: 'tool', index: 0, id: 'a', name: 'Bash' }
    yield { kind: 'input', index: 0, json: '{"command":"ls"}' }
    return STEP
  })
  for await (const _ of $.turn.step({ turnId: 't', index: 0, model: 'm', messageCount: 1 })) void _
  for await (const _ of $.turn.step({ turnId: 't', index: 0, model: 'm', messageCount: 1, agentId: 'sub' })) void _
  expect(writes).toBe(0)
})

const ROW = { tool: TOOL, tool_use_id: 'toolu_9', input: { title: 'Plan', content: '# Old card' }, isRunning: false, isErrored: false, isInterrupted: false }
const MOUNT = { plugin: 'Claudiu', surface: 'terminal' as const, component: 'ToolUse' as const, requestId: 'toolu_9', props: ROW }

test('a drawn call reserves linked rows and writes its card back from the transcript', async ($, on) => {
  mock.env(on, { CLAUDIU_VISUAL_PREFIX: 'visual/s--' })
  const writes: { path: string; text: string }[] = []
  on('fs.write', (_$, e) => {
    writes.push({ path: e.path, text: e.text })
    return { value: undefined }
  })
  const ui = await $.ui.mount(MOUNT)
  const link = await ui.find({ type: 'Link' })
  expect(link?.props?.href).toBe('claudiu://visual/toolu_9/0')
  expect((await ui.find({ type: 'Text', text: /Plan/ }))?.text).toBe('Plan')
  await ui.unmount()
  await new Promise(done => setTimeout(done, 10))
  const card = writes.find(w => w.path.includes('toolu_9'))
  expect(JSON.parse(card!.text)).toEqual({ call: 'toolu_9', content: '# Old card' })
})
