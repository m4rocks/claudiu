# Hard sandbox options on Windows and macOS (research for #5)

Answers m4rocks/claudiu#5 (parent map #2): what can m4code use for a hard,
enforced-under-bypass filesystem and network jail on Windows and macOS, for
processes it starts. Research date: 2026-10-21. Nothing here was run on a Mac,
and nothing sandbox-related was executed on Windows; every claim is read from
a source, and the ledger in section 9 says which ones still need a spike.

Location note: `docs/` had no research-notes convention (only `TESTING.md`),
so this file lives in `docs/research/`.

## 1. The answer in brief

1. **The jail must wrap the harness process, not just its shell commands.**
   Claude Code's own sandbox covers shell commands only, does not run on
   native Windows, and under `bypassPermissions` its "unsandboxed retry" runs
   without a prompt (section 3). "Enforced under bypass" therefore means
   m4code launches the whole `claude` process tree inside an OS jail it
   controls from outside. Both candidate platforms support this.
2. **Windows: use Microsoft's new process-isolation primitive ("PSEC" /
   MXC BaseContainer), via the `mxc-sdk` crate, and refuse to run jailed
   tasks where it is unavailable.** It is the only Windows option found that
   gives read confinement, write confinement and per-container network
   filtering with no admin prompt per launch, no sandbox user and no host ACL
   edits. Cost: it is brand new (MXC GA 2026-10-07; `mxc-sdk` 1.0.x since
   2026-10-06), needs a servicing level (Win 11 24H2/25H2 with the Aug 2026
   update or later), and its low-level OS API is not documented on Microsoft
   Learn, only in MXC's source.
3. **macOS: Seatbelt (SBPL profile applied before exec).** It is
   kernel-enforced and good at filesystem confinement and on/off network, but
   it cannot express per-host or per-CIDR rules, so domain control needs a
   loopback proxy. `sandbox-exec` and `sandbox_init` are officially
   deprecated; every 2026 sandbox in this research still ships on them.
4. **Neither platform can widen a live jail.** Policy is fixed at launch.
   "Approved escapes" are either an operation m4code performs for the agent
   (the broker) or a relaunch under a wider policy.
5. **Weaker Windows designs exist (dedicated sandbox user + write-restricted
   token + ACLs + WFP, as in Codex "elevated" and `srt-win`) and are real,
   but they need admin setup, leave persistent ACL changes, cannot confine
   reads without deny ACEs, and have documented network-fence bypasses.**
   Treat them as a possible second tier, not the v1 base.

## 2. Method and source quality

