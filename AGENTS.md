# AGENTS.md

m4code is a native desktop app, written in Rust with GPUI, that runs coding-agent sessions (Claude Code, Codex, OpenCode and Antigravity) around the user's own CLIs. Agents talk to m4code over ACP and show in its chat view. The repository is `m4rocks/m4code`.

## Layout

- `src/main.rs`: startup, menus and assets.
- `src/app/`: workspace state and behavior. `act.rs` dispatches actions, `keymap.rs` holds the key bindings, and the other files group methods by task. `headless.rs` and `headless/` drive the workspace on GPUI's test platform.
- `src/panels/`: workspace rendering. `workspace_view.rs` is the root view.
- `src/harness/`: harness-agnostic core (`mod.rs`, `acp.rs`, `compaction.rs`, `defaults.rs`, `limits.rs`, `permission.rs`, `runner.rs`, `subagents.rs`, `text_generation.rs`, `thread.rs`) plus isolated per-harness adapters (`claude/`, `codex/`, `opencode.rs`, `antigravity/`).
- `src/mcp/`: m4code's built-in MCP server (`mod.rs`) and IPC request bridge (`bridge.rs`), shared across harnesses.
- `src/thread/`: the conversation view and its composer.
- `src/store.rs` and `src/store/`: persisted app state.
- `src/git/` and `src/github/`: local Git and the GitHub pane.
- `src/terminal/`: the shell panel, the only PTY in the app. `src/lsp/`: language servers.
- `bridge/`: the Node bridge Claude Code sessions run through (Claude Agent SDK).
- `mod/`: the Claude Code plugin m4code installs, with tests in `mod/hooks/*.test.ts`.
- `web/`: the Astro site (Bun). `vendor/gpui-base`: vendored GPUI code. `scripts/`: packaging and icons.

## Commands

`just` lists every recipe. The main ones:

- `just build`, `just run`
- `just lint`: `cargo clippy --locked --all-targets -- -D warnings`. CI runs this.
- `just test-app`: Rust tests. The headless tests are `#[ignore]`d; run them with `cargo test -- --ignored --test-threads=1 app::headless`.
- `just test`: app, bridge and mod tests. The mod tests need the `claude` CLI on PATH.

## Things to know

- Debug builds use their own data folder, so they can run next to an installed release. Only one instance runs per data folder.
- Some tests bake `CARGO_MANIFEST_DIR` in at compile time. A test binary built from another worktree that shares this `target/` will look for files in that worktree.
- A NUL byte in any `.rs` file fails `source_tests::no_source_file_holds_a_nul_byte`, because git would treat the file as binary. Write `'\0'` instead.
- The README describes user-visible behavior; update it when that changes.
