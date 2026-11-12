// Loaded per process through `--plugin-dir`. Claudiu writes nothing to the user's Claude Code config.
//
// What this mod does:
// - Model and effort: answers Claudiu's `set_model` tool in-process, so the override applies from the next request,
//   even mid-turn. It then runs /model and /effort once Claude Code is idle, so the session (and what /model shows)
//   switches too. Claude Code saves those as the user's default.
// - Session name: applies the name Claudiu writes to `CLAUDIU_NAME_FILE` with /rename, which Claude Code queues
//   until the session is idle.
// - Sidebar settings: applies auto-compact, background compaction and advisor changes sent through
//   `CLAUDIU_CTL_FILE`, the same as the user choosing them in /config or typing /advisor (which Claude Code saves).
// - Messages: sends a message Claudiu asks for through the same file (the editor's Ask Claude) as the user's own words,
//   when the session is idle, without typing into the terminal. (Claudiu itself pastes the editor's `@file#L`
//   reference into Claude's input box; that is not done here.)
// - Usage and status: reports context, the 5-hour and 7-day limits, auto-compact, background compaction, advisor and
//   the last command to `CLAUDIU_USAGE_DIR/<claude session id>.json`. No tokens are spent.
// - Cards: answers `show_visual` in-process and, while the model is still writing the call, streams its Markdown to
//   `CLAUDIU_VISUAL_PREFIX<call>.json`. Under each call it reserves rows, each with a link naming the call, and Claudiu
//   paints the live card over them. Claudiu's measured height comes back in `CLAUDIU_VISUAL_PREFIX<call>.rows`.
// - Auto effort: when Claudiu launched the session with `CLAUDIU_AUTO_EFFORT`, asks Haiku before each prompt which
//   effort it needs, within that range, and applies it per request (never with /effort, which saves the user's default).
// - One-shot completions: runs requests from `CLAUDIU_ASK_FILE` (commit messages, session titles) with
//   `$.model.complete`, on this session's own client, so Claudiu needs no `claude -p`.

// ---- Constants ----------------------------------------------------------------------------------------------------

// The aliases `set_model` accepts. A full `claude-…` id is accepted too.
const MODELS = {
  haiku: 'claude-haiku-5-5',
  sonnet: 'claude-sonnet-5-5',
  opus: 'claude-opus-5-5',
  fable: 'claude-fable-5-1',
}
const CARD_ROWS = 6 // reserved for a card until Claudiu has measured it
const PROMPT_MAX = 20000 // characters of a message Claudiu asks the session to send (the editor's Ask Claude)
const EFFORTS = ['low', 'medium', 'high', 'xhigh', 'max']
const ASK_MODELS = ['haiku', 'sonnet', 'opus']
const VISUAL_TOOL = 'mcp__claudiu__show_visual'
// The /config row for background compaction ("Precompute compaction"). Claude Code lists it only while a server-side
// flag enables the feature; without the row the setting does nothing, so Claudiu offers the switch only with it.
const PRECOMPUTE_ROW = 'precomputeCompactionEnabled'

// ---- Per-process state --------------------------------------------------------------------------------------------

let want = {} // { model?, effort? } from Claudiu, applied to each turn step until Claude Code has run /model and /effort
let named = null // `${claude session id}\n${name}` last applied with /rename
let namePoll // watches CLAUDIU_NAME_FILE
let ctlPoll // watches CLAUDIU_CTL_FILE
let ctlSeen = null // highest command number handled; null until the file was first read (older ones are not replayed)
let pendingAdvisor = null // an advisor change Claude Code refused (see setAdvisor), tried again until it goes through
let askPoll // watches CLAUDIU_ASK_FILE
let askSeen = null // id of the last request looked at
let asking = false // a completion is running; a poll that finds another request waits for it
const cardRows = new Map() // show_visual call id -> rows reserved for its card
const published = new Set() // call ids whose card file this process wrote from the transcript
let rowsPoll // watches the heights Claudiu measures
let visualChain = Promise.resolve() // visual writes in order, without holding up the stream
let lastVisual = null // id of the show_visual call being written, or last written

// ---- Usage report -------------------------------------------------------------------------------------------------

let measured = null // { context, limits: { five_hour, seven_day } } from the last session.measure
let written = '' // what the report file holds, minus times; an unchanged report is not rewritten (a write is cheap, a redraw is not)
let activeAdvisor = null // the advisor the current conversation declared; a model change waits for /clear or /compact
let activeSession = null // the Claude session id activeAdvisor belongs to (/clear starts a new one)

