# Local repo index: build or adopt (research for #6)

Answers m4rocks/claudiu#6 (parent map #2): for the local repo index (symbols,
outline, references, relevant tests, repo map, recent diff), should m4code
build it or adopt something existing? Research date: 2026-10-21.

Method: every claim is read from a first-party source (docs, crate source,
README, API) or measured with a throwaway Rust spike that is not committed
(it lived in the agent scratchpad; description in section 3). Section 9 is a
ledger of what is confirmed and what is not. Location note: matches the
sibling research branches (`docs/research/<name>.md`).

## 1. The answer in brief

1. **Build the index and the tools; adopt only libraries.** Use `tree-sitter`
   + `tree-sitter-tags` + per-language grammar crates + `ignore` (walker) +
   `git2` (already a dependency) inside the app. Do not adopt ctags, the
   ast-grep CLI or its MCP server, Serena, or code-index-mcp as runtime
   components (section 5 gives the reason for each).
2. **The cost is low and measured.** On a 25 MB Rust tree (2,089 files) a
   full build with a warm OS file cache takes 1.9 s on 12 threads (7.5 s on
   one) with a 30 to 46 MB process working set; the first read of freshly
   copied files took 32.7 s on one thread, so a first-ever index on a fresh
   checkout can be several times slower. Compiling the tag queries for five
   languages (the startup cost) took 34 to 96 ms. A 1.8 MB repo takes 0.15 s on 8 threads. Re-tagging
   one changed file takes 0.5 to 2 ms (median file) up to about 80 ms (240 KB file).
   Section 3 has the table.
3. **References are name matches, not resolved references.** The tags
   mechanism records identifiers at call sites; it does no scope or type
   resolution. The only tree-sitter project built for precise cross-file
   resolution (stack-graphs) is archived. Tools must say so in their output,
   and precise references should come from the harness's own `LSP` tool when
   a code-intelligence plugin is installed (section 6).
