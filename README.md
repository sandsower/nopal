# Nopal

Nopal is an opinionated Pi distribution with deterministic workflow, gate, and action-policy enforcement.
Its canonical domain is [nopal.sh](https://nopal.sh), and its source repository is [`sandsower/nopal`](https://github.com/sandsower/nopal).

Pi owns interaction, models, tools, and sessions.
Pi owns its built-in prompts, themes, and host defaults.
Beislið owns prose-first workflow meaning and the curated skills shipped by Nopal.
Nopal owns deterministic project defaults but does not replace Pi's interaction resources.
Nopal does not ship a parallel prompt or theme layer.
Nopal Core owns deterministic compilation, policy composition, gate selection, receipt validation, and Workflow Run Ledger evidence.
The Nopal CLI owns confined process execution, durable effects, runtime integrity, and the handoff to Pi.

## Assurance boundary

Nopal's guarantee applies only to Pi sessions started through the installed `nopal` launcher after enforcement initialization succeeds.
Running `pi` directly starts a plain Pi session and carries no Nopal enforcement guarantee.
Nopal never falls back to an unenforced Pi session when initialization fails.

Nopal v0.3 is a clean break from the earlier agent-management product.
`v0.2.16` is the final release before transformation work began.
Versions `v0.2.17` through `v0.2.21` are transitional releases that still contain management-era architecture, and `v0.2.21` is the final v0.2 release.
The former management UI, desktop runtime, coordination protocols, and compatibility commands are absent from the active v0.3 product and release archives.
Git history and the final v0.2 release marker preserve that implementation.
Detectable old commands and project modules stop with `nopal.migration/v1` diagnostics that explain the supported v0.3 replacement without executing an alias.

## Install a release

A v0.3 release archive is self-contained for its target platform.
It contains the Nopal CLI, official Node.js `22.22.0`, exact Pi `0.80.6` with its runtime closure, the Nopal policy adapter, pinned Beislið skills, licenses, provenance, and the installer.
It does not depend on a system Pi, Node, or Beislið installation.

Download the archive for Apple Silicon macOS, Intel macOS, or x86-64 Linux with glibc 2.35 or newer and verify it against `SHA256SUMS` from the same GitHub release.
Then install it under an absolute prefix:

```sh
tar -xzf nopal-v<version>-<target>.tar.gz
cd nopal-v<version>-<target>
./install install "$HOME/.local"
export PATH="$HOME/.local/bin:$PATH"
```

The installer copies the immutable versioned release before atomically switching `$prefix/lib/nopal/current`.
Installing another release preserves the former target as `$prefix/lib/nopal/previous`.
The same installer can exchange the current and previous targets without a network request:

```sh
"$HOME/.local/lib/nopal/current/install" rollback "$HOME/.local"
```

Installation and rollback use only archive bytes already on disk.
An installed `nopal` resolves packaged Pi and Node relative to its own executable before considering source-development or explicit test overrides.
Every launch validates the complete Pi tree, official Node executable, bundled adapter, curated resources, project lock, and executable identity before starting Pi offline.

## Start Nopal

From a supported Git repository:

```sh
nopal
```

A completely unconfigured repository receives a checked-in Nopal and Beislið baseline with evidence-backed validation gates.
Nopal detects configured root ecosystems and only workspaces explicitly declared by root manifests.
Explicit repository tasks and package scripts take precedence over generated ecosystem defaults.
Conflicting tool choices stop with actionable diagnostics.
An unknown repository receives the baseline but does not start Pi until explicit gates are added.
Partial Nopal, existing Beislið-only, and detectable pre-v0.3 project state is preserved and rejected rather than overwritten.

Arguments after `--` pass directly to Pi:

```sh
nopal -- --provider anthropic --model claude-sonnet-4-5
```

Useful inspection and maintenance commands include:

```sh
nopal --dry-run --json
nopal doctor --json
nopal validate --json
nopal verify --json
nopal sync --json
nopal update --json
nopal update --write --json
nopal gates list --json
nopal policy decide --mode supervised_auto --action git.push --class git_remote --json
nopal ledger resume --run-id <run-id> --flow enforcement --json
nopal ledger continue --run-id <run-id> --flow enforcement --json
```

The internal `nopal enforcement` API is hidden from public help and reserved for the authenticated bundled Pi adapter.

## Project contract and synchronization

A configured project uses checked-in authority under `.nopal/` plus `.beislid/workflow.md`:

- `.nopal/nopal.jsonc` identifies the project contract and required modules.
- `.nopal/bundle.jsonc` declares portable builtin, workspace, and npm package identities plus exported Pi resources.
- `.nopal/nopal.lock` records exact versions and artifact, installed-tree, and resource integrity.
- `.nopal/policy.jsonc` declares repository action policy.
- `.nopal/gates.jsonc` declares deterministic gates and versioned first-run template provenance.
- `.beislid/workflow.md` provides prose guidance and optional typed enforcement blocks.

Bare `nopal` verifies locked local evidence and contacts no package registry.
Missing or changed package bytes prevent launch.
`nopal sync` installs and verifies the existing lock without changing it.
`nopal update` previews intentional resolution of the current bundle, while `nopal update --write` replaces the checked-in lock.
Exact version requirements and reviewed lock changes make synchronization distinct from release installation.

Nopal reads enforcement authority only from recognized typed `beislid:*` Markdown fences.
Ordinary prose has no authority.
Invalid recognized blocks fail closed, while unrecognized Beislið-owned blocks remain diagnostic-only.

## Deterministic enforcement

User, repository, and compiled workflow policy compose through the fixed restriction lattice:

```text
allow < ask < deny
```

A narrower source can tighten an earlier decision but can never weaken it.
The enforced distribution pins `supervised_auto`.
Force pushes and equivalent destructive forms remain non-approvable safety-floor denials.
Headless verification cannot approve an `ask` decision or create an action release.

For each protected Pi tool call, the bundled adapter classifies the exact intent and asks one shared Rust verification transaction for a plan.
The CLI executes missing Core-selected gates in bounded, environment-sanitized, repository-confined process groups and records their exact outcomes.
Interactive approval is bound to the exact launch, tool call, action, target, contract, workspace, and fresh evidence.
Only a matching one-shot release permits Pi to execute the original protected call.
Success, error, cancellation, and interruption are recorded against that release.

Nopal Core never executes commands, searches `PATH`, prompts a user, contacts a registry, or publishes effects.
The CLI and Pi adapters own those effects.
Gate children inherit no enforcement capability.

`nopal verify` uses the same planner, selector, gate runner, evidence codec, receipt codec, and ledger transaction as interactive enforcement for the local pre-PR `git.push` boundary.
It launches no Pi process, performs no push, and contacts no remote service.

Every Nopal-launched Pi session uses a private content-addressed runtime snapshot and a private configuration home containing only bounded authentication state.
The extension protects project authority, runtime identity, policy, and ledger state from agent tools for the complete session.
Unsupported shell syntax, unknown active tools, untrusted executable resources, and changed runtime bytes fail closed.

## Workflow Run Ledger

Enforcement evidence lives outside the repository at:

```text
${BEISLID_STATE_DIR:-~/.local/state/beislid}/runs/enforcement/<repo_hash>/<run_id>/
```

The bounded ledger records lifecycle transitions, workflow events, checkpoints, gate attempts, decisions, approvals, receipts, releases, and terminal outcomes.
Each mutation publishes an immutable revisioned transaction before updating compatible projections.
Replay validates the digest chain and repairs only projections matching a committed boundary.
Resume creates a fresh epoch and requires protected proof to be observed again.
The ledger is an evidence surface, not a session or coordination service.

## Source layout

| Path | Responsibility |
|---|---|
| `crates/nopal-core` | Pure typed compilation, policy composition, gate selection, receipt validation, and ledger transitions |
| `crates/nopal-cli` | Launch, shared verification, confined execution, durable effects, distribution integrity, and Pi handoff |
| `crates/nopal-ledger-json` | Canonical Python-compatible ledger JSON encoding |
| `extensions/policy-gate` | Continuous Pi tool-call classification, approval, lease, release, and outcome mediation |
| `resources/beislid` | Pinned curated Beislið skills, license, and source provenance |
| `.nopal/` | Checked-in project, policy, gate, package, and exact lock contracts |
| `docs/adr/` | Durable architectural decisions |

## Development and verification

Run the complete local verification surface before proposing a change:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
npm test
bash scripts/test-release-contracts.sh
bash scripts/check-public-tree.sh
bash scripts/check-active-tree-identity.sh
git diff --check
```

The real Pi proof requires an installed Pi binary and remains opt-in for source development:

```sh
NOPAL_RUN_REAL_PI_ENFORCEMENT_E2E=1 \
NOPAL_TEST_PI_BIN="$(command -v pi)" \
cargo test -p nopal-cli --test real_pi_enforcement -- --ignored --nocapture --test-threads=1
```

That proof uses a deterministic local provider and local bare Git remote.
It exercises allowed, approval-required, denied, stale, substituted, concurrent, interrupted, and terminal tool-call paths without an external model provider.

Start with [ADR 0012](docs/adr/0012-reset-nopal-to-an-enforced-pi-distribution.md) for the product and assurance boundary.
See [ADR 0013](docs/adr/0013-lock-portable-project-distributions.md) for distribution locking, [ADR 0015](docs/adr/0015-mediate-every-protected-pi-tool-call.md) for continuous authorization, and [ADR 0016](docs/adr/0016-journal-workflow-runs-and-share-local-verification.md) for workflow evidence.