// The token count auto-compaction runs at (null while it is off). It only changes with settings, the model or the
// environment, and the breakdown it comes from is a local estimate, so it is read again at most once a minute, after
// a setting changed, or when the model changed: each model has its own compaction window.
let compactAt = null
let compactRead = 0
let compactModel = null // the model compactAt belongs to


// A session id as a file name.
const stem = id => String(id).replace(/[^A-Za-z0-9_-]/g, '_')

// An ISO time as whole seconds, or null when it does not parse.
function secs(iso) {
  const t = Date.parse(iso)
  return Number.isFinite(t) ? Math.floor(t / 1000) : null
}

async function threshold($, fresh) {
  const model = await $.session.model().catch(() => null)
  if (!fresh && model === compactModel && Date.now() - compactRead < 60_000) return compactAt
  compactRead = Date.now()
  compactModel = model
  const usage = await $.session.usage({ breakdown: 'summary' }).catch(() => null)
  const b = usage?.context.breakdown
  if (b) compactAt = b.isAutoCompactEnabled ? (b.autoCompactThreshold ?? null) : null
  return compactAt
}

// Reads one setting through the plugin API. The call normally returns the value itself; older hosts may hand back
// the settings object, so both shapes are accepted.
async function setting($, key) {
  const v = await $.settings.read({ key }).catch(() => undefined)
  if (typeof v === 'string' || typeof v === 'boolean') return v
  if (v && typeof v === 'object' && key in v) return v[key]
  return undefined
}

// The menu speaks aliases; settings may hold a resolved id such as `claude-opus-5-5`. Returns null when the advisor
// is off.
function advisorAlias(value) {
  if (typeof value !== 'string') return null
  const v = value.trim()
  if (!v || v.toLowerCase() === 'off') return null
  const lower = v.toLowerCase()
  if (lower === 'opus' || lower.includes('opus')) return 'opus'
  if (lower === 'sonnet' || lower.includes('sonnet')) return 'sonnet'
  return v
}

// Writes this session's usage and status report for Claudiu's sidebar. `fresh` re-reads the compaction threshold now.
// `override` carries a value Claudiu just asked for, so the sidebar updates before the host has refreshed its cache.
// Every read is best effort: a failed one is left out, and reporting never fails a hook.
async function report($, fresh = false, override = {}) {
  try {
    const dir = await $.env.get('CLAUDIU_USAGE_DIR')
    if (!dir) return
    const id = await $.session.id()
    if (!id) return

    const ctx = measured?.context
    const fraction = ctx?.tokens != null && ctx?.window > 0 ? Math.min(1, ctx.tokens / ctx.window) : null

    const rows = await $.config.list().catch(() => [])
    let autoCompact = rows.find(r => r.key === 'autoCompact')?.value
    if (typeof autoCompact !== 'boolean') {
      const fromSettings = await setting($, 'autoCompactEnabled')
      autoCompact = typeof fromSettings === 'boolean' ? fromSettings : null
    }
    const precompute = rows.find(r => r.key === PRECOMPUTE_ROW)?.value
    let precomputeCompact = typeof precompute === 'boolean' ? precompute : null
    let advisor = advisorAlias(await setting($, 'advisorModel'))
    if ('auto_compact' in override) autoCompact = override.auto_compact
    if ('precompute_compact' in override) precomputeCompact = override.precompute_compact
    if ('advisor' in override) advisor = override.advisor

    // A different model only reaches the running conversation on /clear or /compact. Off and on take effect at once,
    // so a null or newly named advisor replaces the active one; a model-to-model change keeps the old one.
    if (activeSession !== id || advisor == null || activeAdvisor == null) {
      activeSession = id
      activeAdvisor = advisor
    }

    const snap = {
      session_id: id,
      five_hour: measured?.limits.five_hour ?? null,
      seven_day: measured?.limits.seven_day ?? null,
      context_fraction: fraction,
      context_tokens: ctx?.tokens ?? null,
      context_window: ctx?.window ?? null,
      model: (await $.session.model().catch(() => null)) ?? null,
      account: (await $.env.get('CLAUDIU_ACCOUNT')) ?? null,
      auto_compact: autoCompact,
      precompute_compact: precomputeCompact,
      compact_at: await threshold($, fresh),
      advisor,
      advisor_active: activeAdvisor,
      effort: effortShown() ?? null,
      auto_effort: autoState,
      effort_reason: autoState === 'on' ? autoReason : null,
    }
    // `seen_at` (when a limit was read) is left out, so reading the same value again is not a change.
    const key = JSON.stringify(snap, (k, v) => (k === 'seen_at' ? undefined : v))
    if (key === written) return
    written = key

    const path = dir + '/' + stem(id) + '.json'
    await $.fs.write(path, JSON.stringify({ ...snap, captured_at: Math.floor(Date.now() / 1000) }))
  } catch {
    // Reporting must never disturb the session.
  }
}