Primary sources only: the source and docs of `microsoft/mxc`, `openai/codex`,
`anthropic-experimental/sandbox-runtime` ("srt"); Microsoft Learn; Microsoft
Windows Developer Blog; Apple man pages (via a mirror of Apple's man pages);
Claude Code and Codex first-party docs. News write-ups of MXC were seen in
search results and deliberately not used as evidence.

Pinned revisions read (clones were shallow, in a scratchpad, not committed):

| Repo | Commit | Notes |
|---|---|---|
| openai/codex | `d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4` (2026-10-21) | Apache-2.0 |
| microsoft/mxc | `2c1a874e7bac6faa32376a335a8db8dcba7d6079` (2026-10-20) | MIT. The checkout was partial on Windows (a generated file path is too long), docs and schema were intact |
| anthropic-experimental/sandbox-runtime | `c40cf14a65d025e3cd27f071f13a73c12b3b1f92` (2026-10-19) | Apache-2.0 |

Permalinks below use these SHAs. Labels used in this document:
**[confirmed]** = stated by the owning source; **[inferred]** = my reading of
source or docs, not stated outright; **[unverified]** = not checked, or the
source is silent.

## 3. What has to be jailed (the constraint that shapes everything)

- Claude Code docs: "The sandbox covers shell commands only. Claude's file
  tools, MCP servers, and hooks run outside it." Built-in Read/Edit/Write
  "follow permission rules instead"; hooks, local MCP servers, LSP servers and
  `apiKeyHelper` "run with your full access". [confirmed]
  ([sandboxing](https://code.claude.com/docs/en/sandboxing))
- "The sandbox runs on macOS, Linux, and WSL2. On native Windows, Claude Code
  runs commands unsandboxed." [confirmed] (same page)
- The unsandboxed retry (`dangerouslyDisableSandbox`) "runs without a prompt"
  in `bypassPermissions` mode; a matching allow rule also approves it.
  [confirmed] (same page). So the in-harness sandbox is not a boundary under
  bypass.
- Claude Code's own page tells you the fix: "run the Claude Code process
  itself in a container, virtual machine, or the sandbox runtime". [confirmed]
- GitHub Copilot CLI on MXC makes the same admission: built-in file tools run
  inside Copilot and their policy checks "aren't OS-enforced child-process
  isolation". [confirmed]
  ([post](https://commandline.microsoft.com/local-models-sandboxed-tools-github-windows/))
- Codex's own bypass flag (`--dangerously-bypass-approvals-and-sandbox`,
  alias `--yolo`) "has the same effect" as `danger-full-access`: no sandbox,
  no approvals. [confirmed] ([approvals and security](https://learn.chatgpt.com/docs/agent-approvals-security))
  Codex applies its sandbox from inside, so a harness flag can switch it off.

**Consequence for the design:** m4code starts `claude` as a child already
inside the jail (jail applied by m4code, before exec, outside the harness's
control). The harness's bypass setting then only affects prompts, not the OS
boundary. This is the premise of the rest of this document.

**Tension handed to #10 (policy), not resolved here:** the jailed `claude`
needs its own credentials (`~/.claude`), egress to the model API, and its
toolchains, while the jail must deny reads of `~/.ssh`, `.env` and cloud
credentials, and must not give a prompt-injected agent a free exfiltration
route. The model API itself is an allowed channel; no OS jail closes that.

## 4. Windows

### 4.1 Options at a glance

"Reads of secrets" = can `~/.ssh`, `.env`, cloud credentials be made
unreadable. "Toolchains" = what is known about cargo, npm, git.

| Option | Writes | Reads of secrets | Network egress | Admin / install | Maturity |
|---|---|---|---|---|---|
| Job object alone | No (limits, UI restrictions, kill-on-close) | No | No | None | Old, stable |
| Restricted token alone (`WRITE_RESTRICTED`) | Yes, with SID-scoped ACEs | **No** (restricting SIDs checked for writes only) | No | None | Old, stable |
| Dedicated sandbox user + write-restricted token + ACLs + firewall/WFP (Codex "elevated", `srt-win`) | Yes | Yes, via deny-read ACEs, snapshot at launch | Yes, SID-keyed block, with documented bypasses | One-time UAC: local user/group, firewall/WFP, logon rights | Codex: shipped; srt: alpha |
| Codex "unelevated" | Yes | **No** (refuses deny-read) | Weak ("environment-level offline controls") | None | Legacy fallback |
| Raw AppContainer (MXC tiers 2/3: AppContainer + BFS or DACL) | Yes | Default-deny except what is granted; explicit deny via host DENY ACEs | Capability-based (`internetClient`...), coarse | Tier 3 needs admin host prep and mutates host DACLs | MXC fallback |
| **PSEC / MXC BaseContainer (tier 1)** | Yes | Yes, grant-based, native deny when `PSE_SUPPORT_FS_DENY` | Yes, per-container WFP, CIDR/port/protocol + proxy mode | None per launch | New, GA 2026-10-07 |
| Windows Sandbox (VM) | Whole-VM | Host not visible except mapped folders | Block-only or on | Windows Pro/Enterprise/Edu, Hyper-V; not Home | Mature, wrong shape |
| WSLc / MicroVM / IsolationSession (MXC backends) | n/a for v1 | | | Experimental / Windows-version gated | Out of scope |

### 4.2 Job objects

Job objects group processes and enforce limits (working set, priority, time),
UI restrictions and kill-on-close; association cannot be undone and jobs nest
from Windows 8. They do not restrict filesystem or network access.
Children created through `Win32_Process.Create` (WMI) are not associated with
the job. [confirmed]
([Job Objects](https://learn.microsoft.com/en-us/windows/win32/procthread/job-objects)).
Use as a supporting primitive (kill-on-close, UI limits); both Codex and MXC
do. Not a jail.

### 4.3 Restricted tokens

`CreateRestrictedToken` can add deny-only SIDs, drop privileges and add
restricting SIDs; the system then does two access checks and grants access
only if both pass. With `WRITE_RESTRICTED`, "restricting SIDs ... are
considered only when evaluating write access." [confirmed]
([CreateRestrictedToken](https://learn.microsoft.com/en-us/windows/win32/api/securitybaseapi/nf-securitybaseapi-createrestrictedtoken)).
Microsoft also warns that restricted applications should run on a desktop
other than the default one, to stop `SendMessage`/`PostMessage` attacks on
unrestricted windows (same page); Codex creates a private desktop for this.
[confirmed]

Codex's own source states the read consequence: "its WRITE_RESTRICTED token
does not make capability SID deny-read ACEs participate in read access
checks. Read restrictions therefore require the elevated backend" and the
unelevated backend returns "cannot enforce deny-read restrictions directly;
refusing to run unsandboxed"
([windows.rs](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/sandboxing/src/windows.rs)).
[confirmed] A restricted token on the user's own account therefore cannot
hide `~/.ssh`.

### 4.4 Dedicated sandbox user + write-restricted token + ACLs + firewall (Codex elevated, srt-win)

How it works (Codex): an administrator-approved setup creates lower-privilege
sandbox users, filesystem permission boundaries, firewall rules "and local
policy changes"; commands run without administrator privileges
([Windows sandbox page](https://learn.chatgpt.com/docs/windows/windows-sandbox)).
The source shows: a command-runner launched as the sandbox user derives a
restricted token with `DISABLE_MAX_PRIVILEGE | LUA_TOKEN | WRITE_RESTRICTED`
([token.rs](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/windows-sandbox-rs/src/token.rs)),
per-workspace capability SIDs scope write grants
([cap.rs](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/windows-sandbox-rs/src/cap.rs)),
deny-read ACEs go on the sandbox principal
([deny_read_acl.rs](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/windows-sandbox-rs/src/deny_read_acl.rs)),
framed JSON IPC over named pipes with sandbox-user-scoped ACLs connects the
parent to the runner (`elevated/ipc_framed.rs`, `runner_pipe.rs`), and
a Windows Firewall rule (COM `INetFwPolicy2`) blocks all non-loopback IP
traffic, in and out, for the offline user, with loopback blocked except proxy
ports ([firewall.rs](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/windows-sandbox-rs/src/setup_provisioning/firewall.rs)).
Persistent WFP filters keyed on the account additionally block ICMP, DNS
(53, 853) and SMB (445, 139)
([wfp/filter_specs.rs](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/windows-sandbox-rs/src/wfp/filter_specs.rs)). [confirmed]

`srt-win` (Anthropic's alpha, Rust, same idea): runs commands as a dedicated
`srt-sandbox` user via `CreateProcessWithLogonW`, then a runner starts the
target "under a restricted token inside a job object"; network by WFP
`ALE_AUTH_CONNECT` filters that BLOCK the account except loopback to a proxy
port range; filesystem by additive inheriting NTFS ACEs; one-time elevated
`windows-install`
([README](https://github.com/anthropic-experimental/sandbox-runtime/blob/c40cf14a65d025e3cd27f071f13a73c12b3b1f92/README.md#windows-alpha)).
[confirmed]

Properties and costs (all from the srt README unless noted):

- **Read policy is inverted vs MXC.** srt's install grants the user's whole
  profile as readable "except for the credential stores" (opt-out with
  `--skip-profile-grant`); reads are open by default as on macOS/Linux.
  Credential protection is a deny-list. [confirmed]
- **Persistent host changes.** Write grants on project roots are kept between
  sessions; any sandboxed session on the machine can read a kept root; the
  sandbox account and group are one per machine. [confirmed]
- **Deny-read globs are snapshots.** Codex: matches "are evaluated when each
  sandboxed process starts. Files created later stay readable to that running
  process" (Windows page). srt: globs "expanded to concrete paths at
  `initialize()` time". [confirmed]
- **Network fence bypass class (documented by srt itself):** a thread that
  impersonates the anonymous token before creating its socket is not fenced;
  a token derived with `CreateRestrictedToken` (deny-only user SID, or
  restricting list without the account SID) is not fenced; `getaddrinfo` goes
  through the DNS Client service so name resolution succeeds; SMB is
  connected by the kernel redirector, and srt observed a sandboxed command
  reading and writing a share on the machine's own SMB server; `wsl.exe` also
  answers a sandboxed process. [confirmed by srt] Codex's elevated mode keys
  its firewall rule and WFP filters on the sandbox user's SID the same way,
  so the same class very likely applies. [inferred, untested]
- **Schannel breaks inside the write-restricted token:** `curl.exe`,
  PowerShell web cmdlets, .NET `HttpClient`/NuGet, `winget`, "and cargo,
  whose bundled libcurl uses Schannel on Windows" cannot complete TLS
  handshakes; Node, Python, Go and Git for Windows (steered to OpenSSL) work.
  [confirmed by srt, for its token design; not tested under MXC]
- **Per-user tool installs** (nvm, Scoop, `pip --user`, `%LOCALAPPDATA%\Programs`)
  are unreadable by the other account unless the profile grant exists. [confirmed]
- git refuses repos owned by a different user, so Codex injects
  `safe.directory` entries through `GIT_CONFIG_*` env vars
  ([sandbox_utils.rs](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/windows-sandbox-rs/src/sandbox_utils.rs)). [confirmed]
- Windows policy may block the setup (error 1385, logon rights); Codex warns
  about folders writable by `Everyone`. [confirmed]
- Upside: a distinct SID "structurally closes the surrogate-spawn class of
  escape" (Task Scheduler, BITS, out-of-process COM as interactive user),
  because anything spawned out-of-band still carries the sandbox SID. [confirmed
  by srt as a design claim; not independently tested]

Codex's legacy "unelevated" path (token derived from the current user, ACL
boundaries, "environment-level offline controls") "doesn't support denied
read paths" and has "weaker network isolation"; the page positions it as
a fallback "when administrator-approved setup is blocked". [confirmed]

### 4.5 AppContainer (raw)

Microsoft Learn describes AppContainer as isolating credentials, devices,
files/registry (read-write only where granted; "read-only access is less
restricted"), network (granular: Internet, Intranet, server), processes and
windows
([AppContainer isolation](https://learn.microsoft.com/en-us/windows/win32/secauthz/appcontainer-isolation)).
[confirmed] It is a conceptual page; it gives no policy mechanics for
launching arbitrary tools.

MXC's older tiers show the practical cost of using AppContainer directly:
- Tier 2 uses AppContainer + BFS (`bfscfg.exe`); Tier 3 uses AppContainer +
  host DACL edits: `deniedPaths` becomes host DENY ACEs "applied before the
  run and removed on exit", needing `WRITE_DAC` on each path, and two
  concurrent runs sharing a `containerId` "can revoke each other's ACEs"
  ([schema.md](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/docs/schema.md)). [confirmed]
- Tier 3 needs an admin tool (`wxc-host-prep`): ACEs on the system-drive root
  for the AppContainer package SIDs (because `cmd.exe`, `pwsh.exe`, `node.exe`
  call `GetFileAttributes("C:\")` at startup and fail otherwise) and a
  security-descriptor patch on `\Device\Null` that "resets ... at every boot"
  ([host-prep.md](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/docs/backends/process-container/host-prep.md)). [confirmed]
- Network via AppContainer capabilities is coarse: "The AppContainer fallback
  rejects" deny-egress-plus-private-network because the capability is
  bidirectional; Windows 11 per-container WFP is only on the PSEC path
  ([networking.md](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/docs/backends/process-container/networking.md)). [confirmed]

Codex disables the DACL path: its MXC adapter "never invokes MXC's
AppContainer dispatcher, edits host ACLs, creates sandbox users, runs setup,
or requests elevation"
([mxc-sandbox README](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/mxc-sandbox/README.md)). [confirmed]
m4code should do the same (`fallback.allowDaclMutation: false`).

### 4.6 PSEC / MXC BaseContainer (recommended)

What it is. MXC ("Microsoft eXecution Container") is Microsoft's policy-driven
sandbox SDK for agents; GA announced 2026-10-07. Windows backends include
`processcontainer` (default), `isolation_session`, `wslc`, and experimental
`windows_sandbox`, `microvm`, `hyperlight`
([README](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/README.md),
[GA blog](https://blogs.windows.com/windowsdeveloper/2026/10/07/microsoft-execution-containers-policy-driven-containment-for-ai-agents/)).
[confirmed] The ProcessContainer's top tier ("BaseContainer") uses a process
security environment: MXC translates the policy to a "PSEC" FlatBuffer, calls
`CreateProcessSecurityEnvironment` from `processmodel.dll`, and attaches the
handle to `CreateProcessW` through `PROC_THREAD_ATTRIBUTE_SECURITY_ENVIRONMENT`
([secenv.rs](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/src/mxc-sdk/src/backends/process_container/common/secenv.rs),
[versioning.md](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/docs/development/architecture/versioning.md)).
[confirmed] I found **no Microsoft Learn page for these APIs**; the contract is
documented only in MXC source. [confirmed absence in my searches; treat the ABI as
Microsoft-owned but not publicly specified]

Platform floor (Windows 11): process isolation 24H2 build 26100.9278, 25H2
26200.9278, 26H2 26300.9550, 26H1 28000.2804; session isolation needs
.9550 / 28000.3086
([os-version-support.md](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/docs/backends/process-container/os-version-support.md)).
[confirmed] MXC's own wording: availability is decided by runtime probing,
not by build number, and Codex warns "An installed Windows update alone is
not treated as evidence that every policy feature is enabled" and uses a
create/close probe because "symbol presence alone also succeeds on
transitional Windows builds where MXC is not enabled". [confirmed] The dev
machine this was written on reports build 26200.9445 (25H2): above the
process-isolation floor, below the session-isolation floor; not probed.
[confirmed build number only]

Enforcement and policy (`mxc-config` 1.0.0, closed JSON schema):

- **Writes and reads:** `readwritePaths`, `readonlyPaths`, `deniedPaths`; an
  ungranted path is unreachable. On BaseContainer, `deniedPaths` is "enforced
  natively by the OS when PSEC advertises `PSE_SUPPORT_FS_DENY`. No host
  filesystem changes are made." Grants apply to descendants, but volume-root
  grants do not cascade
  ([schema.md](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/docs/schema.md)).
  [confirmed] Tools that search upward for a marker (git, cargo, npm)
  "will" get `ACCESS_DENIED` at an un-granted parent. [confirmed]
- **Network:** per-container WFP "egress filters" scoped to the container
  SID: default deny, allow/deny by CIDR, protocol and port (IPv4 and IPv6),
  explicit deny wins; or "proxy-only egress (recommended)": the container may
  connect only to a configured loopback proxy; no hostname filtering in the
  policy ("Rules are direct network policy, not hostname filtering")
  ([networking.md](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/docs/backends/process-container/networking.md),
  [policy 1.0.0](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/docs/containment-policy/1.0.0/policy.md)). [confirmed]
  Proxy mode requires `ingress.default: "allow"` because
  `privateNetworkClientServer` is one bidirectional capability, and a
  documented limitation says that on build 26691.1002 host-to-container
  listener connections still time out under the proxy mapping. [confirmed]
- **UI:** `ui.disable` maps to the Win32k system-call disable mitigation;
  clipboard, input injection, desktop handles are policy. Codex's adapter
  leaves Win32k enabled because "PowerShell startup" needs it. [confirmed]
- **Process:** the child is created suspended, put into a kill-on-close job
  and the policy is "retained through workload completion"; "The upstream
  runner terminates remaining descendants when the foreground process exits",
  so detached servers die (Codex README). [confirmed]
- **Environment:** a `process.env` can be supplied, and Windows
  ProcessContainer refuses a supplied environment missing `SYSTEMROOT` or
  `LOCALAPPDATA` (policy 1.0.0). [confirmed]
- **Modes:** enforcement, learning (block and record to JSON) and permissive
  (allow and record); activity reports "are available only for Windows
  process containers" (GA blog). `--audit` "turns off all sandbox security".
  [confirmed] Useful for tuning toolchain grants.
- **Hosting:** `wxc-exec.exe` or the SDK embedded in-process; "The sandbox
  launcher `wxc-exec.exe` never elevates itself" and the optional
  `wxc-host-prep.exe` is only needed for the AppContainer tiers. [confirmed]

What Microsoft says about stability: GA for MXC; MicroVM experimental;
"Not stated: the session container's status" in the GA blog; Intune
management "soon". [confirmed]

Caveats specific to using it from Codex, which m4code would inherit unless it
chooses differently (all from the Codex adapter README):
- Codex requires `allow_local_binding = true` for MXC managed networking:
  "native host-loopback access is bidirectional, and MXC's proxy-peer
  identity mode is not integrated here". With it true the jail can reach any
  service on the host's loopback. m4code's broker proxy design should use
  `allowedProxyPeer` instead (documented in networking.md, but I did not test
  it). [confirmed for Codex; unverified that `allowedProxyPeer` works end to end]
- Codex's policy is "readable roots: the whole volume, minus deny paths", i.e.
  a deny-list for reads. That is Codex's policy choice, not a platform limit:
  the platform is grant-based. An allowlist for reads would be stricter but
  is unproven against cargo/npm/git (see 6.3). [inferred]
- Deny globs are expanded to concrete paths once before launch (snapshot).
  [confirmed]
- Non-Unicode paths and environment values are rejected rather than converted.
  [confirmed]

### 4.7 Windows Sandbox (VM) and the other MXC backends

Windows Sandbox is a disposable Hyper-V VM; "Host-installed software isn't
available in the sandbox", networking is on by default, "currently doesn't
allow multiple instances to run simultaneously", and it is "currently not
supported on Windows Home edition"
([Learn](https://learn.microsoft.com/en-us/windows/security/application-security/application-isolation/windows-sandbox/)).
[confirmed] MXC's own backend adds: full VM boot on every create-and-run,
one VM per logon session, directory-only sharing, network block-only
([windows-sandbox.md](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/docs/backends/windows-sandbox/windows-sandbox.md)),
and MXC marks it experimental. [confirmed] Wrong shape for per-task agent
jails over a host repo and host toolchains; revisit only for a future
"untrusted code" mode.

## 5. macOS

### 5.1 Seatbelt

- **Mechanism.** A kernel-enforced profile (SBPL) applied to a process and
  inherited by children. MXC's backend: "translates your JSON policy into a
  Seatbelt profile and applies it with `sandbox_init()` between `fork()` and
  `exec()`"; requires macOS 15+, no root, no daemon
  ([seatbelt-backend.md](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/docs/backends/seatbelt/seatbelt-backend.md)).
  Codex wraps the command in `/usr/bin/sandbox-exec` with a generated profile
  and base policy that starts `(deny default)` "inspired by Chrome's sandbox
  policy"
  ([seatbelt.rs](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/sandboxing/src/seatbelt.rs),
  [base policy](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/sandboxing/src/seatbelt_base_policy.sbpl)).
  srt also uses `sandbox-exec`. [confirmed]
- **`sandbox-exec` status.** Apple's man page: "The sandbox-exec command is
  DEPRECATED", recommending App Sandbox
  ([man page mirror](https://keith.github.io/xcode-man-pages/sandbox-exec.1.html),
  dated 2017); `sandbox_init(3)`: "The sandbox_init() and sandbox_free_error()
  functions are DEPRECATED"
  ([man page mirror](https://keith.github.io/xcode-man-pages/sandbox_init.3.html)).
  The same page marks the five named `kSBXProfile*` profiles deprecated and
  says "These profiles must not be used when building against macOS SDK >=
  27.0 ... Processes opting into them will be killed on attempt to do so";
  the page does not mention custom profile strings (`flags` = 0), so whether
  hand-written SBPL (what Codex, MXC and srt use) is affected is **not
  stated**. [confirmed deprecation; unverified for custom SBPL beyond 26/27]
  Mirror of Apple's pages, not apple.com, so treat the exact wording as
  second-hand until read on a Mac.
- **Filesystem.** Last-match-wins rules; MXC emits `deniedPaths` last so they
  outrank every allow, and names `file-read-metadata` so a denied path does
  not answer `stat()`. `readonlyPaths` also denies write and sockets in that
  subtree; baseline read-only grants cover system dirs (`/bin`, `/usr/lib`,
  `/System`, `/Library`, `/private/etc`, developer dir) and read-data on `/`;
  SIP paths stay unwritable regardless; paths with `..` are rejected;
  symlinked roots (`/tmp`, `/etc`, `/var`) are resolved first. [confirmed]
  Reads are therefore default-deny with MXC's generated profile; MXC's
  `32_mac_agent_workspace.json` example grants `~` read-only and then denies
  `~/.ssh`, `~/.aws`, `~/.gnupg`, `~/.config/gh`, `~/Library/Keychains`
  ([example](https://github.com/microsoft/mxc/blob/2c1a874e7bac6faa32376a335a8db8dcba7d6079/tests/examples/32_mac_agent_workspace.json)).
  [confirmed]
- **Network.** Only an on/off switch plus loopback: "no concept of 'this host
  but not that one.'" `(remote ...)` accepts only `*` and `localhost`;
  per-IP/CIDR/port/hostname rules, inbound-to-loopback-only, and proxy peer
  identity are rejected. `localhost` means "this machine at any address", so a
  loopback allowance covers the host's LAN addresses on that port. Inbound is
  all-or-nothing and a workload binding `0.0.0.0` is LAN-reachable. [confirmed]
  **Proxy-only egress is kernel-enforced**: with `egress: deny` the profile
  allows only `network-outbound` to `localhost:<proxy-port>`; a client that
  ignores `HTTP_PROXY` "just doesn't get anywhere". Using the proxy is
  cooperative. [confirmed] This is how a domain allowlist must be built on
  macOS (srt does the same).
- **AF_UNIX sockets are governed by the filesystem policy** (path match), so a
  read-write root lets the jail `connect()` to any pre-existing listener below
  it; the docs call out Docker, `ssh-agent`, `gpg-agent` as control planes and
  say to deny them. MXC clears the host environment unconditionally, so
  `SSH_AUTH_SOCK` and cloud credentials in the environment do not leak. [confirmed]
- **Opt-ins that widen the jail** (keep off): `guiAccess` (all `mach-lookup`,
  read-write on `/private/tmp` and `/private/var/folders`), `keychainAccess`,
  `profileOverride` (raw profile; all other policy ignored), and, in srt,
  `allowAppleEvents` ("launched applications run outside the sandbox
  entirely"). [confirmed]
- **DNS.** srt states resolution via the system resolver "mirrors the macOS
  behaviour" (the connect is blocked, the lookup is serviced outside).
  [confirmed by srt as a statement; not tested here] A name-based exfil
  channel is plausible; inferred.
- **`HOME`.** MXC sets `HOME` to the working directory, so `.gitconfig`,
  `.npmrc`, `.config/*` there are read as user config; set `HOME` explicitly
  for untrusted workspaces. [confirmed]
- **Deny-glob timing.** Not evaluated here. Seatbelt rules can use `(regex ...)`
  matched by the kernel at access time, which would catch later-created files
  (unlike Windows snapshot expansion), but that is my reading of SBPL, not a
  tested or documented guarantee. [unverified]
- **Alternatives considered:** App Sandbox is for signed apps with entitlements
  (Apple's recommended path) and cannot wrap arbitrary child toolchains
  [inferred]; a VM (Virtualization.framework) was not researched.

## 6. Cross-cutting questions

### 6.1 Write confinement

Windows PSEC and macOS Seatbelt both default-deny writes and grant explicit
roots. For m4code that means a policy with exactly: the task workspace
(worktree), a per-task scratch/temp dir, and the harness's own state dir if
the harness is jailed. Protected sub-paths inside writable roots matter:
Codex keeps `.git`, `.agents`, `.codex` and `.aws` read-only even inside
workspace-write; srt auto-denies shell startup files and tool config under
writable roots; Claude Code's page lists the same class (a command that
could edit these "could grant itself permissions, or add a hook or MCP
server"). [confirmed] m4code's policy needs the same carve-outs
(its own hooks, `.claude/settings*`, `.mcp.json`, git hooks).

### 6.2 Read confinement of secrets

| Platform / design | Default | Secrets story | Caveat |
|---|---|---|---|
| Windows PSEC | grant-based | Do not grant `~/.ssh`, cloud dirs; add `deniedPaths` as belt-and-braces | `deniedPaths` native only if `PSE_SUPPORT_FS_DENY`; Codex fails launch if a requested deny is unsupported. Glob deny is a launch-time snapshot |
| Windows dedicated user | open or granted (srt grants the profile minus a credential-store deny list) | deny ACEs | Snapshot; needs persistent host ACLs; user-profile grants can be stale until refreshed |
| Windows restricted token alone | open | none | `WRITE_RESTRICTED` ignores reads |
| macOS Seatbelt | default-deny with baseline | do not grant, plus `deniedPaths` | AF_UNIX sockets, loopback, environment variables |

Files created during a run that match a deny glob (for example a new `.env`)
stay readable on Windows (documented by Codex and srt). Mitigation is policy,
not mechanism: do not grant parents that contain secrets; keep the workspace
grant narrow; place secret-like files outside the workspace. [confirmed
limitation]

### 6.3 How cargo, npm and git behave

Honest summary: **there is no first-party evidence in these sources of
cargo, npm or git running correctly inside PSEC.** What is known:

- Windows, dedicated-user token: Schannel clients fail (cargo's libcurl,
  winget, .NET, git with schannel backend, `curl.exe`); Node/Python/Go and
  Git for Windows with OpenSSL work; git needs `safe.directory`; per-user
  tool installs need an explicit read grant. [confirmed by srt/Codex]
- Windows PSEC: upward marker searches hit `ACCESS_DENIED` at ungranted
  parents (git root discovery, `Cargo.toml` workspace search, `package.json`
  lookup); grant only the parents that must be reached. Detached children
  die when the foreground process exits. `cmd.exe`/PowerShell start up
  under it (Codex tests "the normal `powershell.exe` and `pwsh.exe` command
  paths, not only `cmd.exe`"). MXC offers `available_tools_policy()` which
  "discover[s] tool and SDK directories from environment ... as read-only
  policy paths". [confirmed]; whether Schannel works under PSEC: [unverified]
- macOS: the environment is cleared (set `PATH`), tools outside the system
  dirs need an env entry and a read grant (Homebrew needs `/opt/homebrew`);
  `/usr/bin/git` and `python3` are `xcrun` shims that need the developer-dir
  grant, which MXC adds automatically; test runners/bundlers need `nestedPty`
  and AF_UNIX sockets inside a read-write root; `getcwd()` fails unless the
  working directory itself is granted. MXC's `34_mac_offline_build.json`
  example exercises JS/Python workloads offline. [confirmed]
- Network-dependent steps (`cargo fetch`, `npm install`, `git fetch`) need
  either an allowlisting proxy or to run in a separate network-enabled phase;
  Seatbelt cannot allow "crates.io only" without the proxy. Proxy env vars
  (`HTTP_PROXY`, `CARGO_HTTP_PROXY`...) are cooperative on both platforms; the
  kernel/WFP rule is what makes a non-cooperative client fail rather than
  escape. [confirmed]

This section is the main reason a spike is required before #10 can promise
"builds work inside the jail".

### 6.4 Privileged broker and approved escapes

Findings:

- **Policy is fixed at launch** on both platforms: PSEC retains the policy
  "through workload completion", Seatbelt applies at exec for the life of the
  process tree. [confirmed] A live jailed process cannot be widened in place
  by either (ACL-based designs can add an ACE, but srt shows the cost:
  time proportional to the tree, grants kept, no per-exec overrides). [confirmed]
- So an approved escape is: (a) **broker-performed operation** (m4code
  performs the write/fetch/command outside the jail and returns the result),
  or (b) **relaunch** under a wider policy (new process or resumed harness
  session). The refuse-then-`request_access` flow in the map's sandbox stance
  maps onto (a) for small operations and (b) for tool-wide grants. [inferred]
- **Harness-level escape hatches are not a broker.** Claude Code's
  `dangerouslyDisableSandbox`, `excludedCommands` and `allowUnsandboxedCommands`
  decisions are made inside the harness and auto-approve under bypass
  (section 3). Codex separates "sandbox sets what Codex can technically do"
  from "approval policy sets when Codex must stop and ask", and `never` still
  applies the sandbox. [confirmed] m4code's jail must sit outside both, with
  the broker being m4code itself.
- **Channels from jail to broker:**
  - Windows PSEC: loopback proxy with `allowedProxyPeer` identity pinning is
    the documented MXC route (networking.md); Codex instead uses framed JSON
    over named pipes whose ACL names the sandbox principal
    (`elevated/ipc_framed.rs`, `runner_pipe.rs`). Whether a PSEC container can
    open an m4code named pipe, and what its DACL must grant, is [unverified].
  - macOS: a Unix socket in a directory granted read-write (AF_UNIX is gated
    by the filesystem policy); the same grant lets the jail connect to any
    other listener under that directory, so use a dedicated directory.
    [confirmed mechanism]
  - Either: MCP over loopback to the proxy port already allowed.
- **Prior art for "refuse with a legal alternative":** Codex `execpolicy`
  rules have `decision = allow | prompt | forbidden` and a `justification`
  that "should include a recommended alternative" for `forbidden`
  ([README](https://github.com/openai/codex/blob/d138398cef7d49e46fe33f1d0ddbd2ca0ed16aa4/codex-rs/execpolicy/README.md)). [confirmed]

### 6.5 Rust crates and existing code to reuse

| Asset | What it gives | License / state | Fit |
|---|---|---|---|
| [`mxc-sdk`](https://crates.io/crates/mxc-sdk) 1.0.1 | One JSON-policy API over Windows ProcessContainer (PSEC), macOS Seatbelt, Linux Bubblewrap; `run`, `spawn`, `spawn_with_pty`, `probe`, `platform_support`, `available_tools_policy`; Node and .NET SDKs | MIT; crate created 2026-10-06, two versions; 1,671 downloads when queried | Best single dependency for both platforms. Young; API "stable" at 1.0.0. Telemetry is off unless run, user and admin all opt in |
| `codex-mxc-sandbox` (in openai/codex) | A worked adapter from a permission profile to an MXC request: volume roots, deny expansion, loopback handling, launcher-env transport | Apache-2.0, workspace crate; pinned `mxc-sdk` by git rev | Reference to copy patterns from, not a dependency |
| `codex-windows-sandbox` (in openai/codex) | The dedicated-user + restricted-token + ACL + firewall/WFP implementation | Apache-2.0, ~21k lines | Reference for a possible second tier |
| `srt-win` (in sandbox-runtime, `vendor/srt-win-src`) | Rust crate: sandbox account, token, WFP, ACL grants, job, runner | Apache-2.0, `publish = false`, version 0.0.1, alpha | Smaller reference for the same design |
| `@anthropic-ai/sandbox-runtime` | Seatbelt profile generation, bubblewrap, domain-allowlist HTTP/SOCKS proxies | Apache-2.0, TypeScript | Reference for the proxy and Seatbelt profile; not embeddable in a Rust app without Node |
| Codex `sandboxing` Seatbelt `.sbpl` files | A tested base SBPL policy | Apache-2.0 | Alternative to MXC's generator if m4code wants to own the profile |
| Codex `execpolicy`, `network-proxy` | Prefix-rule policy engine; managed proxy | Apache-2.0 | Candidates for #10 and for the broker proxy |

## 7. Recommendation per platform

### Windows

1. **Use PSEC through `mxc-sdk` (BaseContainer tier only).** Pass
   `fallback.allowDaclMutation = false` (as Codex does), treat any
   downgrade to AppContainer tiers 2/3 as "jail unavailable", and gate on a
   runtime probe (`mxc_sdk::v1::probe` or equivalent create/close probe),
   not on a build number.
2. **Wrap the whole harness process tree** (section 3). Policy sketch for
   #10: read/write = task worktree + scratch; read-only = system dirs,
   toolchain dirs (via `available_tools_policy` plus explicit entries),
   the minimum harness config; deny = `~/.ssh`, cloud and token dirs, browser
   profiles, `.env` outside the worktree; network = egress deny with a
   m4code-owned loopback proxy (`allowedProxyPeer` pinned, not
   `allow_local_binding`) that carries the model API and an allowlist.
3. **If PSEC is unavailable:** default to refusing "jailed" tasks and say so
   in the UI. The dedicated-user design (Codex elevated, `srt-win`) is the
   only credible fallback; it needs a UAC-approved install, persistent ACLs,
   has documented network-fence bypasses, and breaks Schannel clients, so it
   should be an explicit product decision in #10 rather than a silent
   fallback. Do not ship the "restricted token alone" path: it cannot protect
   reads.
4. **Do not use** job objects alone, Windows Sandbox, or raw AppContainer
   with DACL edits as the primary jail.

### macOS

1. **Use Seatbelt**, either via `mxc-sdk`'s Seatbelt backend or m4code's own
   generated SBPL starting from Codex's base policy. Launch the harness
   inside it (section 3).
2. **Network:** `egress: deny` plus a loopback proxy port, never `egress:
   allow` for jailed tasks; keep `hostLoopback: deny`. Domain allowlisting
   happens in m4code's proxy.
3. **Filesystem:** explicit read grants, `deniedPaths` last for secrets and
   the broker/agent sockets (Docker, `ssh-agent`, `gpg-agent`); dedicated
   directory for the m4code broker socket.
4. **Keep off:** `guiAccess`, `keychainAccess`, `profileOverride`, Apple
   Events/`lsopen`.
5. **Track the deprecation.** Pin a macOS test matrix including the SDK 27
   transition; if Apple removes custom-profile support, the only generic
   replacement identified is a VM (not researched here).

## 8. What each platform can and cannot guarantee

Written as claims m4code could honestly make. "Can" items are what the
sources describe the mechanism as doing; none were tested by me.

**Windows (PSEC)**
- Can: deny writes and reads outside granted paths for the jailed process tree
  without admin prompts or host ACL changes; filter direct outbound traffic
  per container by address, port and protocol, and force traffic through a
  proxy; keep the policy for the life of the workload; kill the tree on exit.
- Cannot (stated by sources): filter by hostname in the OS (needs the proxy);
  cover detached processes after the foreground process exits; expand a
  running jail; apply deny-globs to files created after launch; give the proxy
  mapping a working host-to-container listener on build 26691.1002.
- Unknown: whether the kernel/WFP layer has bypass classes like the
  SID-keyed WFP gaps srt documents (impersonation and derived tokens, DNS
  client service, SMB redirector); whether COM/RPC surrogate execution (Task
  Scheduler, WMI) is reachable from a PSEC container; Schannel behavior;
  behavior on a build between the floor and the probe passing.
- Not guaranteed by any OS jail: exfiltration through allowed channels
  (the model API, an allowlisted domain), kernel exploits, social engineering
  of the approval flow.

**Windows (dedicated user, if ever added)**
- Can: write and read confinement on NTFS with ACEs; SID-keyed outbound block.
- Cannot: close the documented network-fence gaps; hide files created after
  launch; avoid persistent ACL and account changes; run Schannel clients.

**macOS (Seatbelt)**
- Can: kernel-enforced default-deny read/write by subtree with deny
  overrides; block or allow outbound IP as a whole; confine egress to one
  loopback port; block UI/clipboard/injection by Mach service.
- Cannot: scope network by host/CIDR/port; scope inbound by peer or address;
  prevent a process that binds `0.0.0.0` from being LAN-reachable; stop the
  system resolver from servicing lookups (per srt); guarantee the
  mechanism's future given Apple's deprecation.

## 9. Claim ledger

**Confirmed (stated by the owning source, read directly)**
- Claude Code sandbox: shell-only, not on native Windows, bypass auto-approves
  the unsandboxed retry.
- Restricted tokens: two access checks; `WRITE_RESTRICTED` affects write
  checks only; Codex unelevated therefore cannot deny reads.
- Job objects: no filesystem/network control; WMI children escape.
- Codex Windows backends: `mxc` / `elevated` / `unelevated`; elevated =
  sandbox users + firewall + ACLs + WFP, admin setup; deny-read snapshot.
- srt-win design and its listed bypasses; Schannel failure; profile grant.
- MXC: backends, policy schema, tier logic, OS floor table, PSEC call
  sequence, network model, Seatbelt semantics, host-prep requirements.
- `sandbox-exec` and `sandbox_init` are marked DEPRECATED (mirror of Apple's
  pages); named profiles banned for SDK >= 27.
- Windows Sandbox: VM, edition limits, single instance.
- `mxc-sdk` 1.0.1, MIT, created 2026-10-06; openai/codex Apache-2.0;
  microsoft/mxc MIT; sandbox-runtime Apache-2.0.

**Inferred (my reading; not stated)**
- Codex elevated's SID-keyed firewall/WFP has the same bypass class srt lists.
- Seatbelt regex rules may catch files created after launch.
- App Sandbox cannot wrap arbitrary toolchains.
- Approved escapes map to broker-performed operations or relaunch.
- A read allowlist (instead of Codex's deny-list) is feasible for toolchains.
- DNS lookups are serviced outside the Seatbelt jail (srt says so; unchecked).

**Unverified (needs a spike or a source I did not have)**
- Whether `sandbox-exec`/custom SBPL stops working on macOS 27+.
- cargo, npm, git, rustup behavior under PSEC with a read allowlist,
  including Schannel/TLS under a PSEC container.
- `allowedProxyPeer` end to end; named-pipe access from a PSEC container;
  surrogate-execution routes from a PSEC container.
- Whether this dev box (26200.9445) passes the PSEC runtime probe.
- Public API documentation for `CreateProcessSecurityEnvironment` (none found
  on Microsoft Learn).
- Whether Windows Developer Blog's "AppContainer (Windows)" label for the
  process container matches the repo's description of BaseContainer as a
  distinct, native OS sandbox API; I report both as written.
- Whether Claude Code adopts MXC; one secondary summary said it was expected,
  not checked against a primary source.
- Any Mac behavior at all (nothing was run).

## 10. Inputs for #10 (sandbox policy) and suggested spike

- Decide the jail unit: harness-process tree (this document's premise) vs
  per-command wrapping; and how the harness's credentials and model egress
  coexist with secret denial (section 3).
- Decide behavior when the primitive is missing: refuse vs a weaker second
  tier.
- Spike (Windows): run `wxc-exec --probe` on the dev box; launch `claude`
  under a PSEC policy with read allowlist + proxy-only egress; run cargo
  build, `cargo fetch` via proxy, npm install, git status/commit; check
  Schannel, upward marker lookups, named-pipe/loopback broker channel.
- Spike (macOS): same matrix with MXC's Seatbelt backend; confirm
  deny-glob and new-file behavior; confirm the `claude` binary and its
  keychain/credential use under `keychainAccess: false`.
- Both: measure startup overhead and what a refusal looks like to the model.
