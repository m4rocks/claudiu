import { expect, mock, test } from 'claude-code/testing'

type Engine = { idle: boolean }

// A session whose ctl file gets Claudiu's hand-off request. `idle` false is a turn running.
async function handOff($: any, on: any, engine: Engine) {
  mock.env(on, { CLAUDIU_CTL_FILE: 'ctl/me.json', CLAUDIU_USAGE_DIR: 'usage' })
  const clock = mock.clock(on)
  let file = '{}'
  const ran: string[] = []
  const compactions: string[] = []
  const writes: any[] = []
  on('fs.read', () => ({ value: file }))
  on('fs.write', (_$: any, e: any) => {
    writes.push(JSON.parse(e.text))
    return { value: undefined }
  })
  on('session.id', () => ({ value: 's1' }))
  on('session.model', () => ({ value: 'claude-opus-5-5' }))
  on('config.list', () => ({ value: [] }))
  on('settings.read', () => ({ value: {} }))
  on('session.start', (_$: any, e: any) => ({ cwd: e.cwd }))
  // What Claude Code does after the person stopped a turn: a plugin's slash command is refused until they type.
  on('command.run', (_$: any, e: any) => {
    if (engine.idle) throw new Error('refused: the session was stopped and nobody has sent input since')
    ran.push(e.command)
    return {}
  })
  on('session.compact', (_$: any, e: any) => {
    if (!engine.idle) throw new Error('a turn is running')
    compactions.push(e.trigger)
    return { messages: [{ role: 'user', text: 'summary', toolUses: [] }] }
  })

  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await clock.advance(1000)
  file = JSON.stringify({ cmds: [{ seq: 1, key: 'compact', value: true }] })
  await clock.advance(1000)
  return { ran, compactions, compactedAt: writes.at(-1)?.compacted_at ?? null }
}

test('a hand-off compacts an idle session, even after the person stopped its last turn, and says so', async ($, on) => {
  const r = await handOff($, on, { idle: true })
  expect(r.compactions.length).toBe(1)
  expect(r.compactedAt).not.toBe(null)
})

test('a hand-off during a turn queues /compact for when the turn ends', async ($, on) => {
  const r = await handOff($, on, { idle: false })
  expect(r.ran).toEqual(['compact'])
})
