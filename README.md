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
Install the matching archive and ensure `nopal`, `pi`, and `beislid` are available on `PATH`.
Nopal v0.3 does not require tmux for its canonical Pi launch.

## Quick start

From any supported Git repository:

```sh
nopal
```

A completely unconfigured repository receives the checked-in Nopal and Beislið baseline before Pi starts.
Partial Nopal, existing Beislið-only, and legacy pre-v0.3 project state is preserved and rejected rather than overwritten.

Arguments after `--` pass directly to Pi:

```sh
nopal -- --provider anthropic --model claude-sonnet-4-5
```

Useful read-only inspection commands include:

```sh
nopal --dry-run --json
nopal sync --json
nopal update --json
nopal update --write --json
nopal validate --json
nopal gates list --json
nopal policy decide --mode supervised_auto --action git.push --class git_remote --json
nopal ledger resume --flow enforcement --json
```

The internal `nopal enforcement` machine API is hidden from public help and reserved for the trusted bundled Pi adapter.

## Project contract

A configured project uses checked-in files under `.nopal/`:

- `.nopal/nopal.jsonc` identifies the project contract and profile.
- `.nopal/bundle.jsonc` declares portable builtin, workspace, and npm package identities and their exported Pi resources.
- `.nopal/nopal.lock` records exact versions and artifact, installed-tree, and resource integrity.
- `.nopal/policy.jsonc` declares repository action policy.
- `.nopal/gates.jsonc` declares deterministic gates.
- `.beislid/workflow.md` provides prose guidance and optional typed enforcement blocks.

Nopal reads enforcement authority only from recognized typed `beislid:*` Markdown fences.
Ordinary prose has no authority.
Invalid recognized blocks fail closed, while unrecognized Beislið-owned blocks remain diagnostic-only.

## Distribution synchronization

Bare `nopal` only verifies local locked evidence and always starts Pi offline.
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
Normal `git push` and force push have distinct action identities.
The walking skeleton maps `git.push` to the `pre_pr` gate stage and denies `git.push_force` through policy.

## Enforcement flow

For a protected Pi tool call, the bundled adapter:

1. Classifies the complete shell envelope before execution.
2. Rejects compound, dynamic, redirected, expanded, or otherwise unsupported shell syntax rather than authorizing only part of it.
3. Requests an action plan from Nopal Core through the resolved launch binary.
4. Resolves `ask` only through Pi's user interface and binds approval to the exact contract and workspace context.
5. Executes each missing gate returned by Core.
6. Records the observed exit code against the original contract, workspace, and gate-definition digests.
7. Reauthorizes the action and releases the original tool call only when current authenticated evidence exists.

Nopal Core never executes gate commands.
The Pi adapter is the execution boundary.

Every executable Pi extension is verified against an identity embedded in the installed Nopal binary before Pi starts.
The default bundle includes only the enforcement adapter, and enforced launch rejects ambient, injected, or untrusted sibling extensions.
The adapter also protects its source, the Nopal executable, project authority files, user policy, and enforcement ledger state from agent tools.
Adapter subprocesses use the resolved current Nopal executable rather than a `PATH` lookup.

## Gate receipts

A passing receipt binds:

- the action identity;
- repository and workspace content;
- the effective enforcement contract;
- the exact gate definition;
- the observed exit code;
- an ephemeral per-launch receipt capability.

The capability lives in a mode-0600 file inside the protected enforcement run directory.
It never enters the Pi process environment, extension globals, subprocess arguments, project data, or ledger events.
The internal Nopal CLI reads it directly to authenticate receipts with HMAC-SHA256.
A forged, unsigned, stale, or context-mismatched receipt cannot authorize an action.

## Workflow Run Ledger

Enforcement evidence lives outside the repository at:

```text
${BEISLID_STATE_DIR:-~/.local/state/beislid}/runs/enforcement/<repo_hash>/<run_id>/
```

The ledger records action decisions, gate attempts, passing receipts, checkpoints, interruption, and outcomes.
It is a bounded evidence surface, not a dashboard, session registry, or coordination product.

## Workspace

The active v0.3 path currently centers on:

| Path | Responsibility |
|---|---|
| `crates/nopal-core` | Typed compilation, restrictive policy composition, gate selection, receipt validation, and ledger evidence |
| `crates/nopal-cli` | Bare launch, fail-closed initialization, hidden adapter machine API, and Pi handoff |
| `extensions/policy-gate` | Continuous Pi tool-call mediation and adapter-owned gate execution |
| `.nopal/` | Checked-in project, policy, gate, package, and exact distribution-lock contracts |
| `docs/adr/0012-reset-nopal-to-an-enforced-pi-distribution.md` | v0.3 product and assurance-boundary decision |
| `docs/adr/0013-lock-portable-project-distributions.md` | Offline launch and explicit package synchronization decision |

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
NOPAL_PI_BIN="$(command -v pi)" \
cargo test -p nopal-cli --test real_pi_enforcement -- --ignored --nocapture --test-threads=1
```

That proof uses a deterministic local provider and local bare Git remote.
It covers allowed push, stale-receipt rerun, force-push denial, unsupported shell bypass attempts, authority-file protection, trusted adapter identity, resolved CLI identity, and durable ledger evidence without an external network provider.

## Architectural decisions

Durable decisions live under [`docs/adr/`](docs/adr/README.md).
Start with [ADR 0012](docs/adr/0012-reset-nopal-to-an-enforced-pi-distribution.md) for the v0.3 product boundary and [ADR 0013](docs/adr/0013-lock-portable-project-distributions.md) for portable distribution locking.
