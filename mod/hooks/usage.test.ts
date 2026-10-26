import { expect, mock, test } from 'claude-code/testing'

test('reports context, limits, auto-compact and advisor; rewrites only on change', async ($, on) => {
  mock.env(on, { CLAUDIU_USAGE_DIR: 'usage', CLAUDIU_ACCOUNT: 'work' })
  let advisor: string | undefined = 'opus'
  const writes: { path: string; snap: any }[] = []
  on('fs.write', (_$, e) => {
    writes.push({ path: e.path, snap: JSON.parse(e.text) })
    return { value: undefined }
  })
  on('session.id', () => ({ value: 'abc/123' }))
  on('session.model', () => ({ value: 'claude-haiku-5-5' }))
  on('config.list', () => ({ value: [{ key: 'autoCompact', label: 'Auto-compact', kind: 'boolean', value: true, provider: { plugin: 'Claudiu', tier: 'user' }, isLocked: false }] }))
  on('settings.read', () => ({ value: { advisorModel: advisor } }))
  let compactAt: number | undefined = 167_000
  on('session.usage', () => ({
    value: { startedAt: 0, rateLimits: [], cost: { usd: 0 }, context: { window: 1_000_000, breakdown: { isAutoCompactEnabled: compactAt != null, autoCompactThreshold: compactAt } } },
  }) as any)
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('command.run', () => ({}))
  on('session.measure', (_$, e) => ({ changed: e.changed }))

  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  expect(writes.length).toBe(1)
  expect(writes[0]!.path).toMatch(/usage[\\/]abc_123\.json$/) // the session id cannot escape the directory
  expect(writes[0]!.snap).toMatchObject({ session_id: 'abc/123', model: 'claude-haiku-5-5', account: 'work', auto_compact: true, advisor: 'opus', advisor_active: 'opus', compact_at: 167_000, context_fraction: null })
  expect(writes[0]!.snap.precompute_compact).toBe(null) // Claude Code does not offer the switch here

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

  compactAt = undefined // auto-compact switched off
  await $.command.run({ command: 'autocompact', args: 'off', origin: { kind: 'composer' }, presentation: { isFullscreen: false, columns: 80 } })
  expect(writes.at(-1)!.snap.compact_at).toBe(null)

  advisor = undefined
  await $.command.run({ command: 'advisor', args: ' off ', origin: { kind: 'composer' }, presentation: { isFullscreen: false, columns: 80 } })
  expect(writes.at(-1)!.snap.advisor).toBe(null)
  advisor = 'claude-opus-5-5' // a resolved id still reads as the Opus alias
  await $.session.measure(measure as any)
  expect(writes.at(-1)!.snap.advisor).toBe('opus')

  advisor = 'sonnet' // saved now, but the running conversation keeps Opus until compaction
  await $.session.measure(measure as any)
  expect(writes.at(-1)!.snap).toMatchObject({ advisor: 'sonnet', advisor_active: 'opus' })
  expect(JSON.stringify(writes.at(-1)!.snap)).not.toContain('command') // what the user types is not reported
})

test('reports the auto-compact threshold of the model the session switched to', async ($, on) => {
  // Each model has its own compaction window, so a cached threshold must not outlive a model change.
  mock.env(on, { CLAUDIU_USAGE_DIR: 'usage' })
  let model = 'claude-sonnet-5-5'
  const windows: Record<string, number> = { 'claude-sonnet-5-5': 367_000, 'claude-haiku-5-5': 67_000 }
  const writes: any[] = []
  on('fs.write', (_$, e) => {
    writes.push(JSON.parse(e.text))
    return { value: undefined }
  })
  on('session.id', () => ({ value: 's1' }))
  on('session.model', () => ({ value: model }))
  on('config.list', () => ({ value: [] }))
  on('settings.read', () => ({ value: {} }))
  on('session.usage', () => ({
    value: { startedAt: 0, rateLimits: [], cost: { usd: 0 }, context: { window: 1_000_000, breakdown: { isAutoCompactEnabled: true, autoCompactThreshold: windows[model] } } },
  }) as any)
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('session.measure', (_$, e) => ({ changed: e.changed }))

  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  expect(writes.at(-1).compact_at).toBe(367_000)

  model = 'claude-haiku-5-5'
  await $.session.measure({ context: { tokens: 1000, window: 1_000_000, percent: 0 }, rateLimits: [], changed: ['context'] } as any)
  expect(writes.at(-1).compact_at).toBe(67_000)
})