4. **Language coverage is shallow but wide.** 13 of the 17 grammar crates I
   checked, including all the mainstream ones (Rust, TS/JS, Python, Go, C,
   C++, C#, Java), ship a `queries/tags.scm` and export it as `TAGS_QUERY`
   (section 4; Bash, Zig and the `kotlin-ng` Kotlin crate have none, Scala has
   the file but no constant). The files are small, 350 to
   2,200 bytes. Two traps found by measurement: the TypeScript query is an overlay that must be
   concatenated with the JavaScript one (4x more definitions on zod), and
   the Rust query ignores consts, statics, enum variants and fields.
5. **Minimum tool set: four tools, each with a budget parameter**
   (`repo_map`, `outline`, `find_symbol`, `related_tests`), plus `recent_diff`
   as an optional tool that is better injected into the volatile tail
   (section 7).
6. **Not shown by this research:** that the index saves tokens end to end.
   Outlines are 4 to 37 percent of the file's bytes, which bounds the saving
   when the model would otherwise read a whole file, but whether the model
   prefers these tools over its built-in `Grep`/`Read` is untested. That is
   spike material for #12.

## 2. What the building blocks are (sources)

**tree-sitter tags.** Tagging "identif[ies] named entities in a program"; a
tag has a role (definition or reference) and a kind, captured as
`@role.kind` plus a nested `@name`; optional `@doc`; queries conventionally
live in `queries/tags.scm`
([tree-sitter docs, Code Navigation](https://tree-sitter.github.io/tree-sitter/4-code-navigation.html)).
The page does not say references are resolved, and the query vocabulary
(`@reference.call`, `@reference.class`, `@reference.implementation`) is
syntactic.

**The `tree-sitter-tags` crate (0.27.1, published 2026-10-08).** Read from the
crate source (`src/tags.rs`), not the docs.rs example, which still shows
`tree-sitter-tags = "0.19"`:
- `TagsConfiguration::new(language, tags_query: &str, locals_query: &str) -> Result<Self, Error>`.
- `TagsContext::generate_tags(&mut self, config, source: &[u8], cancellation_flag: Option<&AtomicUsize>) -> Result<(impl Iterator<Item = Result<Tag, Error>>, bool), Error>`.
- `Tag { range, name_range, line_range, span, docs: Option<String>, is_definition: bool, syntax_type_id: u32 }`.
- `line_range` is capped by `MAX_LINE_LEN = 180`, so a "signature" is the first
  line of the definition cut at 180 bytes (multi-line signatures lose their
  parameters; seen in the spike: `pub fn new(`).
- It calls `parse_with_options(..., None, ...)`: always a full parse with no
  old tree, so there is no incremental reparse through this crate. The
  cancellation flag is checked from the parse progress callback.
- One `TagsContext` per thread (docs.rs page).

**Versions and freshness (crates.io API, 2026-10-21).** `tree-sitter` 0.27.1
(updated 2026-10-08), `tree-sitter-rust` 0.24.2, `ast-grep-core` /
`ast-grep-language` 0.51.0 (2026-10-15), `ignore` 0.4.33, `notify` 8.2.0.
GitHub API: tree-sitter/tree-sitter and ast-grep/ast-grep are MIT and pushed
2026-10-21.

**Aider's repo map** is the prior art for the "repo map" tool. From
`aider/repomap.py`: tags come from `*-tags.scm` queries, with Pygments used to
backfill references when a file has definitions but no references; tags are
cached in a disk cache keyed by filename with the file's mtime (re-extracted
when mtime differs); a `networkx` MultiDiGraph links referencer files to
definer files with weight `sqrt(num_refs)` times multipliers (identifier
mentioned in chat x10, long snake/camel names x10, `_`-prefixed x0.1,
defined in more than five files x0.1, referenced from chat files x50); PageRank
runs with personalization on chat files; the number of tags rendered is found
by binary search against a token budget (stops within 15 percent).
Its docs say the budget `--map-tokens` "defaults to 1k tokens" and that the map
lists files with their key symbol definitions
([repomap.py](https://raw.githubusercontent.com/Aider-AI/aider/main/aider/repomap.py),
[docs](https://aider.chat/docs/repomap.html)).

**Zed** is the closest architectural precedent (Rust + GPUI). It uses
tree-sitter with a separate `outline.scm` per language (16 found under
`crates/grammars/src/`, e.g. `rust/outline.scm`) and ships no `tags.scm`
(git tree API, 2026-10-21). Its Rust outline captures `@item`, `@name`,
`@context`, `@annotation` and covers structs, enums, enum variants, impls,
traits, functions, mods, type aliases, consts, statics and fields; richer
than the stock Rust `tags.scm`. Zed's repo carries `LICENSE-APACHE` and
`LICENSE-GPL` (GitHub reports the license as unresolved), so treat its
queries as inspiration and write our own.

## 3. Measurements (spike, release build, Windows 11, 12 logical cores)

Spike: `ignore::WalkBuilder` to find files, one `TagsContext` per thread over
chunked file lists, `TAGS_QUERY` from the grammar crates (Rust, Python, Go,
JavaScript, TypeScript), definitions and reference names stored in plain
`Vec<(String, u32)>` per file. Process working set read with
`GetProcessMemoryInfo` (includes about 10 MB of runtime baseline). Warm file
cache unless noted. Single measurements except where "x3" is given.

| Repo (source indexed) | Files | Walk | Tag, 1 thread | Tag, N threads | Defs / refs | Peak working set |
|---|---|---|---|---|---|---|
| This repo `src/` (0.8 MB Rust) | 35 | 9 ms | 0.34 s | n/a | 960 / 13,966 | 14 MB |
| flask (0.6 MB Python) | 83 | 54 ms | 1.35 s (cold) | 0.07 s (8) | 1,756 / 3,904 | 12 to 14 MB |
| gin (0.7 MB Go) | 99 | 27 ms | 1.91 s (cold) | 0.08 s (8) | 1,554 / 14,986 | 15 to 19 MB |
| ripgrep (1.8 MB Rust) | 110 | 29 to 41 ms | 0.69 to 0.80 s (x3 warm); 2.17 s cold | 0.15 to 0.18 s (8) | 3,584 / 15,140 | 18 MB (1), 28 MB (8) |
| zod (3.3 MB TS, stock TS query only) | 482 | 120 ms cold | 9.9 s cold, then 1.3 s | 0.41 s (8) | 1,011 / 3,000 | 15 MB |
| zod (TS + JS queries concatenated) | 482 | 43 to 47 ms | 1.33 s | 0.48 s (8) | 3,895 / 55,732 | 18 MB (1), 26 MB (8) |
| 6 popular crates copied from the cargo registry (25 MB Rust) | 1,985 | 85 ms | 7.5 s warm (32.7 s on first read after copy) | 1.9 s (12) | 44,911 / 181,359 | 30 MB (1), 46 MB (12) |
| whole cargo registry (1,015 MB Rust, 35,844 files) | 35,844 | 6.7 s | n/a | 218 s (12) | 2.0 M / 3.8 M | 539 MB |

Reading the table:
- Throughput is about 2.4 to 3.3 MB/s per thread warm. The "cold" rows (first
  read of files after download or copy) were 3 to 8x slower (zod 9.9 s versus 1.3 s; the 25 MB tree 32.7 s versus 7.5 s); the cause was not
  isolated (file cache, antivirus, or both), so a first-ever index of a fresh
  checkout on Windows can be several times slower than the warm numbers.
- Parallelism is imbalanced by large files (zod: one 173 KB file dominates), so
  a work-stealing split should do better than my static chunks.
- The 1 GB run is a stress anchor, not a typical repo; it includes generated
  files up to 4 MB (one took 1.36 s to re-tag).
- Memory is dominated by storing every reference occurrence as an owned
  `String`: 5.8 M tags cost about 500 MB. Storing references as
  interned name to (file, count) would cut that; not measured.
- Startup: compiling the tag queries for the five languages took 34 to 96 ms
  in every run (once per process).
- Walking for changes: a full `ignore` walk of 2,089 files took 84 ms. The
  walk did not stat or compare mtimes, so the cost of a change check (walk plus
  one `metadata` call per file) is extrapolated, not measured; it is probably
  of the same order, which would make a watcher optional.
- Re-tag of one file (average of 20): median file 0.5 to 1.9 ms (5.9 ms for this repo's 13 KB
  `statusline.rs`), largest file
  in each repo 9 to 81 ms (flask `app.py` 63 KB: 9 ms; ripgrep `defs.rs` 240 KB:
  72 to 81 ms; zod `schemas.ts` 173 KB: 50 to 57 ms).
- Outline size, definitions only, one line each, versus the file: this repo
  `app.rs` 5.1 percent of bytes; ripgrep `walk.rs` 8.8; flask `app.py` 3.9; gin
  `context.go` 17.8; zod `schemas.ts` 36.8 (TS arrow-function exports and type
  lines inflate it). Bytes, not tokens.
- Binary size (stripped, thin LTO): tree-sitter runtime + tags + `ignore`
  alone 2.1 MB; adding the Rust grammar 3.4 MB (+1.3 MB); all five grammars
  plus the spike code 6.0 MB (about +0.6 MB per additional grammar on
  average). The grammar `parser.c` sources are 1.5 MB (Go) to 8.4 MB
  (TypeScript, which includes TSX).
- The five grammar crates and their C code built with the stock Windows
  toolchain in about 42 s cold (the `cc` crate compiles the parsers). macOS
  was not tried.

## 4. Language coverage

Measured by downloading each published crate from static.crates.io and
checking for `queries/tags.scm` and a `TAGS_QUERY` export in the Rust bindings
(2026-10-21):

| Grammar crate (version) | `tags.scm` bytes | `TAGS_QUERY` exported |
|---|---|---|
| tree-sitter-rust 0.24.2 | 1,194 | yes |
| tree-sitter-python 0.25.0 | 350 | yes |
| tree-sitter-go 0.25.0 | 1,097 | yes |
| tree-sitter-javascript 0.25.0 | 2,218 | yes |
| tree-sitter-typescript 0.23.2 | 573 | yes (overlay, see below) |
| tree-sitter-c 0.24.2 | 375 | yes |
| tree-sitter-cpp 0.23.4 | 668 | yes |
| tree-sitter-c-sharp 0.23.5 | 848 | yes |
| tree-sitter-java 0.23.5 | 499 | yes |
| tree-sitter-ruby 0.23.1 | 1,087 | yes |
| tree-sitter-php 0.25.1 | 947 | yes |
| tree-sitter-swift 0.7.4 | 1,214 | yes |
| tree-sitter-lua 0.5.0 | 788 | yes |
| tree-sitter-scala 0.26.3 | 1,396 | no (file ships, no constant) |
| tree-sitter-bash 0.25.1, tree-sitter-zig 1.1.2, tree-sitter-kotlin-ng 1.1.0 | none | no |

For comparison, `ast-grep-language` 0.51.0 bundles 28 grammars by default
(bash, c, cpp, c-sharp, css, dart, elixir, go, haskell, hcl, html, java,
javascript, json, kotlin, lua, md, nix, php, python, ruby, rust, scala,
solidity, swift, typescript, yaml, zig; its `Cargo.toml`). (Only the `kotlin-ng` Kotlin crate was checked; ast-grep uses a different one,
`kotlin-sg`, whose tags support was not.) Grammar coverage is
not the constraint; having a good tags query per language is.

Quality findings from the spike (Rust, Python, Go, JS, TS only; C, C++, C#,
Java queries exist but were not run):
- **TypeScript:** the crate's `tags.scm` contains only function signatures,
  method signatures, abstract classes, modules, interfaces, type-annotation
  references and `new` expressions. The ordinary declarations come from the
  JavaScript query, so the two must be concatenated. On zod this moved
  definitions from 1,011 to 3,895 and references from 3,000 to 55,732. The
  concatenated query compiled against the TypeScript grammar. TSX needs the
  TSX grammar from the same crate (not tried).
- **Rust:** the stock query (`tree-sitter-rust` `queries/tags.scm`) defines
  structs, enums, unions, type aliases, methods, functions, traits, modules and
  macros, and references only calls, macro invocations and `impl` targets. It
  has no consts, statics, enum variants, struct fields, or type-use
  references. Zed's `outline.scm` covers those.
- **Definition lines are single lines** (180 byte cap), which loses multi-line
  Rust and TS signatures.
- Reference noise is large: 14,000 reference names for 960 definitions in this
  repo's `src/`. References must be filtered (by defined names, as Aider does
  with its multipliers) before they are worth showing.

Consequence: m4code should own and version its own `tags.scm`/outline queries
per supported language, starting from the upstream ones, and treat the
upstream files as a starting point.

## 5. Existing tools and servers considered

| Option | What it is (source) | Verdict |
|---|---|---|
| **ctags** (universal-ctags) | C, GPL-2.0 (GitHub API). One-shot command that writes a tag file; `--output-format=json` exists only if built with libjansson; `--append` adds to an existing file; no incremental mechanism seen in the man page ([ctags(1)](https://docs.ctags.io/en/latest/man/ctags.1.html), [JSON format](https://docs.ctags.io/en/latest/man/ctags-json-output.5.html)). Built for definitions; whether it emits references for the languages we care about was not checked. | Reject as a dependency: GPL, an external binary to ship on both OSes, no incremental mode seen. Could be a fallback for languages without a tags query; not worth it for v1. |
| **ast-grep** CLI | MIT Rust. Structural search/lint/rewrite over tree-sitter, multi-core; its intro page says nothing about an index ([intro](https://ast-grep.github.io/guide/introduction.html)). | It is a search tool, not an index. Reject as the index. `ast-grep-core`/`ast-grep-language` (MIT crates) are an option later for a structural-search tool, at the cost of its pattern language in the model's prompt. |
| **ast-grep-mcp** | Python MCP server, "experimental", needs `uv` and the `ast-grep` binary; four tools: `dump_syntax_tree`, `test_match_code_rule`, `find_code`, `find_code_by_rule` ([README](https://raw.githubusercontent.com/ast-grep/ast-grep-mcp/main/README.md)). | Reject: Python runtime, experimental, no symbol index. |
| **Serena** | Python MCP server giving symbol-level find/edit via language servers (40+ languages), requires `uv`; the app is GPL-3.0-or-later, its SolidLSP component MIT ([README](https://raw.githubusercontent.com/oraios/serena/main/README.md)). | Reject for bundling (GPL, Python, per-language LSP installs). It is the right shape for the precise-reference tier, which Claude Code's own `LSP` tool already covers. |
| **code-index-mcp** (johnhuang316) | Python 3.10+ and `uv`, MIT; tree-sitter parsing for 10 languages, basic file indexing otherwise; 13 tools, file watcher, a separate `build_deep_index` step for symbols ([README](https://raw.githubusercontent.com/johnhuang316/code-index-mcp/master/README.md)). | Reject: Python runtime, 13 tools is far more surface than the model needs. |
| **stack-graphs** | Rust, incremental name resolution on tree-sitter. README: "This repository is no longer supported or updated by GitHub"; GitHub API: archived, last push 2025-09-09. | Reject: unmaintained, so there is no maintained path to precise cross-file references on tree-sitter. |
| **SCIP / language-specific indexers** | `sourcegraph/scip` is active (Apache-2.0), but indexers are per language and build-system aware. | Not evaluated beyond the repo metadata; out of scope for the minimal local index. |
| **Claude Code's own tools** | `Grep` (ripgrep), `Glob`, `Read`, and an `LSP` tool for definitions, references, symbols and diagnostics that stays inactive until a code-intelligence plugin is installed and the user installs the language server binary ([tools reference](https://code.claude.com/docs/en/tools-reference)). | Baseline the index competes with. `Grep`/`Glob` are absent by default on macOS, Linux and WSL (same page), so there the baseline is Bash `rg`/`grep`; both OSes should be measured. The index must beat `Grep` + `Read` on tokens, and should defer to `LSP` for precise references. |

## 6. Incremental update and exposure to the model

**Incremental update.** The tags crate always does a full parse (section 2), but
a file re-tag costs 1 to 80 ms (section 3), so the unit of incrementality is
the file, as in Aider's mtime-keyed cache. Recommended: keep the index in memory
in the app process; at each tool call, run the `ignore` walk and re-tag files
whose mtime or size changed (the walk alone is 84 ms for 2,089 files; the
full check is extrapolated), and add a
`notify` watcher later if that walk becomes a cost. `notify` was not tried.
Persisting the index to disk is not needed for v1: a 25 MB tree rebuilds in
about 2 s warm.

**Where the code goes.** `src/mcp.rs` already implements a hand-written
newline-delimited JSON-RPC stdio MCP server (`claudiu --mcp-title`), a separate
process from the app. Its cross-session tools reach the app through
`crate::bridge::ask`, which files `bridge/req/<id>.json` and polls for
`bridge/res/<id>.json` every 200 ms (`src/bridge.rs`); the app answers every
such request through a user-confirmation modal (`ask_about_next_request` in
`src/app/cross.rs`; `bridge.rs` says "the model can never start, list or read
sessions on its own"). **So the existing bridge cannot carry the index tools:**
a confirmation dialog per `repo_map` or `outline` call would be unusable.
Index tools need either (a) a second, non-confirming request path to the app,
which keeps one shared index but adds the file-polling latency per call, or (b)
the index living in the MCP helper process itself (stdio server started per
session with the project folder), which needs no bridge but rebuilds the index
once per session and holds it in a second process. Either way the new tools
must be added to `allowed_tools` in `src/mcp.rs` so Claude Code does not show
its own permission prompt either. Choosing between (a) and (b), or an
in-process server once the PTY is gone, is a design decision this research did
not make; neither was prototyped. `git2` is already a dependency
(`Cargo.toml`). New dependencies needed: `tree-sitter`, `tree-sitter-tags`,
the grammar crates, `ignore`, and optionally `notify`.

**Claude Code constraints on the tools** ([MCP docs](https://code.claude.com/docs/en/mcp)):
- By default MCP tools are deferred behind `ToolSearch`; only names and server
  `instructions` load at start. Server instructions and the description of a
  tool that loads upfront are cut at 4,096 characters; a deferred tool's
  description at 16,384. A tool can set `_meta["anthropic/alwaysLoad"] = true`
  to load upfront (costing context every turn; not measured).
- Tool output warns above 10,000 tokens and is capped at 25,000 by default;
  a text result over 50,000 characters is saved to a file; a tool can raise its
  own limit with `_meta["anthropic/maxResultSizeChars"]`. So each tool needs a
  budget parameter and a default well under 10,000 tokens.
- The Claude Code adapter is the only v1 adapter (map notes), so these limits
  are the ones that matter now. Other harnesses were not checked.

## 7. Recommendation and minimum tool set

**Build-or-adopt: build, on adopted libraries.** The index is small (a walker,
a tags run per file, a map keyed by name, a PageRank pass), fits the existing
Rust app and, with the transport caveat in section 6, its MCP server, has no GPL or Python dependency, and none of the
existing servers provides the one thing m4code wants over them, a repo map
with a token budget.

Minimum tool set, each taking a budget (token or result-count) parameter and
returning plain text, not JSON:

1. `repo_map(budget_tokens, focus?)`: ranked files with their top definitions,
   Aider's algorithm (file graph from shared identifiers, PageRank
   personalised by `focus` paths or symbols, binary search on the budget).
   Heuristic (the ranking is not exact).
2. `outline(path, budget_tokens?)`: definitions with line numbers for one file,
   replacing a whole-file `Read` for orientation. Deterministic given the
   queries; 4 to 37 percent of file bytes in the spike.
3. `find_symbol(name, kind?, include_refs?, limit)`: definitions with their
   signature line and, if asked, name-matched reference sites grouped by file
   with counts. Output states `precision: name-match`. Heuristic for
   references; exact for definitions per file.
4. `related_tests(path_or_symbol)`: tests likely to cover a file. Heuristic:
   naming conventions (`foo_test.go`, `test_foo.py`, `foo.test.ts`, `tests/`,
   same-file `#[cfg(test)]` modules) plus tests whose reference names include
   the file's definitions. Not an index feature; accuracy untested.
5. Optional, `recent_diff(since?, budget_tokens)`: changed files and the
   definitions whose line ranges overlap changed hunks, via `git2`. The model
   can already run `git diff`, so the added value is the symbol mapping and the
   budget; it is better injected into the volatile tail of a turn than offered
   as a tool, to keep the cached prefix stable (map notes).

Explicitly not in the minimum: a standalone `references` tool (folded into
`find_symbol`), structural search, any persistent on-disk index, and precise
cross-file resolution (use the harness `LSP` tool when present).

## 8. Open questions this leaves for later tickets

- Does the model use these tools instead of `Grep`/`Read`, and how many tokens
  per accepted change do they save? Needs an end-to-end spike (#12).
- Quality of the C, C++, C#, Java and other upstream queries on real repos.
- Query ownership: how m4code stores and versions its own queries per
  language, and which languages ship in v1.
- Whether `alwaysLoad` for four tools costs more context than it saves.
- Reference-filtering heuristics (Aider-style multipliers) tuned on real tasks.

## 9. Ledger

**Confirmed by reading a primary source**
- tags vocabulary and `tags.scm` convention; tags crate API, `Tag` fields,
  180-byte line cap, full-parse-only, cancellation flag (crate source 0.27.1).
- Grammar crates' `tags.scm` presence and `TAGS_QUERY` export (downloaded
  tarballs), Rust and TypeScript query contents.
- Aider's repo-map mechanics (`repomap.py`); its default 1k token budget
  (docs).
- Zed's per-language `outline.scm` (16 files) and absence of `tags.scm` (git
  tree), Rust outline contents, `LICENSE-APACHE`/`LICENSE-GPL` present.
- ast-grep intro (no index mentioned), ast-grep-mcp tools and requirements,
  `ast-grep-language` grammar list, Serena and code-index-mcp READMEs,
  stack-graphs archived notice, ctags man page statements quoted above.
- Claude Code tool-search, instruction-length and output-limit rules; `LSP`
  tool inactive until a code-intelligence plugin is installed; `Grep` built on
  ripgrep.
- Licenses and activity dates from the GitHub API and crates.io API.

**Measured in the spike (one machine, Windows 11, 12 cores, one run unless
noted; spike code not committed)**
- Everything in section 3, including the cold-versus-warm difference.

**Not verified**
- macOS build and performance; Windows with antivirus excluded; the cause of
  the 3 to 8x cold-read slowdown.
- End-to-end token savings; whether the model prefers the tools; the cost of
  `alwaysLoad` tool definitions; behaviour of non-Claude harnesses.
- `notify` latency and reliability on Windows and macOS; PageRank cost on a
  large graph (not implemented in the spike); the memory of an interned index.
- Query quality for C, C++, C#, Java, Ruby, PHP, Swift, Lua; TSX handling.
- Whether ctags has a usable interactive mode and Windows builds (the man page
  fetched did not cover it; a navigation link to an "Interactive mode"
  chapter exists but was not read), ast-grep's own timing on these repos
  (not run), SCIP indexers beyond repo metadata.
- Whether shipping a GPL-2.0 ctags executable alongside the app is acceptable;
  this was not assessed and is not legal advice.
- The claim in the exported OpenCode session about these tools was not read in
  this ticket.