// ---- Sidebar commands ---------------------------------------------------------------------------------------------

// Applies what Claudiu's sidebar asked for since the last look. The file holds the latest few `{ seq, key, value }`
// commands; only known keys and plainly shaped values are acted on.
async function applyCtl($) {
  try {
    const file = await $.env.get('CLAUDIU_CTL_FILE')
    if (!file) return
    const cmds = JSON.parse(await $.fs.read(file).catch(() => '{}')).cmds ?? []
    if (ctlSeen === null) {
      // First look: what is already in the file was sent before this session started, so it is not replayed.
      ctlSeen = cmds.reduce((top, c) => Math.max(top, c.seq), 0)
      return
    }
    const override = {}
    if (pendingAdvisor !== null) await setAdvisor($, pendingAdvisor, override)
    const pending = cmds.filter(c => c.seq > ctlSeen).sort((a, b) => a.seq - b.seq)
    for (const c of pending) {
      ctlSeen = c.seq
      await applyCtlCommand($, c, override)
    }
    if (Object.keys(override).length > 0) await report($, true, override)
  } catch {
    // A bad file must not disturb the session.
  }
}

async function applyCtlCommand($, c, override) {
  if (c.key === 'auto_compact' && typeof c.value === 'boolean') {
    await $.config.set({ key: 'autoCompact', value: c.value }).catch(() => {})
    override.auto_compact = c.value
  } else if (c.key === 'precompute_compact' && typeof c.value === 'boolean') {
    // Fails while Claude Code does not list the row; the report then keeps saying it is not offered.
    const set = await $.config.set({ key: PRECOMPUTE_ROW, value: c.value }).then(() => true, () => false)
    if (set) override.precompute_compact = c.value
  } else if (c.key === 'prompt' && typeof c.value === 'string' && c.value.trim() !== '' && c.value.length <= PROMPT_MAX) {
    // A turn of its own once the session is idle, which can be a whole turn away: not awaited, so the commands
    // behind it in the file are not held up.
    void $.prompt.submit({ text: c.value, asUser: true }).catch(() => {})
  } else if (c.key === 'advisor' && typeof c.value === 'string' && /^[A-Za-z0-9._-]{1,60}$/.test(c.value)) {
    await setAdvisor($, c.value, override)
  } else if (c.key === 'effort' && (c.value === 'auto' || EFFORTS.includes(c.value))) {
    if (c.value === 'auto') resumeAutoEffort()
    else pickEffortByHand(c.value)
    await report($)
  }
}

// Claude Code refuses a plugin's slash command after the person stopped a turn, until they type again
// ("refused: the session was stopped and nobody has sent input since"). That change is kept and tried again on the
// next look, and the new advisor is only reported once it went through. Any other failure drops the change.
async function setAdvisor($, value, override) {
  pendingAdvisor = null
  try {
    await $.command.run({ command: 'advisor', args: value })
  } catch (e) {
    if (String(e?.message ?? e).includes('refused')) pendingAdvisor = value
    return
  }
  override.advisor = advisorAlias(value)
}

// ---- Visual panel -------------------------------------------------------------------------------------------------

// The text of a string property in JSON that may still be being written. It can end anywhere, even inside an escape.
function partialString(json, key) {
  const m = new RegExp('"' + key + '"\\s*:\\s*"').exec(json)
  if (!m) return null
  const ESCAPES = { n: '\n', t: '\t', r: '\r', b: '\b', f: '\f' }
  let out = ''
  let i = m.index + m[0].length
  while (i < json.length) {
    const ch = json[i]
    if (ch === '"') break
    if (ch !== '\\') {
      out += ch
      i += 1
      continue
    }
    const n = json[i + 1]
    if (n === undefined) break
    if (n === 'u') {
      const hex = json.slice(i + 2, i + 6)
      if (!/^[0-9a-fA-F]{4}$/.test(hex)) break
      out += String.fromCharCode(parseInt(hex, 16))
      i += 6
    } else {
      out += ESCAPES[n] ?? n
      i += 2
    }
  }
  return out
}

// Queues a write of a card's file. Writes run one after another, and a failed one is dropped.
function writeVisual($, prefix, call, content) {
  const body = JSON.stringify({ call, content: String(content ?? '').slice(0, 200_000) })
  visualChain = visualChain.then(() => $.fs.write(`${prefix}${call}.json`, body)).catch(() => {})
}

