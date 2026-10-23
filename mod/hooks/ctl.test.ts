import { expect, mock, test } from 'claude-code/testing'

test('applies the settings Claudiu asks for, once each, and not the ones sent before it started', async ($, on) => {
  mock.env(on, { CLAUDIU_CTL_FILE: 'ctl/me.json', CLAUDIU_USAGE_DIR: 'usage' })
  const clock = mock.clock(on)
  let file = JSON.stringify({ cmds: [{ seq: 5, key: 'advisor', value: 'sonnet' }] }) // from an earlier run
  const sets: unknown[] = []
  const ran: string[] = []
  const writes: { path: string; text: string }[] = []
  on('fs.read', () => ({ value: file }))
  on('fs.write', (_$, e) => {
    writes.push({ path: e.path, text: e.text })
    return { value: undefined }
  })
  on('session.id', () => ({ value: 's1' }))
  on('session.model', () => ({ value: 'claude-haiku-5-5' }))
  on('config.list', () => ({ value: [] }))
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
  expect(ran).toEqual([]) // seq 5 predates this process

  file = JSON.stringify({
    cmds: [
      { seq: 5, key: 'advisor', value: 'sonnet' },
      { seq: 6, key: 'auto_compact', value: false },
      { seq: 7, key: 'advisor', value: 'opus' },
      { seq: 8, key: 'advisor', value: 'rm -rf /' }, // not a model name
      { seq: 9, key: 'theme', value: 'light' }, // not a setting Claudiu sets
    ],
  })
  await clock.advance(1000)
  expect(sets).toEqual([['autoCompact', false]])
  expect(ran).toEqual(['advisor opus'])
  const snap = JSON.parse(writes.at(-1)!.text)
  expect(snap.advisor).toBe('opus')
  expect(snap.advisor_active).toBe('opus')
  expect(snap.auto_compact).toBe(false)

  await clock.advance(3000)
  expect(sets.length + ran.length).toBe(2) // handled once
})
