import { expect, mock, test } from 'claude-code/testing'

test('applies a changed name with /rename, never the launch name, and again after /clear', async ($, on) => {
  const clock = mock.clock(on)
  mock.env(on, { M4CODE_NAME_FILE: 'names/s.txt' })
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

test('a name the user gave with /rename is not applied again when m4code writes it back', async ($, on) => {
  const clock = mock.clock(on)
  mock.env(on, { M4CODE_NAME_FILE: 'names/s.txt' })
  let name = 'Launch name'
  const renames: string[] = []
  on('fs.read', () => ({ value: name }))
  on('session.id', () => ({ value: 'claude-1' }))
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('command.run', (_$, e) => {
    renames.push(`${e.command} ${e.args}`)
    return {}
  })

  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  await $.command.run({ command: 'rename', args: ' Typed name ', origin: { kind: 'composer' }, presentation: { isFullscreen: false, columns: 80 } })
  name = 'Typed name' // m4code adopted it from the transcript and wrote it back
  await clock.advance(3000)
  expect(renames).toEqual(['rename  Typed name '])
})
