# Performance and energy report

Measured 2026-11-03. Baseline source: `ffad8b1b4a77c72bb68afe581cfd1bb0a1895e62`.

## Results

| Workload | Before | After | Improvement |
| --- | ---: | ---: | ---: |
| Inactive terminal: 60 output events over about 1.04 seconds | 60 redraw notifications | 5 notifications | 91.7% fewer |
| Hidden terminal, same workload | 0 notifications | 0 notifications | Preserved |
| Foreground terminal, same workload | 60 notifications | 60 notifications | Normal responsiveness preserved |
| Busy sidebar, hidden or inactive, per 200 ms | 5 animation notifications | 0 notifications | 100% fewer |
| Busy sidebar, foreground, per 200 ms | 5 notifications | 5 notifications | Animation preserved |
| Queue and drain 1,000,000 pending terminal redraw requests, optimized build, median of 7 rounds | 68.237 ms | 1.893 ms | 97.2% less time; 36.0 times faster |
| Pending redraw events in that queue workload | 1,000,000 | 1 | 99.9999% fewer |

These are rendering-demand and event-queue measurements on Windows, not macOS battery-life or whole-app CPU measurements. Fewer redraw requests remove work that otherwise reaches layout, painting and GPU submission. The actual battery saving depends on the Mac, its display, window arrangement and the child processes. No wattage or battery-life percentage is claimed.

## Changes

- Coalesce terminal `Wakeup` events before they enter the channel. The grid already contains the latest output, so repeated redraw requests carry no additional content. All protocol replies, titles, clipboard events, bells and exits still enter the channel in order.
- Limit terminal output notifications in inactive windows to a 250 ms interval. A single deferred update flushes the last output even when the process stops emitting events. Foreground output keeps the existing approximately 60 Hz pump limit; activation requests a redraw immediately. Hidden terminals continue receiving output without requesting renders.
- Stop the sidebar's 40 ms animation timer when the window is inactive or the sidebar is absent. Rendering or activation restarts it when needed. Status polling and completion notifications continue.
- Skip the workspace notification when a transcript refresh finds no changed transcripts. This removes an unnecessary redraw every five seconds for unchanged live sessions.

The inactive terminal can display output up to 250 ms later. Typing and foreground output do not acquire that delay. This change does not reduce CPU use inside Claude Code, shells, language servers or builds they launch.

## Measurement method

Environment: Windows NT 10.0.26200.0, x86_64 MSVC; AMD64 Family 25 Model 80 Stepping 0; Rust 1.98.1. Dependencies are pinned by `Cargo.lock`. No dependencies were added.

The queue benchmark runs the previous `futures::mpsc::unbounded` send-and-drain implementation and the new channel in the same optimized executable. Each round enqueues a million `Wakeup` events without a consumer, then drains the queue. This deliberately stresses an output burst while the UI is occupied; it does not represent the time taken to parse or render a million terminal updates. Times include enqueueing and draining. Baseline runs first in each round; these are wall-clock microbenchmark results, not process CPU counters.

Raw release measurements, microseconds:

| Round | Previous queue | Coalescing queue |
| --- | ---: | ---: |
| 0 | 66139 | 1868 |
| 1 | 72601 | 1959 |
| 2 | 68237 | 2248 |
| 3 | 68891 | 1893 |
| 4 | 72488 | 1889 |
| 5 | 67536 | 1922 |
| 6 | 66314 | 1892 |

The terminal benchmark uses the real GPUI test executor, terminal channel, event pump and entity notification stream. It sends 60 events 17 ms apart, with real wall-clock waits and matching test-clock advances. Three repeated runs produced exactly 60 foreground, 5 inactive and 0 hidden notifications; each scenario took 1040–1044 ms. The baseline inactive count follows the previous unconditional `if visible { notify() }` behavior, also exercised by the foreground control. The first inactive update is immediate; steady streaming then uses four updates per second. The final pending update is checked separately by a regression test.

The sidebar measurement drives a real workspace and its session-list entity, advancing the test clock in five 40 ms steps. It measures foreground, hidden, foreground again, inactive through a second window, and foreground again: **5, 0, 5, 0, 5** notifications. Thus the test checks both stopping and resuming. Its baseline is the previous timer's unconditional notification every 40 ms while busy. It measures notifications, not physical display frames.

## Reproduce

Run each command separately; workspace tests use process-wide test data directories.

```sh
cargo test --locked --release benchmark_redraw_queue -- --ignored --nocapture
cargo test --locked benchmark_terminal_notifications -- --ignored --nocapture
cargo test --locked energy_sidebar_scheduling -- --ignored --nocapture
cargo test --locked -- --test-threads=1
cargo clippy --locked --all-targets -- -D warnings
```

The first three tests are ignored during the normal suite because they are manual measurements or require an isolated workspace. Queue tests also cover concurrent producers, rearming after consumption, receiver shutdown, and preservation of protocol-event order. Terminal tests cover the final deferred update and immediate foreground updates.

## Validation

- Full app suite: **303 passed, 0 failed, 22 ignored**, 73.53 seconds. The measurements above were run explicitly in addition to the normal suite.
- All three terminal redraw tests passed, including the deferred final update. Sidebar stopping and resuming passed for hidden and inactive windows.
- Optimized queue benchmark: all seven rounds passed their queue-length assertions.
- `git diff --check`: passed.
- Strict Clippy remains blocked by nine existing diagnostics in `attached_sync.rs`, `mcp.rs`, `terminal/view/mouse.rs`, older `headless.rs` tests, `files.rs`, and `github/mod.rs`. With those existing lint categories allowed, all targets pass with warnings denied. No unrelated lint cleanup was included. The verification command was:

```sh
cargo clippy --locked --all-targets -- -D warnings -A clippy::type_complexity -A clippy::useless_format -A clippy::collapsible_if -A clippy::useless_conversion -A clippy::cloned_ref_to_slice_refs -A clippy::single_element_loop -A clippy::redundant_closure
```

## Remaining hardware verification

A macOS run is needed to quantify battery savings. Compare release builds of the baseline and this commit on the same Mac with the same session output, display brightness and power settings. Measure idle, foreground streaming, inactive streaming and hidden-sidebar cases for equal intervals, recording app CPU, GPU activity and energy use separately from Claude Code's child processes. The Windows test platform does not exercise AppKit activation callbacks or Metal submission. Existing periodic status, Git and visual-file checks remain; this patch targets avoidable rendering and event traffic.