// ---- One-shot completions -----------------------------------------------------------------------------------------

// The request file holds `{ id, model, system, prompt, deadline }`. The answer goes next to it as `<name>.res.json`:
// `working` as soon as the request is picked up, then `done` with the text, or `error`.
async function answerAsk($) {
  if (asking) return
  let reply
  let id
  try {
    const file = await $.env.get('CLAUDIU_ASK_FILE')
    if (!file) return
    const text = await $.fs.read(file).catch(() => '')
    if (!text) return
    const req = JSON.parse(text)
    if (typeof req.id !== 'string' || req.id === askSeen) return
    askSeen = req.id
    id = req.id
    if (!(req.deadline > Date.now() / 1000)) return // nobody is waiting any more

    asking = true
    reply = file.replace(/\.json$/, '.res.json')
    if (typeof req.prompt !== 'string' || !req.prompt.trim()) {
      await $.fs.write(reply, JSON.stringify({ id, state: 'error', error: 'The request had no prompt.' }))
      return
    }
    await $.fs.write(reply, JSON.stringify({ id, state: 'working' }))

    const r = await $.model.complete({
      model: ASK_MODELS.includes(req.model) ? req.model : 'haiku',
      system: typeof req.system === 'string' ? req.system.slice(0, 4000) : undefined,
      prompt: req.prompt.slice(0, 120_000),
      maxTokens: 1024,
      timeoutMs: 90_000,
    })
    const out = r.isAnswered
      ? { id, state: 'done', text: r.text }
      : { id, state: 'error', error: 'The model call failed (' + r.reason + (r.status ? ' ' + r.status : '') + ').' }
    await $.fs.write(reply, JSON.stringify(out))
  } catch (err) {
    if (reply) {
      const error = 'The model call failed: ' + String(err?.message ?? err).slice(0, 200)
      await $.fs.write(reply, JSON.stringify({ id, state: 'error', error })).catch(() => {})
    }
  } finally {
    asking = false
  }
}

// ---- Auto effort --------------------------------------------------------------------------------------------------

// What Haiku judges by: Anthropic's effort guidance, condensed from
// https://platform.claude.com/docs/en/build-with-claude/effort (copied 2026-11-12; refresh it when the docs change).
const JUDGE_SYSTEM = `You pick the reasoning effort for a coding agent's next turn. Anthropic's guidance for the effort levels:
- low: most efficient; simpler tasks that need the best speed and lowest cost: questions, lookups, small scoped edits.
- medium: agentic tasks that need a balance of speed, cost and performance; well-specified coding work.
- high: complex reasoning, difficult coding problems, harder or longer agentic tasks.
- xhigh: long-running agentic and coding tasks (over 30 minutes).
- max: the deepest possible reasoning, regardless of cost; rarely worth it.
Opus 5.5 and Haiku 5.5 default to medium; Sonnet 5.5 uses medium for well-specified agentic coding and high for harder or longer work.
Pick the lowest level that will do the task well, from the allowed levels only.
Answer with JSON alone: {"effort": "<level>", "reason": "<one short sentence for the user>"}`

const JUDGE_TIMEOUT_MS = 5000 // the prompt waits at most this long for Haiku, the git call included
const REPLY_TAIL = 2000 // characters of the previous answer Haiku reads
const PROMPT_HEAD = 8000 // characters of the prompt Haiku reads
const MAX_CHANGED = 50 // changed files listed on the first prompt
const MAX_MENTIONS = 20 // @-mentioned paths looked at on the first prompt
// Prompts that aren't the user's words keep the current effort. A plugin's prompt counts when sent as the user's
// (the editor's Ask Claude).
const UNJUDGED_ORIGINS = ['task-notification', 'scheduled-trigger', 'peer', 'peer-send-message', 'projects-relay', 'channel']

let autoRange = null // { lowest, highest } from CLAUDIU_AUTO_EFFORT; null when the session runs without auto effort
let autoState = 'off' // 'on', 'fallback' (Haiku failed last time), 'manual' (the user picked one), 'off'
let autoEffort // the level Haiku picked last, applied to main-thread steps while auto is on
let autoReason = null // Haiku's reason for it
let effortInUse = null // the effort the last main-thread request went out with
let lastReply = '' // the end of the last main-thread answer
let resumedRead = false // the resumed transcript's last reply was looked for
let heldPick = null // { pick } for a prompt waiting behind the running turn, applied when its own turn starts

// `low-high` as the range it names, or null.
function parseRange(text) {
  const m = /^([a-z]+)-([a-z]+)$/.exec(String(text ?? '').trim())
  if (!m) return null
  const [lo, hi] = [EFFORTS.indexOf(m[1]), EFFORTS.indexOf(m[2])]
  return lo >= 0 && hi >= lo ? { lowest: m[1], highest: m[2] } : null
}

