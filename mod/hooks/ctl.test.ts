import { expect, mock, test } from 'claude-code/testing'

test('ignores the sidebar commands that used to change Claude Code settings, and never writes them', async ($, on) => {
  mock.env(on, { CLAUDIU_CTL_FILE: 'ctl/me.json', CLAUDIU_USAGE_DIR: 'usage' })
  const clock = mock.clock(on)
  let file = JSON.stringify({ cmds: [] })
  const sets: unknown[] = []
  const ran: string[] = []
  on('fs.read', () => ({ value: file }))
  on('fs.write', () => ({ value: undefined }))
  on('session.id', () => ({ value: 's1' }))
  on('session.model', () => ({ value: 'claude-haiku-5-5' }))
  on('config.list', () => ({ value: [] }) as any)
  on('settings.read', () => ({ value: {} }))
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('config.set', (_$, e) => {
    sets.push([e.key, e.value])
    return { value: e.value }
  })
  on('command.run', (_$, e) => {
    ran.push(e.command + ' ' + e.args)
    return {}
  })

  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await clock.advance(1000)

  // Auto-compact, background compaction and advisor now reach Claude Code through the launch-time --settings file, so
  // a command for them (say, from an older Claudiu) must not save anything to the user's own config.
  file = JSON.stringify({
    cmds: [
      { seq: 1, key: 'auto_compact', value: false },
      { seq: 2, key: 'advisor', value: 'opus' },
      { seq: 3, key: 'precompute_compact', value: false },
    ],
  })
  await clock.advance(3000)
  expect(sets).toEqual([])
  expect(ran).toEqual([])
})

test('sends a message Claudiu asks for as the person’s own words, once', async ($, on) => {
  mock.env(on, { CLAUDIU_CTL_FILE: 'ctl/me.json', CLAUDIU_USAGE_DIR: 'usage' })
  const clock = mock.clock(on)
  let file = JSON.stringify({ cmds: [{ seq: 1, key: 'prompt', value: 'from an earlier run' }] })
  const sent: unknown[] = []
  on('fs.read', () => ({ value: file }))
  on('fs.write', () => ({ value: undefined }))
  on('session.id', () => ({ value: 's1' }))
  on('session.model', () => ({ value: 'claude-haiku-5-5' }))
  on('config.list', () => ({ value: [] }) as any)
  on('settings.read', () => ({ value: {} }))
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('prompt.submit', (_$, e) => {
    sent.push([e.text, e.origin])
    return { drop: 'test' }
  })

  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await clock.advance(1000)
  expect(sent).toEqual([])

  file = JSON.stringify({
    cmds: [
      { seq: 1, key: 'prompt', value: 'from an earlier run' },
      { seq: 2, key: 'prompt', value: '@src/a.rs#L3-9: Explain this code.' },
      { seq: 3, key: 'prompt', value: '   ' }, // nothing to say
      { seq: 4, key: 'prompt', value: 42 }, // not text
    ],
  })
  await clock.advance(1000)
  expect(sent).toEqual([['@src/a.rs#L3-9: Explain this code.', { kind: 'plugin', name: expect.anything(), asUser: true }]])

  await clock.advance(3000)
  expect(sent.length).toBe(1) // handled once
})
