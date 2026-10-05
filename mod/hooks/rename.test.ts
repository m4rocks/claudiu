import { expect, mock, test } from 'claude-code/testing'

test('applies a changed name with /rename, never the launch name, and again after /clear', async ($, on) => {
  const clock = mock.clock(on)
  mock.env(on, { CLAUDIU_NAME_FILE: 'names/s.txt' })
  let name = 'Launch name'
  let sid = 'claude-1'
  const renames: string[] = []
  on('fs.read', () => ({ value: name }))
  on('session.id', () => ({ value: sid }))
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('command.run', (_$, e) => {
    renames.push(`${e.command} ${e.args}`)
    return {}
  })

  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await clock.advance(2000)
  expect(renames).toEqual([])

  name = 'New name'
  await clock.advance(1000)
  expect(renames).toEqual(['rename New name'])
  await clock.advance(3000)
  expect(renames).toEqual(['rename New name'])

  sid = 'claude-2' // /clear starts a nameless Claude session
  await clock.advance(1000)
  expect(renames).toEqual(['rename New name', 'rename New name'])
})