// The level and reason in Haiku's answer, the level held inside the range; null when the answer names no level.
function parseJudgement(text, range) {
  try {
    const json = /\{[\s\S]*\}/.exec(String(text ?? ''))
    const v = json ? JSON.parse(json[0]) : null
    const at = EFFORTS.indexOf(String(v?.effort ?? '').trim().toLowerCase())
    if (at < 0) return null
    const level = EFFORTS[Math.min(Math.max(at, EFFORTS.indexOf(range.lowest)), EFFORTS.indexOf(range.highest))]
    const reason = typeof v.reason === 'string' && v.reason.trim() ? v.reason.trim().slice(0, 200) : null
    return { effort: level, reason }
  } catch {
    return null
  }
}

const wordCount = text => String(text ?? '').trim().split(/\s+/).filter(Boolean).length

// Whether a prompt after the first gets a fresh effort. A prompt that arrives while a turn runs joins that turn,
// unless the user asked it to wait for its own.
function isJudged(e) {
  if (e.isMeta) return false
  if (e.turnId && !e.wait) return false
  const origin = e.origin?.kind
  if (origin === 'plugin' ? !e.origin.asUser : UNJUDGED_ORIGINS.includes(origin)) return false
  return wordCount(e.text) > 3
}

// The @-mentioned paths (a `#L12-30` range dropped) with their line counts; one that can't be read as a file
// (a folder, a missing path) is listed by name.
async function mentionedFiles($, text) {
  const paths = [...String(text).matchAll(/(?:^|\s)@("[^"]+"|\S+)/g)]
    .map(m => m[1].replace(/^"|"$/g, '').replace(/#L[\d-]*.*$/, '').replace(/[:,;.)]+$/, ''))
    .filter(Boolean)
  const cwd = await $.session.cwd().catch(() => '')
  const lines = []
  for (const p of [...new Set(paths)].slice(0, MAX_MENTIONS)) {
    const full = /^([A-Za-z]:)?[\\/]/.test(p) || !cwd ? p : cwd + '/' + p
    const body = await $.fs.read(full).catch(() => null)
    lines.push(typeof body === 'string' ? `${p} (${body.split('\n').length} lines)` : p)
  }
  return lines
}

// The names in `git status --porcelain`; none outside a repository.
async function changedFiles($) {
  const r = await $.process.run(['git', 'status', '--porcelain'], { timeoutMs: JUDGE_TIMEOUT_MS }).catch(() => null)
  if (!r || r.exitCode !== 0) return []
  return r.stdout.split('\n').map(l => l.slice(3).trim()).filter(Boolean)
}

// The text of the last assistant message in the transcript a resumed session continues (Claudiu names it).
async function resumedReply($) {
  if (resumedRead) return ''
  resumedRead = true
  const file = await $.env.get('CLAUDIU_RESUMED_TRANSCRIPT')
  const text = file ? await $.fs.read(file).catch(() => '') : ''
  const lines = text.split('\n')
  for (let i = lines.length - 1; i >= 0; i--) {
    let entry
    try {
      entry = JSON.parse(lines[i])
    } catch {
      continue
    }
    const content = entry?.type === 'assistant' ? entry.message?.content : null
    const said = Array.isArray(content) ? content.filter(b => b?.type === 'text').map(b => b.text).join('\n').trim() : ''
    if (said) return said
  }
  return ''
}

async function judgeRequest($, text, first) {
  const parts = [`<prompt>\n${String(text).slice(0, PROMPT_HEAD)}\n</prompt>`]
  if (first) {
    const [mentioned, changed] = await Promise.all([mentionedFiles($, text), changedFiles($)])
    if (mentioned.length) parts.push(`<mentioned_files>\n${mentioned.join('\n')}\n</mentioned_files>`)
    const more = changed.length > MAX_CHANGED ? `\n(${changed.length - MAX_CHANGED} more)` : ''
    if (changed.length) parts.push(`<changed_files>\n${changed.slice(0, MAX_CHANGED).join('\n')}${more}\n</changed_files>`)
  } else {
    const reply = lastReply || (await resumedReply($))
    parts.push(`<current_effort>${effortInUse ?? 'unknown'}</current_effort>`)
    if (reply) parts.push(`<previous_reply_end>\n${reply.slice(-REPLY_TAIL)}\n</previous_reply_end>`)
  }
  const allowed = EFFORTS.slice(EFFORTS.indexOf(autoRange.lowest), EFFORTS.indexOf(autoRange.highest) + 1)
  parts.push(`Allowed levels: ${allowed.join(', ')}.`)
  return parts.join('\n\n')
}

// Asks Haiku for this prompt's effort: the pick, or null when Haiku failed or ran past the time limit, whose late
// answer is dropped.
async function judge($, text, first) {
  const stop = new AbortController()
  let timer
  const timedOut = new Promise(resolve => { timer = $.clock.after(JUDGE_TIMEOUT_MS, () => resolve(null)) })
  const answered = (async () => {
    const prompt = await judgeRequest($, text, first)
    const r = await $.model.complete(
      { model: 'haiku', effort: 'medium', system: JUDGE_SYSTEM, prompt, maxTokens: 300, timeoutMs: JUDGE_TIMEOUT_MS },
      { signal: stop.signal },
    )
    return r.isAnswered ? parseJudgement(r.text, autoRange) : null
  })().catch(() => null)
  const pick = await Promise.race([answered, timedOut])
  timer?.cancel?.()
  stop.abort()
  return pick
}

// Without a pick the prompt goes on at the effort it had. A hand pick made meanwhile wins.
function applyPick(pick) {
  if (autoState === 'manual') return
  if (pick) {
    autoEffort = pick.effort
    autoReason = pick.reason
    autoState = 'on'
  } else {
    autoState = 'fallback'
  }
}

// A level the user picked (the inspector, set_model): it holds for the session and ends auto effort until /clear or
// `auto`. Without auto effort it is a plain session-only override.
function pickEffortByHand(level) {
  want = { ...want, effort: level }
  if (autoRange) {
    autoState = 'manual'
    autoEffort = undefined
    autoReason = null
  }
}

// Auto effort on again: after /clear, or `auto` from the inspector or set_model.
function resumeAutoEffort() {
  if (!autoRange) return
  heldPick = null
  want = { ...want, effort: undefined }
  autoState = 'on'
  autoEffort = undefined
  autoReason = null
}

// The effort the session's next request goes out with, as far as the mod knows it.
const effortShown = () => want.effort ?? (autoState !== 'manual' ? autoEffort : undefined) ?? effortInUse

async function onPromptSubmit($, e, next) {
  if (!autoRange || autoState === 'manual' || e.command) return next(e)
  const first = (await $.session.turns().catch(() => 1)) === 0
  if (!first && !isJudged(e)) return next(e)
  const pick = await judge($, e.text, first)
  // A prompt typed during a turn waits for its own; the running turn keeps its effort until then.
  if (e.turnId) heldPick = { pick }
  else applyPick(pick)
  await report($)
  return next(e)
}

async function onTurnStart($, e, next) {
  const r = await next(e)
  if (!e.agentId && heldPick) {
    applyPick(heldPick.pick)
    heldPick = null
    await report($)
  }
  return r
}

async function onTurnComplete($, e, next) {
  const r = await next(e)
  if (!e.agentId && autoRange) lastReply = String(e.answer ?? '').slice(-REPLY_TAIL)
  return r
}

// ---- Session name -------------------------------------------------------------------------------------------------

// Applies the name Claudiu writes to CLAUDIU_NAME_FILE with /rename: once per name and Claude session.
async function watchSessionName($) {
  const file = await $.env.get('CLAUDIU_NAME_FILE')
  if (!file) return
  const read = async () => (await $.fs.read(file).catch(() => '')).trim()
  const key = async name => (await $.session.id()) + '\n' + name

  // What the file holds now is the name the session was launched with (`--name`): don't re-run it.
  const initial = await read()
  named = initial ? await key(initial) : null
  namePoll?.cancel()
  namePoll = $.clock.every(1000, async () => {
    const name = await read()
    if (!name) return
    const k = await key(name) // a new Claude session id (/clear) gets the name again
    if (k === named) return
    named = k
    await $.command.run({ command: 'rename', args: name }).catch(() => { named = null })
  })
}

// ---- Hook handlers ------------------------------------------------------------------------------------------------

async function onMeasure($, e, next) {
  const seen = Math.floor(Date.now() / 1000)
  const limit = kind => {
    const r = e.rateLimits.find(l => l.kind === kind)
    if (!r) return null
    return { utilization: Math.min(1, Math.max(0, r.percentUsed / 100)), resets_at: secs(r.resetsAt), seen_at: seen }
  }
  measured = { context: e.context, limits: { five_hour: limit('five_hour'), seven_day: limit('seven_day') } }
  const r = await next(e)
  await report($)
  return r
}

async function onCommandRun($, e, next) {
  const r = await next(e) // the command has run: a setting it changed is read after it
  // Only the user's own /effort is a hand pick; set_model's queued one comes from this plugin.
  if (autoRange && e.origin?.kind === 'composer' && e.command === 'effort') {
    want = { ...want, effort: undefined }
    autoState = 'manual'
    autoEffort = undefined
    autoReason = null
    if (EFFORTS.includes(e.args.trim())) effortInUse = e.args.trim()
  } else if (e.command === 'clear') {
    lastReply = ''
    resumedRead = true // the resumed conversation is gone
    resumeAutoEffort()
  }
  await report($, true)
  return r
}

async function onConfigSet($, e, next) {
  const r = await next(e)
  await report($, true)
  return r
}

// Compaction rebuilds the request, so the saved advisor becomes the one this conversation declares. A precompute
// installs nothing, so it is not a compaction here.
async function onSessionCompact($, e, next) {
  if (e.trigger === 'precompute') return next(e)
  const r = await next(e)
  await noteCompaction($)
  return r
}

async function noteCompaction($) {
  activeAdvisor = advisorAlias(await setting($, 'advisorModel'))
  activeSession = await $.session.id().catch(() => null)
  await report($, true)
}

async function onSetModel($, e) {
  const alias = String(e.model ?? '').trim().toLowerCase()
  const model = MODELS[alias] ?? (/^claude-[a-z0-9.-]+$/.test(alias) ? alias : undefined)
  const effort = String(e.effort ?? '').trim().toLowerCase()
  if (e.model != null && !model) {
    return { result: 'Unknown model: ' + e.model + '. Use haiku, sonnet, opus, fable or a full claude-… id.', isError: true }
  }
  if (e.effort != null && effort !== 'auto' && !EFFORTS.includes(effort)) {
    return { result: 'Unknown effort: ' + e.effort + '. Use ' + EFFORTS.join(', ') + ' or auto.', isError: true }
  }
  if (!model && e.effort == null) return { result: 'Give a model, an effort, or both.', isError: true }

  if (model) want = { ...want, model }
  // With auto effort, an effort from set_model holds for this session only and saves no default.
  const effortByHand = e.effort != null && autoRange != null
  if (effortByHand) {
    if (effort === 'auto') resumeAutoEffort()
    else pickEffortByHand(effort)
    await report($)
  } else if (e.effort != null) want = { ...want, effort: effort === 'auto' ? undefined : effort }

  // turn.step covers the rest of this turn. The session's own model and effort (what /model shows) follow once
  // Claude Code is idle. A command can't run from inside a hook the turn waits on, hence the timer.
  $.clock.after(0, () => {
    if (model) {
      $.command.run({ command: 'model', args: model }).then(
        () => { if (want.model === model) want = { ...want, model: undefined } },
        () => {},
      )
    }
    if (e.effort != null && !effortByHand) {
      $.command.run({ command: 'effort', args: effort }).then(
        () => { if (want.effort === (effort === 'auto' ? undefined : effort)) want = { ...want, effort: undefined } },
        () => {},
      )
    }
  })

  const what = [model, e.effort != null && 'effort ' + effort].filter(Boolean).join(', ')
  // Ending the turn lets the queued /model and /effort run, so the session (and /model) switch for good.
  return {
    result: 'Switched to ' + what + ' from your next request. Your system prompt still names the previous model; ignore that. ' +
      'Now stop: end your turn with one short line telling the user the switch is done and asking them to confirm ' +
      '(or say what to do next) so the session itself switches. Do not continue the task in this turn.',
  }
}

// The model's show_visual call streams to its card while it is written. The call's arguments arrive as pieces of JSON;
// the Markdown read out of them so far goes to the file a few times a second at most. The tool.call hook writes the
// finished card.
async function onShowVisual($, e) {
  const prefix = await $.env.get('CLAUDIU_VISUAL_PREFIX')
  if (!prefix) {
    return { result: "Visuals aren't available in this session (Claudiu's mod has no file for them).", isError: true }
  }
  if (typeof e.content !== 'string' || !e.content.trim()) return { result: 'content must be non-empty Markdown.', isError: true }
  writeVisual($, prefix, e.tool_use_id ?? lastVisual ?? 'v' + Date.now(), e.content)
  return { result: "Shown inline in the terminal. Say the key facts in a short reply; don't repeat the content." }
}

// Applies the saved model and effort (or auto effort's pick) to this turn's steps, and streams a show_visual call to its
// card while the model writes it. Sub-agent steps are left alone.
async function* onTurnStep($, e, next) {
  const effort = e.agentId ? undefined : want.effort ?? (autoState !== 'manual' ? autoEffort : undefined)
  const step = !e.agentId && (want.model || effort)
    ? { ...e, model: want.model ?? e.model, effort: effort ?? e.effort }
    : e
  if (!e.agentId && (step.effort ?? null) !== effortInUse) {
    effortInUse = step.effort ?? null
    void report($)
  }
  const prefix = e.agentId ? null : await $.env.get('CLAUDIU_VISUAL_PREFIX')
  if (!prefix) return yield* next(step)

  const open = new Map() // block index -> the show_visual call being written there
  for await (const c of next(step)) {
    if (c.kind === 'tool' && c.name === VISUAL_TOOL) {
      open.set(c.index, { id: c.id, json: '', at: 0 })
      lastVisual = c.id
    } else if (c.kind === 'input' && open.has(c.index)) {
      const v = open.get(c.index)
      v.json += c.json
      if (Date.now() - v.at >= 150) {
        v.at = Date.now()
        writeVisual($, prefix, v.id, partialString(v.json, 'content'))
      }
    }
    yield c
  }
}

// Draws a show_visual call as its title and the rows its card covers. Each row's link names the call and the row, which
// is how Claudiu finds them on screen, and the rows scroll and clip with the transcript. Drawing a call from an earlier
// process writes its card again from the transcript: that is what brings cards back after a restart.
async function onVisualRow($, e, next) {
  if (e.props.tool !== VISUAL_TOOL || e.props.isErrored) return next(e)
  const { Box, Link, Text } = $.ui.resolve(e)
  const id = e.props.tool_use_id
  if (!cardRows.has(id)) cardRows.set(id, CARD_ROWS)
  if (!e.props.isRunning && !published.has(id) && e.props.input?.content) {
    published.add(id)
    const prefix = await $.env.get('CLAUDIU_VISUAL_PREFIX')
    if (prefix) writeVisual($, prefix, id, e.props.input.content)
  }
  const links = Array.from({ length: cardRows.get(id) }, (_, i) =>
    h(Link, { key: String(i), href: `claudiu://visual/${id}/${i}` }, ' '.repeat(16)))
  return h(Box, { flexDirection: 'column' }, h(Text, { dimColor: true }, String(e.props.input?.title ?? 'Visual')), ...links)
}

// Claude Code folds MCP calls into one count line, which would hide a card's rows.
function onVisualGroup($, e, next) {
  if (!e.props.calls.some(c => c.tool === VISUAL_TOOL)) return next(e)
  return next({ ...e, props: { ...e.props, isExpanded: true } })
}

// Reserves the rows Claudiu measured for each card drawn in this process.
async function readCardRows($) {
  const prefix = await $.env.get('CLAUDIU_VISUAL_PREFIX')
  if (!prefix) return
  let changed = false
  for (const [id, rows] of [...cardRows]) {
    const measured = parseInt(await $.fs.read(`${prefix}${id}.rows`).catch(() => ''), 10)
    if (measured > 0 && measured !== rows) {
      cardRows.set(id, measured)
      changed = true
    }
  }
  if (changed) $.ui.invalidate('ui.render')
}

async function onSessionStart($, e, next) {
  const r = await next(e)
  autoRange = parseRange(await $.env.get('CLAUDIU_AUTO_EFFORT'))
  autoState = autoRange ? 'on' : 'off'
  // A level picked by hand before Claudiu restarted the session still holds.
  const hand = await $.env.get('CLAUDIU_HAND_EFFORT')
  if (autoRange && EFFORTS.includes(hand)) pickEffortByHand(hand)
  await report($)
  await applyCtl($)

  ctlPoll?.cancel()
  ctlPoll = $.clock.every(1000, () => applyCtl($))
  askPoll?.cancel()
  askPoll = $.clock.every(500, () => answerAsk($))
  rowsPoll?.cancel()
  rowsPoll = $.clock.every(200, () => readCardRows($))
  await watchSessionName($)
  return r
}

export function register(on) {
  on('session.measure', onMeasure)
  on('command.run', onCommandRun)
  on('config.set', onConfigSet)
  on('session.compact', onSessionCompact)
  on('tool.call', { tool: 'mcp__claudiu__set_model' }, onSetModel)
  on('tool.call', { tool: VISUAL_TOOL }, onShowVisual)
  on('ui.render', { component: 'ToolUse' }, onVisualRow)
  on('ui.render', { component: 'ToolGroup' }, onVisualGroup)
  on('turn.step', onTurnStep)
  on('turn.start', onTurnStart)
  on('turn.complete', onTurnComplete)
  on('prompt.submit', onPromptSubmit)
  on('session.start', onSessionStart)
}
