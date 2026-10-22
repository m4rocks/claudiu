# Running the spike's bench

`bench` measures Claudiu against stock Claude Code through the same ACP adapter, and writes the report the GO or STOP decision is made from. It needs the adapter installed by the app (`<data>/acp`), `claude` on PATH, a logged-in account and `git`. It spends real money: 9 tasks x 3 runs x 4 arms, plus the Haiku arms.

## Tasks

`tasks.json` is a list of tasks. Write nine, with four marked `haiku`:

```json
[
  {
    "id": "fix-parser-bug",
    "repo": "C:/work/some-repo",
    "commit": "abc1234",
    "prompt": "The parser drops the last token of an empty file. Fix it and add a test.",
    "checks": ["cargo build", "cargo test"],
    "haiku": true,
    "timeout_secs": 1200
  }
]
```

Every run clones `repo` fresh. `checks` run in the clone after the agent is done; all must succeed. Pick tasks of different kinds (a bug, a feature, a refactor, a task that needs reading a lot of code) in repositories the tests of which are quick.

## Steps

```
cargo run --release -p claudiu-core --bin bench -- run --tasks tasks.json --out bench-out
cargo run --release -p claudiu-core --bin bench -- review --out bench-out
```

`run` can be interrupted and started again; finished runs are kept. `--arm stock` runs one arm. `--mode` sets the permission mode of every arm (default `auto`).

`review` writes `bench-out/review/<task>-rNNN.diff`: the diffs of runs that passed their checks, under random ids that do not say which arm wrote them. Read them without opening `key.json`. In `verdicts.json` set `accepted` to `true` or `false` for each id. `refusals.json` lists every command or write the guards refused; set `false` to `true` for a refusal that should not have happened.

```
cargo run --release -p claudiu-core --bin bench -- report --out bench-out
```

`report.md` states every criterion against the bar, the ablations and the Haiku arm, the pinned versions and each arm's configuration. It names which bar the numbers clear; the decision is yours.

## Arms

| arm | what runs |
|---|---|
| stock | Claude Code through the adapter, nothing added |
| claudiu | hooks, firewall, claudiu server, delegation |
| claudiu-no-firewall | as claudiu without the firewall |
| claudiu-no-delegation | as claudiu without `delegate` |
| stock-haiku, claudiu-haiku | the two above on Haiku, only for tasks marked `haiku` |

## What is measured

- **Cost** is Claude Code's own cost for the task and the subagents it started. **Cache-hit rate** is cache reads over everything read.
- **Accepted** means the checks passed and the diff was accepted in the blind review.
- **Hook p95** is how long the core takes to answer a hook call. The time Claude Code spends before and after the call is not visible over ACP, so the real figure is higher.
- **Subagent cold start** is the size of a subagent's first request.
- The STOP conditions: a saving under 15%, a firewall that never compacted, or guards that did not fire and did not fall back to ask mode. A saving of 15 to 30% with everything else met is the one-time week extension.
