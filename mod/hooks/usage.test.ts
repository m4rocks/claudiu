import { expect, mock, test } from 'claude-code/testing'

test('reports context, limits, auto-compact, advisor and the typed command; rewrites only on change', async ($, on) => {
  mock.env(on, { CLAUDIU_USAGE_DIR: 'usage', CLAUDIU_ACCOUNT: 'work' })
  let advisor: string | undefined = 'opus'
  const writes: { path: string; snap: any }[] = []
  on('fs.write', (_$, e) => {
    writes.push({ path: e.path, snap: JSON.parse(e.text) })
    return { value: undefined }
  })
  on('session.id', () => ({ value: 'abc/123' }))
  on('session.model', () => ({ value: 'claude-haiku-5-5' }))
  on('config.list', () => ({ value: [{ key: 'autoCompact', label: 'Auto-compact', kind: 'boolean', value: true }] }))
  on('settings.read', () => ({ value: { advisorModel: advisor } }))
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('command.run', () => ({}))
  on('session.measure', (_$, e) => ({ changed: e.changed }))

  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  expect(writes.length).toBe(1)
  expect(writes[0].path).toMatch(/usage[\\/]abc_123\.json$/) // the session id cannot escape the directory
  expect(writes[0].snap).toMatchObject({ session_id: 'abc/123', model: 'claude-haiku-5-5', account: 'work', auto_compact: true, advisor: 'opus', context_fraction: null })

  const measure = {
    context: { tokens: 28000, window: 1_000_000, percent: 3 },
    rateLimits: [
      { kind: 'five_hour', percentUsed: 67, resetsAt: '2026-10-22T09:00:00.000Z' },
      { kind: 'seven_day', percentUsed: 22, resetsAt: '2026-10-28T18:00:00.000Z' },
    ],
    changed: ['context', 'rateLimits'],
  } as const
  await $.session.measure(measure as any)
  const s = writes.at(-1)!.snap
  expect(Math.abs(s.context_fraction - 0.028)).toBeLessThan(1e-6)
  expect([s.context_tokens, s.context_window]).toEqual([28000, 1_000_000])
  expect(s.five_hour).toMatchObject({ utilization: 0.67, resets_at: 1792659600 })
  expect(s.seven_day).toMatchObject({ utilization: 0.22, resets_at: 1793210400 })

  const n = writes.length
  await $.session.measure(measure as any)
  expect(writes.length).toBe(n) // nothing changed: nothing rewritten

  advisor = undefined
  await $.command.run({ command: 'advisor', args: ' off ' })
  expect(writes.at(-1)!.snap).toMatchObject({ advisor: null, last_command: 'advisor off' })

  await $.command.run({ command: 'compact', args: 'keep my private notes' })
  expect(writes.at(-1)!.snap.last_command).toBe('compact') // free-text arguments are not reported
})
