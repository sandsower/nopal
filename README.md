# Nopal

Nopal is an opinionated Pi distribution with deterministic workflow, gate, and action-policy enforcement.
Its canonical domain is [nopal.sh](https://nopal.sh), and its source repository is [`sandsower/nopal`](https://github.com/sandsower/nopal).

Pi owns interaction, models, tools, and sessions.
Beislið owns prose-first workflow meaning and lifecycle guidance.
Nopal Core owns deterministic compilation, policy composition, gate selection, receipt validation, and Workflow Run Ledger evidence.

## Current status

Nopal v0.3 is a clean break from the v0.2 agent-management product.
Version v0.2.16 is the final release that presents Field as the product.
Legacy Field, desktop, coordination, Rondo, Memento, and Herdr code remains temporary migration residue until the v0.3 removal slice lands.
Those components are not supported public launch routes in v0.3.

Bare `nopal` is the canonical launch command.
It validates the effective project contract, initializes enforcement, and replaces itself with Pi.
It never falls back to an unenforced Pi session when initialization fails.

## Installation

Each GitHub release provides the `nopal` binary for Apple Silicon macOS, Intel macOS, and x86-64 Linux.
Install the matching archive and ensure `nopal`, exact Pi `0.80.6`, `beislid`, and the official Node.js `22.22.0` distribution are available on `PATH`.
Package-manager rebuilds of Node are not equivalent because production launch verifies the canonical executable bytes.

Download Node from `https://nodejs.org/download/release/v22.22.0/`, then verify the selected executable:

```sh
node --version
openssl dgst -sha256 "$(command -v node)"
```

The version must report `v22.22.0` and the executable digest must match the release platform:

| Platform | Official Node executable SHA-256 |
| --- | --- |
| Apple Silicon macOS | `913b144fdb40638b1acef7974ab3c33fbd527cc0974cb5da467ab1e6ac51b4d4` |
| Intel macOS | `bf0e0ff20d4e5a16436d1ec372e47161e52be8e487db8070ae3f06b01efbba0c` |
| x86-64 Linux | `1bec56ef7cfa9a76f3e0b7c0a87f220eb73f23102b9c0b4c7529a3f7c3ce7c31` |

Nopal reports an explicit expected/observed digest error when a different runtime is selected.
Nopal v0.3 does not require tmux for its canonical Pi launch.

## Quick start

From any supported Git repository:

```sh
nopal
```

A completely unconfigured repository receives a checked-in Nopal and Beislið baseline with evidence-backed validation gates.
Nopal detects configured root ecosystems and only those workspaces explicitly declared by root manifests.
Explicit repository tasks and package scripts take precedence over generated ecosystem defaults.
Conflicting tool choices stop with actionable diagnostics.
An unknown repository receives the baseline but does not start Pi until explicit gates are added.
Partial Nopal, existing Beislið-only, and legacy pre-v0.3 project state is preserved and rejected rather than overwritten.

Arguments after `--` pass directly to Pi:

```sh
nopal -- --provider anthropic --model claude-sonnet-4-5
```

Useful read-only inspection commands include:

```sh
nopal --dry-run --json
nopal doctor --json
nopal sync --json
nopal update --json
nopal update --write --json
nopal validate --json
nopal verify --json
nopal gates list --json
nopal policy decide --mode supervised_auto --action git.push --class git_remote --json
nopal ledger resume --run-id <run-id> --flow enforcement --json
nopal ledger continue --run-id <run-id> --flow enforcement --json
```

The internal `nopal enforcement` machine API is hidden from public help and reserved for the trusted bundled Pi adapter.

## Project contract

A configured project uses checked-in files under `.nopal/`:

- `.nopal/nopal.jsonc` identifies the project contract and profile.
- `.nopal/bundle.jsonc` declares portable builtin, workspace, and npm package identities and their exported Pi resources.
- `.nopal/nopal.lock` records exact versions and artifact, installed-tree, and resource integrity.
- `.nopal/policy.jsonc` declares repository action policy.
- `.nopal/gates.jsonc` declares deterministic gates and records versioned first-run template provenance when generated.
- `.beislid/workflow.md` provides prose guidance and optional typed enforcement blocks.

Nopal reads enforcement authority only from recognized typed `beislid:*` Markdown fences.
Ordinary prose has no authority.
Invalid recognized blocks fail closed, while unrecognized Beislið-owned blocks remain diagnostic-only.

## Distribution synchronization

Bare `nopal` only verifies local locked evidence and always starts Pi offline through the canonical entrypoint, complete platform-specific package and dependency closure, and exact Node runtime pinned for `@earendil-works/pi-coding-agent`.
The closure digest covers manifests, dependency bytes, native/WASM assets, symlink targets, and executable modes.
After verification, launch clones the Pi closure and the official Node executable into a private read-only content-addressed run snapshot, rehashes both, and executes only from that snapshot.
The pinned Node build has no non-system dynamic-library closure, so mutable package-manager libraries cannot sit outside its identity.
It never installs packages, updates versions, or contacts a registry.
Missing or changed package and resource bytes prevent launch.

`nopal sync` installs and verifies the checked-in lock without changing it.
The v0.3 bundle accepts only exact semantic-version requirements, optionally prefixed by `=`, so changing a requested version is an explicit reviewed contract edit.
`nopal update` resolves the current bundle into a lock proposal, while `nopal update --write` intentionally replaces the checked-in lock.
Npm packages are verified against SHA-512 SRI and extracted without links, traversal, special files, duplicate paths, or unbounded archive expansion.

Ambient Pi resources are disabled unless `.nopal/bundle.jsonc` explicitly inherits a non-executable resource kind.
Ambient and third-party executable extensions remain forbidden by the enforced v0.3 profile.

## Policy composition

User, repository, and compiled workflow policy compose through the fixed restriction lattice:

```text
allow < ask < deny
```

Repository and workflow policy may tighten user policy but cannot weaken it.
The enforced distribution pins `supervised_auto`; ambient environment variables cannot select another mode.
Normal `git push` and destructive push forms have distinct action identities.
Force options, deletion refspecs, mirror, prune, and equivalent destructive forms compile to `git.push_force`, which remains a non-approvable safety-floor denial in every Core mode.
Continuous enforcement maps ordinary `git.push` to the `pre_pr` stage while mediating every supported Pi tool throughout the session.

## Enforcement flow

For a protected Pi tool call, the bundled adapter:

1. Classifies the complete shell envelope before execution.
2. Rejects compound, dynamic, redirected, expanded, or otherwise unsupported shell syntax rather than authorizing only part of it.
3. Sends the exact intent to one private verification transaction through the resolved launch binary.
4. Lets the trusted CLI adapter execute each missing Core-selected gate in a canonical-root-confined, non-profile, output-bounded, process-group-bounded, capability-free subprocess.
5. Resolves executors for every potentially applicable `continuous`, `per_edit`, `pre_commit`, and `pre_pr` gate before launch, independent of current policy, selectors, or changed files, pins canonical paths and bytes in a run-private alias directory, and revalidates that manifest before every private authorization transition.
6. Uses a private gate home and cache paths so proof tools do not create authority or cache files in the repository.
7. Records the observed exit code against the exact contract, workspace, executor identity, gate definition, and authorization binding.
8. Resolves `ask` only through Pi's user interface and durably binds the response to the exact action context.
9. Reauthorizes the action, consumes any one-shot approval, and releases the original tool call only when current authenticated evidence exists.
10. Records success, error, cancellation, or shutdown interruption against the exact authenticated release before its lease is cleared.

Pi receives an explicit environment allowlist, a system-only base `PATH`, a private per-run `HOME`, and a private configuration directory containing only bounded no-follow authentication state.
Ambient user Git, curl, npm, pip, Kubernetes, Cargo, and related tool configuration is disabled or redirected to that protected empty home; system and repository Git configuration remains observed and bound by the workspace adapter.
External transfer authorization supports only the exact shape `curl --disable <literal-http-url>` (or `-q`), with no other options; redirects, config files, proxies, URL globbing, multiple targets, alternate protocols, and unaudited transfer tools fail closed.
Executable, symlinked, or multiply-linked project settings fail launch, and the exact settings file is revalidated before every private authorization transaction, so ambient Pi settings cannot select a custom shell, command prefix, or executable resource.
Executable Git and ripgrep environment or configuration carriers fail closed before protected effects.

Nopal Core never executes gate commands.
The trusted CLI adapter owns gate execution and durable effects.
The Pi adapter retains classification, concurrency leases, interactive approval, protected-call release, and exact result matching.

`nopal verify` uses the same local verification transaction, policy compiler, gate selector, executor manifest, gate runner, receipt codec, and ledger publication path for the fixed `git.push` pre-PR boundary.
It performs no push, launches no Pi process, contacts no remote service, and cannot approve an `ask` decision.
An approval-required headless run stops as interrupted evidence and creates no release.

Every executable Pi extension is verified against an identity embedded in the installed Nopal binary before Pi starts.
The launch probe requires the complete audited `bash`, `edit`, `find`, `grep`, `ls`, `read`, and `write` catalog after the guard is installed.
Missing built-ins, unknown active tools, and caller-supplied tool-catalog overrides block launch.
The default bundle includes only the enforcement adapter, and enforced launch rejects ambient, injected, or untrusted sibling extensions.
The adapter also protects its source, the Nopal executable, project authority files, executable Pi project settings, user policy, and enforcement ledger state from agent tools for the entire session.
Adapter subprocesses use the resolved current Nopal executable rather than a `PATH` lookup.
Filesystem inspection uses Pi's audited `read`, `grep`, `find`, and `ls` tools rather than ambient shell executables.
The shell read grammar admits only non-file identity commands (`pwd`, `uname`, `whoami`, and `id`) plus separately audited Git reads; commands whose option or positional surfaces can mutate, execute helpers, or disclose secrets fail closed.

## Gate receipts

Each passing receipt is immutable and stored under its exact gate plus authorization binding, so concurrent calls cannot replace one another's evidence.
A passing receipt binds:

- the launch, session, tool call, tool name, and canonical input;
- the action identity and exact target;
- repository, worktree, placement, and changed-file selector evidence;
- the effective policy, workflow, and distribution contract;
- the exact run-private gate executor manifest, gate definition, and observed exit code;
- an ephemeral per-launch receipt capability.

The capability lives only in an anonymous inherited descriptor and is never published to the run directory.
The launcher maps that descriptor into the trusted extension, which reads and closes it before agent activity and retains only the private value.
Each private adapter subprocess receives a fresh unlinked mode-0600 one-shot capability channel; the matching proof travels through bounded stdin, never through command-line arguments or gate environments.
The capability never enters project data, run artifacts, gate processes, or ledger events.
The internal Nopal CLI authenticates receipts with HMAC-SHA256.
A forged, unsigned, stale, or context-mismatched receipt cannot authorize an action.

## Workflow Run Ledger

Enforcement evidence lives outside the repository at:

```text
${BEISLID_STATE_DIR:-~/.local/state/beislid}/runs/enforcement/<repo_hash>/<run_id>/
```

Independent read-only calls may remain in flight concurrently, but a mutator is exclusive against every other protected call.
The ledger records lifecycle transitions, workflow events, checkpoints, gate attempts, policy decisions, approvals, passing receipts, exact one-shot releases, and terminal success, error, cancellation, or interruption outcomes.
Each mutation publishes one immutable revisioned transaction before updating its compatible `run.json`, `events.jsonl`, transcript, checkpoint, and artifact projections.
A later process validates the digest chain and repairs only a projection that matches a committed boundary, without duplicating evidence.
Exact resume queries return the journal revision, transaction digest, resume epoch, redacted continuation, expected next action, and whether all protected proof must be rerun.
`ledger continue` is the only transition from interrupted back to running, increments the resume epoch, and records that re-verification is mandatory.
All filesystem scans, payloads, reports, event counts, gate attempts, lock waits, and continuation fields are bounded.
It is a bounded evidence surface, not a dashboard, session registry, or coordination product.

## Workspace

The active v0.3 path currently centers on:

| Path | Responsibility |
|---|---|
| `crates/nopal-core` | Typed compilation, restrictive policy composition, gate selection, receipt validation, and ledger evidence |
| `crates/nopal-cli` | Bare launch, shared local verification transaction, confined gate execution, durable effects, hidden adapter machine API, and Pi handoff |
| `extensions/policy-gate` | Continuous Pi tool-call classification, approval, lease, release, and outcome mediation |
| `.nopal/` | Checked-in project, policy, gate, package, and exact distribution-lock contracts |
| `docs/adr/0012-reset-nopal-to-an-enforced-pi-distribution.md` | v0.3 product and assurance-boundary decision |
| `docs/adr/0013-lock-portable-project-distributions.md` | Offline launch and explicit package synchronization decision |
| `docs/adr/0015-mediate-every-protected-pi-tool-call.md` | Exact continuous action authorization and Pi guard decision |

Other legacy crates remain only until the dedicated removal slice deletes them from active main.

## Development

Before proposing a change, run:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm test
```

The real Pi enforcement proof is opt-in because it requires an installed Pi binary:

```sh
NOPAL_RUN_REAL_PI_ENFORCEMENT_E2E=1 \
NOPAL_TEST_PI_BIN="$(command -v pi)" \
cargo test -p nopal-cli --test real_pi_enforcement -- --ignored --nocapture --test-threads=1
```

That proof uses a deterministic local provider and local bare Git remote.
It covers every built-in Pi tool adapter and protected action class, allowed effects, explicit RPC approval, denial, stale and foreign receipts, force-push and shell bypass attempts, repository-policy protection, trusted adapter acknowledgement and identity, resolved CLI identity, and durable ledger evidence without an external network provider.

## Architectural decisions

Durable decisions live under [`docs/adr/`](docs/adr/README.md).
Start with [ADR 0012](docs/adr/0012-reset-nopal-to-an-enforced-pi-distribution.md) for the v0.3 product boundary, [ADR 0013](docs/adr/0013-lock-portable-project-distributions.md) for portable distribution locking, and [ADR 0015](docs/adr/0015-mediate-every-protected-pi-tool-call.md) for continuous tool-call authorization.
