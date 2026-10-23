import { expect, mock, test } from 'claude-code/testing'

// The host hands back the path it resolved, with backslashes on Windows.
const isReply = (path: string) => path.replace(/\\/g, '/').endsWith('ask/s.res.json')
const usage = { input_tokens: 0, output_tokens: 0, cache_read_input_tokens: 0, cache_creation_input_tokens: 0 }

function setup($: any, on: any, request: () => string) {
  const clock = mock.clock(on)
  mock.env(on, { CLAUDIU_ASK_FILE: 'ask/s.json' })
  const writes: any[] = []
  const asked: any[] = []
  on('fs.read', () => ({ value: request() }))
  on('fs.write', (_$: any, e: any) => {
    writes.push({ path: e.path, body: JSON.parse(e.text) })
    return { value: undefined }
  })
  on('session.id', () => ({ value: 's1' }))
  on('session.model', () => ({ value: 'm' }))
  on('config.list', () => ({ value: [] }))
  on('settings.read', () => ({ value: {} }))
  on('session.start', (_$: any, e: any) => ({ cwd: e.cwd }))
  return { clock, writes, asked }
}

const soon = () => Math.floor(Date.now() / 1000) + 60

test('answers a request once, saying it is working first', async ($, on) => {
  let file = ''
  const { clock, writes, asked } = setup($, on, () => file)
  on('model.complete', (_$: any, e: any) => {
    asked.push(e)
    return { value: { isAnswered: true, text: 'Fix the thing', usage } }
  })
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await clock.advance(1000)
  expect(asked).toEqual([])

  file = JSON.stringify({ id: 'a1', model: 'haiku', system: 'You write.', prompt: 'Diff…', deadline: soon() })
  await clock.advance(500)
  const replies = writes.filter(w => isReply(w.path)).map(w => w.body)
  expect(replies).toEqual([
    { id: 'a1', state: 'working' },
    { id: 'a1', state: 'done', text: 'Fix the thing' },
  ])
  expect(asked.length).toBe(1)
  expect(asked[0]).toMatchObject({ prompt: 'Diff…', system: 'You write.' })

  await clock.advance(3000)
  expect(asked.length).toBe(1) // the same id is not asked twice
})

test('reports a failed call, and skips a request nobody waits for any more', async ($, on) => {
  let file = JSON.stringify({ id: 'old', prompt: 'p', deadline: 1 })
  const { clock, writes } = setup($, on, () => file)
  on('model.complete', () => ({ value: { isAnswered: false, reason: 'api-error', status: 529, error: 'overloaded', usage } }))
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await clock.advance(1000)
  expect(writes.filter(w => isReply(w.path))).toEqual([]) // stale

  file = JSON.stringify({ id: 'b2', prompt: 'p', deadline: soon() })
  await clock.advance(500)
  const last = writes.filter(w => isReply(w.path)).pop()!.body
  expect(last.state).toBe('error')
  expect(last.error).toContain('api-error 529')
})
