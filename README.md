# Nopal

Nopal is the next-generation agentic dev harness: one deterministic core (Nopal Core) with multiple thin surfaces over it, delivered as a distribution over Pi.
Its canonical domain is [nopal.sh](https://nopal.sh), and its source repository is [`sandsower/nopal`](https://github.com/sandsower/nopal).
This repo is its physical umbrella - a Cargo workspace holding the `nopal` binary (deterministic core plus product-facing coordinator), the two inter-product contract surfaces (execution, memory), and Nopal's own versioned config/envelope and process/proof-artifact surface.

Nopal Core decides, selects, and explains; it never executes gates, and its cold commands make no network, shell, or agent calls.
Four surfaces are deliberately warm: bare `nopal` attaches to or creates the tmux-backed Field; `nopal cli` is the single-session Pi launcher, running its own cold gates and, once they pass, `exec`ing into Pi; `nopal ledger` records durable run state under the state dir and probes git; `nopal bridge herdr` publishes the versioned Field feed to a local Herdr socket.

## Workspace

| Path | What it is |
|---|---|
| `crates/nopal-ledger-json` | Leaf lib: ledger-scoped JSON value/parser preserving Python-canonical numeric text |
| `crates/nopal-core` | The engine: `.nopal/` discovery, `nopal.project/v1` manifest, profile/module validation, `nopal.bundle/v1` resolution, stable-code diagnostics, TOON encoding, and the coordinator surface (readiness, policy placement, Rondo Core lifecycle) |
| `crates/nopal-feed-client` | Host-neutral consumer models for Nopal's versioned coordination feeds; shared by the Field and external host adapters |
| `crates/nopal-rondo-client` | Client for Nopal's Rondo Core submission, observation, and lifecycle integration |
| `crates/nopal-field` | The tmux-backed flagship Field client and native seat-management surface |
| `crates/nopal-cli` | The `nopal` binary: thin clap wrapper over nopal-core, plus `nopal cli`'s plan/exec handoff to Pi and bare invocation's dispatch to the Field |
| `contracts/` | The two inter-product contracts (execution, memory), owner/schema pointers, versioning rules, and distro-manifest seed |
| `docs/surface/` | Nopal's own versioned config/envelope and process/proof-artifact surface docs |
| `conformance/` | Fixture and deterministic runner homes for the contracts and Nopal's own surface |

## Installation

Nopal requires `tmux` for its default Field surface.
Install tmux with Homebrew (`brew install tmux`) or your Linux package manager before starting the Field.

Each GitHub Release provides the `nopal` binary for Apple Silicon macOS, Intel macOS, and x86-64 Linux:

| Platform | Archive |
|---|---|
| Apple Silicon macOS | `nopal-vX.Y.Z-aarch64-apple-darwin.tar.gz` |
| Intel macOS | `nopal-vX.Y.Z-x86_64-apple-darwin.tar.gz` |
| x86-64 Linux | `nopal-vX.Y.Z-x86_64-unknown-linux-gnu.tar.gz` |

Download the matching archive and `SHA256SUMS` from the repository's GitHub Release, verify the one archive you downloaded, then install the binary somewhere on `PATH`:

```sh
stem=nopal-vX.Y.Z-aarch64-apple-darwin
archive="$stem.tar.gz"
awk -v file="$archive" '$2 == file { print }' SHA256SUMS | shasum -a 256 -c - # macOS
# awk -v file="$archive" '$2 == file { print }' SHA256SUMS | sha256sum -c -   # Linux
tar -xzf "$archive"
mkdir -p "$HOME/.local/bin"
install -m 0755 "$stem/nopal" "$HOME/.local/bin/nopal"
install -m 0755 "$stem/rondo" "$HOME/.local/bin/rondo"
install -m 0644 "$stem/rondo-runtime.json" "$HOME/.local/bin/rondo-runtime.json"
export PATH="$HOME/.local/bin:$PATH"
command -v nopal
nopal info
```

Add the `PATH` export to your shell profile so later terminals can find the installed binary.
Each archive keeps `nopal`, its version-pinned sibling `rondo` runtime, `rondo-runtime.json` provenance, `LICENSE`, `README.md`, `NOTICE.md`, `Rondo-LICENSE`, `Rondo-NOTICE`, and `THIRD_PARTY_LICENSES.html` inside its versioned top-level directory.
The bundled Rondo artifact is an escript and requires Erlang/OTP 28 on the installed machine, but it needs no separate Rondo checkout or installation.
Nopal verifies the live Core contract version and process identity before adopting that sibling runtime.

The Homebrew formula and `sandsower/homebrew-tap` are deferred work.
Until that tap exists, `brew install nopal` is not a supported installation path.

## Usage

Until the native desktop route is ready for adoption, bare `nopal` and bare `nopal field` remain aliases for the legacy tmux-backed Field.
The explicit native route is reserved for the separately packaged `nopal-field-native` sibling and works without a TTY.
That sibling is not part of release archives while native renderer selection and packaging remain unfinished.
Until then, the route reports an actionable unavailable-sibling error instead of silently falling back to tmux.

```sh
nopal                        # bare invocation: attach to or create the tmux-backed Field
nopal field                  # same as bare `nopal`, spelled out explicitly
nopal field native           # require the separately installed native Field sibling
nopal field native --state-dir <p>
                             # native Field state root override
nopal field legacy           # explicit tmux-backed compatibility fallback
nopal cli                    # single-session Pi launcher: validate, resolve .nopal/bundle.jsonc, then exec into a pinned Pi session
nopal cli --dry-run          # print the nopal.launch/v1 plan without touching Pi
nopal cli --with-ambient     # layer the pinned bundle on top of ambient Pi resources
nopal cli --verbose          # also print the one-line nopal.launch/v1 stderr summary before exec
nopal validate        # validate manifest + profile-required modules (exit 0/1)
nopal preflights list # list nopal.gates/v1 preflights
nopal gates list      # list gates, gate sets, and selectors
nopal gates select --stage pre_pr --changed-files a.rs,b.md
                      # deterministic changed-file-aware gate selection
nopal policy evaluate  --mode <mode> --action <id> [--class <c>]... [--env <NAME>]...
nopal policy placement --mode <mode> --action <id> [--class <c>]... [--env <NAME>]...
nopal policy decide    --mode <mode> --action <id> [--class <c>]... [--env <NAME>]...
nopal export process --stdout --json
nopal export process --output .nopal/process-artifact.json
nopal export process --output .nopal/process-artifact.json --check
nopal import beislid-workflow              # preview .beislid/workflow.md -> .nopal/*.jsonc drafts
nopal import beislid-workflow --write      # write drafts, refusing existing files unless --overwrite
nopal import beislid-workflow --check      # fail if checked-in .nopal modules differ semantically
nopal ledger init --skill <skill> [--flow <f>] [--ticket-id <id>] [--ticket-title <t>] [--ticket-url <u>] [--branch <b>] [--run-id <id>]
nopal ledger event --run-id <id> [--flow <f>] --type <type> [--json-file <p>] [--summary <s>]
nopal ledger checkpoint --run-id <id> [--flow <f>] --name <name> [--json-file <p>] [--resume-hint <h>]
nopal ledger gate --run-id <id> [--flow <f>] --name <name> --envelope-file <p> [--scope <s>] [--resume-hint <h>]
nopal ledger interrupt --run-id <id> [--flow <f>] --reason <r> [--resume-hint <h>]
nopal ledger finalize --run-id <id> [--flow <f>] --status <interrupted|failed|completed> [--report-file <p>]
nopal ledger resume [--flow <f>] [--ticket-id <id>] [--branch <b>] [--include-completed]
nopal ledger dashboard [--flow <f>] [--all] [--limit <n>]
nopal ledger --state-dir <p> <subcommand> ...
                      # ledger state root override (note: precedes the subcommand)
nopal field inspect          # inspect runs, placements, gates, ledger state, and pending asks across every repo
nopal field inspect --all --rondo-events <feed>
                      # include completed runs/terminal asks and attach rondo.core/v1 run status/events
nopal field inspect --state-dir <p>
                      # Field state root override (a Field spans every repo, not just --dir)
nopal bridge herdr     # continuously publish matching live run/gate/ask state to herdr's sidebar
nopal bridge herdr --once
                      # poll once; a missing herdr socket is a successful no-op
nopal bridge herdr --state-dir <p>
                      # state root override for the child Field inspection
nopal bridge herdr --socket <p> --interval 5
                      # override the Unix socket and daemon poll interval
nopal status          # Nopal readiness + missing modules, explicitly (the cold path; bare invocation opens the Field, `nopal cli` launches instead)
nopal info            # machine-readable version + capability report (nopal.info/v1)
nopal placement       # explain the runtime placement Nopal would use for an action
nopal rondo start     # start or reuse the verified user-scoped Rondo Core
nopal rondo health    # display verified Core identity, health, state, and log paths
nopal rondo restart   # restart the verified Core when no runs are active
nopal rondo stop      # stop the verified Core when no runs are active
nopal run start       # dry-run run-start coordination; never submits AFK work
nopal run submit --manifest <path> --plot-id <id> [--state-dir <path>]
                      # submit one approved per-slice export through Rondo Core
nopal run observe --repo-id <id> --plot-id <id> --run-id <id> [--cursor <cursor>] [--state-dir <path>]
                      # fetch one bounded status and event page without cancelling the run
nopal --json ...      # versioned JSON envelopes (nopal.status/v1, nopal.validation/v1, nopal.preflights.list/v1, nopal.gates.*/v1, nopal.policy_*/v1, nopal.process_artifact/v1, nopal.beislid_import/v1, nopal.run_ledger.*/v1, nopal.ask.*/v1, nopal.field/v1, nopal.placement/v1, nopal.rondo_service/v1, nopal.run_start_dry_run/v1, nopal.run_submit/v1, nopal.run_observation/v1, nopal.launch/v1, nopal.bundle/v1, nopal.info/v1)
nopal --dir <path>    # start project discovery there (walks up to the git root to find `.nopal/`)
```

Configuration lives in `.nopal/` as JSONC: `nopal.jsonc` (the manifest) plus per-concern modules (`gates.jsonc`, `policy.jsonc`, `workflow.jsonc`, `roots.jsonc`, `integrations.jsonc`, `guidance.jsonc`), plus the coordinator's own `config.jsonc` for run-start policy and optional explicit Core endpoint settings.
Verified Rondo lifecycle state and durable logs live in the user state directory, never in the repository.

Profiles declare which modules are required:

| Profile | Required modules |
|---|---|
| `minimal` | manifest only |
| `portable` | gates, policy |
| `nopal` | gates, policy, workflow, integrations, guidance |

`nopal validate` checks module presence, JSONC parseability, and the deep `nopal.gates/v1`, `nopal.policy/v1`, `nopal.workflow/v1`, `nopal.integrations/v1`, and `nopal.guidance/v1` schemas for any module file that is present.

Nopal treats additive vocabulary membership as data compatibility where the axis can grow safely, not as a closed Rust enum. It keeps true safety lattices closed. Unknown tokens should degrade conservatively rather than silently widening behavior.

The closed lattices that remain ABI-sensitive are:

- policy decisions (`allow` < `ask` < `deny`)
- policy placements (`shared_user_runtime` < `dedicated_repo_runtime` < `dedicated_run_runtime` < `blocked`)
- run-ledger status semantics
- the protected safety floors for destructive and secret-bearing handling

No other vocabulary axis is guaranteed to remain closed; additive tokens elsewhere are treated as data, not ABI surface, and should not be mistaken for safety lattices.

`nopal export process` emits `nopal.process_artifact/v1`: normalized JSON for the parsed `.nopal/` source tree, source metadata with stable hashes, and validation diagnostics. Secret-looking keys or string literals are redacted in the artifact. `--check` compares an on-disk artifact to the current normalized export and reports `process_artifact_missing`, `process_artifact_parse_error`, or `process_artifact_drift` instead of silently accepting stale config.

`nopal import beislid-workflow` reads `beislid:<key>` fenced blocks from `.beislid/workflow.md` and drafts representable `.nopal/*.jsonc` modules for gates, policy, workflow lifecycle actions, and integrations.
Preview is the default; `--write` creates files but refuses to overwrite existing modules unless `--overwrite` is explicitly supplied.
`--check` is mutually exclusive with write flags and compares parsed JSONC values, so formatting and comments do not create drift while missing, invalid, or semantically stale generated modules fail closed.
Using `--check` declares importer ownership of `integrations.jsonc`, `gates.jsonc`, `policy.jsonc`, `workflow.jsonc`, `guidance.jsonc`, and `review_policy.jsonc` inside the output directory, so a checked-in managed file also drifts when its source block disappears.
Other `.nopal` files remain manually owned and are ignored by this check.
Unsupported blocks or fields remain visible as `beislid_import_unsupported` diagnostics in every mode instead of being silently dropped.

The importer is covered by sanitized fixtures rather than a checked-in private workflow.
Nopal acts as a deterministic config and export surface for Beislið-authored workflows while preserving Beislið standalone portability.
Beislið workflows can still run from `.beislid/workflow.md`; projects that opt into Nopal's config surface can import sanitized workflow blocks, validate generated modules, and hand consumers a drift-checkable `nopal.process_artifact/v1`.
See the `migration_bridge_proof` integration test fixtures for the current Nopal, Rondo, and Memento proof.

For vocabulary growth, the rule is: strings/data may expand, but the closed safety lattices above do not. That keeps old consumers conservative when they encounter a newer token they do not understand.

See `examples/` for valid trees per profile and deliberately broken trees (missing module, bad version, malformed JSONC, invalid gates/policy/workflow/integrations/guidance schema) with the diagnostics they produce.

## Policy (`nopal.policy/v1`)

`.nopal/policy.jsonc` declares, per run mode, which actions are allowed and how strongly they must be isolated.
Nopal decides and explains; enforcement belongs to the caller (Nopal's own coordinator, Rondo).

Decisions and placements are closed safety lattices; modes and action classes are open vocabulary with built-in members (additive tokens are data, and unknown classes degrade conservatively as protected/unsafe):

| Axis | Values |
|---|---|
| Modes (open; built-ins listed) | `manual`, `supervised_auto`, `unattended_auto`, `ci`; other tokens such as `nopal_tui` or `rondo_afk` are additive data |
| Action classes (open; built-ins listed) | `read`, `workspace_write`, `dependency_install`, `network_read`, `git_local`, `git_remote`, `destructive`, `secret_bearing`; unknown classes are treated as protected/unsafe |
| Decisions (closed lattice) | `allow` < `ask` < `deny` (most restrictive matched decision wins) |
| Placements (closed lattice) | `shared_user_runtime` < `dedicated_repo_runtime` < `dedicated_run_runtime` < `blocked` (strongest matched placement wins) |

Rules match any-of in v1: on the action id (`actions`) or on any intersecting class (`classes`).
Effective classes are the caller-declared classes plus the classes of referenced env refs; classifying `LINEAR_API_KEY` as `secret_bearing` in the policy `env` list makes every action that references it secret-bearing.
When no matched rule sets a decision or placement, the mode's `default_decision`/`default_placement` applies, then the built-in default for the mode (`manual` allows, automation modes ask, `ci` denies; interactive modes share the user runtime, unattended modes get a per-repo runtime, `ci` a per-run runtime).

Policy commands exit 0 when evaluation succeeded, 1 when `.nopal/policy.jsonc` is missing or invalid; the verdict is in the payload, never in the exit code.
Policy evaluation reads only the policy module, so a typo in another module can never flip a verdict.

## Gates (`nopal.gates/v1`)

`gates.jsonc` declares readiness checks; nopal selects and explains them, executors (Nopal's own coordinator, Rondo) run them.

- `preflights` and `gates` are separate flat lists; every entry has a stable unique `id`, a fixed `stage`, and exactly one of `command` / `argv`; gates may also declare an `autofix` command for executors that support repair flows.
- Preflight stages: `session_start`, `run_start`.
  Gate stages: `per_edit`, `pre_commit`, `pre_pr`, `post_pr`, `continuous`, `human_interrupt`.
- `gate_sets` group gates by id; ordered `selectors` match changed files with globs (`paths` / `exclude`, `*` stays within a path segment, `**` crosses) and reference sets.
- `nopal gates select` walks selectors in declaration order, dedups gates by first selection, and applies the stage filter last, so a wrong-stage pull is reported as `stage_mismatch` instead of vanishing.
  With no selectors configured, every stage-matching gate is default-selected.
- Commands may use flat brace placeholders from a fixed vocabulary (currently `{changed_files}`); anything else is a structured diagnostic.

## Workflow (`nopal.workflow/v1`)

`workflow.jsonc` models Beislið-compatible lifecycle/checkpoint side-effect declarations without executing them.
Its optional `establishment.events` allowlist names the explicit checkpoints that may establish a Provisional Plot.
`roots.jsonc` records durable Root declarations and their stage/gate Proof Requirements.
At an allowed checkpoint, `nopal plot establish --event <event> --workspace <path>` freezes the primary Repository's normalized Workflow, records Repository and Workspace snapshots, and binds the live Session to exactly one Workspace.
Exact replay is idempotent, while an attempted Session move fails with a structured conflict.

- Lifecycle events are fixed: `kickoff_start`, `break_spec_approved`, `spec_approved`, `blueprint_approved`, `kickoff_context_ready`, `implementation_plan_created`, `review_feedback_loaded`, and `ready_for_review_pre_submit`.
- Every action requires a stable `id` plus a supported `type` for the event. `kickoff_start` supports `cli`; planning approval events support `artifact`/`cli` and `spec_approved` also supports `tracker`; checkpoint events support `artifact`.
- Action `id`s must be unique within an event; duplicates are reported as `duplicate_id`.
- CLI actions require `command` and explicit `approval`; artifact paths, when present, must stay repo-local `.md` paths. `on_failure` is `prompt | continue | abort`.

## Integrations (`nopal.integrations/v1`)

`integrations.jsonc` models external provider surfaces for consumers while keeping Nopal cold.

- Tracker surfaces cover ticket source/update providers (`mcp`, `cli`, `file`, `paste` for sources; `mcp`, `cli` for updates) and required provider fields.
- PR review surfaces cover `cli`/`paste` sources and `cli`/`manual` updates.
- Pi handoff, model routing, visual surfaces (`lavish-axi`), workflow signals (`tmux-glance`), and probe cache settings are schema-checked but never invoked by Nopal.

## Guidance (`nopal.guidance/v1`)

`guidance.jsonc` is intentionally non-authoritative. It may carry skill, agent, model, and context hints for hosts, but validation rejects attempts to define gates, policy, workflow/lifecycle actions, progression, or proof requirements. Deterministic decisions must live in the dedicated Nopal modules, not guidance prose.

## Run ledger (`run-ledger-v1`)

`nopal ledger` is a port of Beislið's durable run ledger and shares its on-disk contract: both tools read and write the same trees, byte for byte (proven by a write-equivalence test against the vendored Python reference in `crates/nopal-cli/tests/reference/`).

State lives outside the repo at `${BEISLID_STATE_DIR:-~/.local/state/beislid}/runs/<flow>/<repo_hash>/<run_id>/` (`--state-dir` beats the env var); each run holds `run.json`, an append-only `events.jsonl`, a human-readable `transcript.md`, `checkpoints/`, and per-attempt gate envelopes under `artifacts/gates/<scope>/<gate>/<n>/`.
Writes are durable (temp file, fsync, rename, fsync parent) and serialized under an exclusive lock on a dedicated `.lock` file, so concurrent writers from any mix of the Python and Rust tools stay consistent.
Everything recorded passes secret redaction first (assignment/bearer/env-var patterns aligned with the action-policy secret vocabulary).

Ledger commands exit 0 on success, 1 on domain problems reported in the envelope (`run_id_invalid`, `run_id_collision`, `run_not_found`, `run_ambiguous`, `ledger_status_invalid`, `ledger_entry_invalid`), 2 on usage/IO failures.
Mutating commands emit `nopal.run_ledger.<command>/v1` envelopes that keep the Python tool's stdout field names as a compatible superset.

Conformance deltas from the Python tool: the ghost `active` status - read but never written by any command - is not modeled and surfaces as a `ledger_status_invalid` warning when found on disk; the legacy flat `runs/<repo_hash>` layout is not discovered; `--run-id` is validated on every command, not only `init` (traversal-shaped ids are rejected as `run_id_invalid` instead of being joined into the search path); a malformed non-string `gate.scope` in an envelope falls back to `repo` instead of crashing; and payload numbers echo their original text (Python-canonical literals match byte-for-byte and no precision is lost, but a hand-written non-canonical literal like `1E30` is preserved rather than re-normalized to `1e+30`).

## Launch (`nopal cli`, `nopal.launch/v1`, `nopal.bundle/v1`)

`nopal cli` runs Nopal Core's cold gates and, if they pass, `exec`s into a Pi session pinned to `.nopal/bundle.jsonc`.
`--dry-run`, `--with-ambient`, and `--verbose` are flags of `nopal cli`, not top-level flags - bare `nopal` itself opens the Field (see Usage above).
`nopal status` is the only way to get the plain readiness report.

`--dir` is the starting point for project discovery, not necessarily the project root itself.
Config resolves at the nearest directory with a `.nopal/` directory, walking up from `--dir` to the enclosing Git repository's top-level directory; outside a Git repository there is no walk and `--dir` itself is the root.
Every subcommand that operates on the project root uses this discovered root, so `nopal cli` (and `nopal validate`, `nopal status`, ...) work from anywhere inside the repo.

The first real (non-dry-run) `nopal cli` launch in an unconfigured repo - no `.nopal/` directory anywhere up to the git toplevel - silently writes `.nopal/nopal.jsonc` (profile `"minimal"`, always) and `.nopal/bundle.jsonc` at the git root, re-validates against the files it just wrote, and then launches; there is no prompt.
Scaffolding only ever creates a brand-new `.nopal/` directory: an existing `.nopal/` that is merely missing the manifest or bundle fails closed with the usual `manifest_missing`/`bundle_missing` diagnostics and is never written into.
`--dry-run` reports the pending scaffold as `scaffold: "would_create"` and never writes anything; the plan's `scaffold` field is `"none"` for configured repos and `"created"` after a launch that just scaffolded.
Pi's working directory stays at `--dir` as given - config, gates, and the bundle resolve at the discovered root, but the launched session starts where you stand.

The scaffolded bundle's content has two possible sources.
If `${NOPAL_CONFIG_DIR:-$HOME/.config/nopal}/bundle-default.jsonc` exists, it is copied into the new `.nopal/bundle.jsonc` verbatim - the exact bytes, comments included.
Template authors should anchor any pinned resource `path` with a bare `~`, an absolute path, or an `${ENV}` token: resolution still follows the normal bundle rules against the *new* repo's root, not the template file's own location, so a project-relative path in the template would resolve against a directory the template's author never saw.
Before anything is written, the template is validated through the same parse-and-shape checks `nopal.bundle/v1` always runs (`version` must be `"nopal.bundle/v1"`, `inherit_ambient`/resource-array shapes must be well-formed) - including resolution and existence of every pinned resource path against the new repo's root.
A template pinning a `~`-anchored path that does not exist on a given machine therefore blocks the scaffold of every fresh repo on that machine, by design: the failure surfaces at scaffold time with the template named, not later as a half-configured repo.
An invalid template fails the launch closed: nothing is written - not even the manifest half, which has nothing to do with the bundle - and there is no silent fallback to Nopal's own default; the diagnostics (`scaffold_template_invalid`, plus the underlying shape errors) name the template's real filesystem path, and `--dry-run` reports the same failure without writing anything either.
With no template file present, the scaffold falls back to Nopal's built-in hermetic default:

```jsonc
{
  // Created by nopal on first launch. Hermetic by default: no ambient pi
  // resources are inherited. Set "inherit_ambient": true (or a list like
  // ["skills"]) or pin resources explicitly to opt in. A user-wide default
  // for new repos can live at ~/.config/nopal/bundle-default.jsonc.
  "version": "nopal.bundle/v1",
  "inherit_ambient": false
}
```

A scaffolded repo with no template is fully hermetic on first launch: all four `--no-*` flags and zero pinned resources.
Silently inheriting the operator's whole ambient Pi state into an unfamiliar repository would be surprising, so the scaffolded file explains how to opt back in per repository or set a standing template for every new repository.

A real launch always prints two notices to stderr immediately before exec, and neither is gated by `--verbose`.
Unlike the `nopal.launch/v1` summary line below, an operator should never have to opt in just to see what a launch is about to hand Pi.
A launch that scaffolded prints `nopal: created .nopal/nopal.jsonc + .nopal/bundle.jsonc (from <template path>)` or `(built-in hermetic defaults)`, naming exactly which source was used.
Every real launch, scaffolded or not, also prints a one-line resource-surface summary, e.g. `nopal: 10 pinned resources; ambient: skills`, `nopal: hermetic launch - no ambient, no pinned resources`, or `nopal: full ambient inheritance; no pinned resources`.

Launch gates run in order, failing closed before any handoff:

1. **Validity** (`nopal_core::validate::validate`, narrowed by `required_scope_ok`): a bad manifest or a missing/schema-invalid *required* module blocks launch. A schema-invalid *optional*-but-present module does not - `Validation::ok()` alone can't tell the two apart, since it flags schema errors in any present module regardless of whether the active profile requires it.
2. **Readiness** (the same `Validation::ok()`, unnarrowed): `ready == false` from *optional* module gaps is a non-blocking warning, not a block.
3. **Process-artifact drift**, only when `.nopal/process-artifact.json` exists: a present-and-stale/invalid artifact blocks launch; a missing artifact (the normal case - it is never committed) is a non-blocking provenance note.
4. **Bundle resolution** (`nopal_core::bundle::bundle_report`): every declared resource in `.nopal/bundle.jsonc` must resolve to an existing path; a missing bundle, bad schema, or unresolvable resource blocks launch.

`.nopal/bundle.jsonc` (`nopal.bundle/v1`) declares up to four Pi resource kinds - `extensions`, `skills`, `prompt_templates`, `themes` - each entry `{ source, version?, path }`. `path` may be project-relative (resolved against the discovered project root - not `--dir` and not the process's current directory), or use a bare `~` (home-directory only - `~user` is not supported) or `${ENV}` tokens for absolute locations; version is recorded metadata only, never enforced.

Launch is hermetic by default: it passes Pi four `--no-*` flags (`--no-extensions --no-skills --no-prompt-templates --no-themes`) to disable ambient resource discovery, then loads each pinned resource through its own explicit-path flag (`-e`, `--skill`, `--prompt-template`, `--theme`) - pinned resources always load, even for a kind whose `--no-*` flag is present. `--no-context-files` is deliberately not set - `AGENTS.md`/`CLAUDE.md` are repo context, not ambient user state.

`inherit_ambient` controls this per kind. Set it to `true` (all four kinds) or `false`/omit it (none) for the original all-or-nothing behavior, or give it an array of the kind names that should inherit ambient state, e.g. `"inherit_ambient": ["skills", "themes"]` drops only `--no-skills`/`--no-themes` while still passing `--no-extensions --no-prompt-templates`. An unrecognized token in the array is a non-blocking `bundle_ambient_kind_unknown` warning (conservative: treated as not inherited); a value that is neither a boolean nor an array of kind strings is a `field_invalid` error. Passing `--with-ambient` always widens the effective set to all four - it unions with the bundle's own declaration and never narrows it.

`nopal cli --dry-run` runs the same four gates and prints the `nopal.launch/v1` plan (`ok`, `would_exec`, resolved bundle resources, the exact Pi argv, diagnostics) without touching Pi - this is both the operator debug surface and the hermetic test hook; automated tests only ever exercise `--dry-run`. The plan's `ambient` field stays a bool (`true` only when all four kinds are inherited) for backward compatibility; `ambient_kinds` lists exactly which kind field names are currently inherited. On a real launch, the equivalent one-line `nopal.launch/v1` stderr summary is opt-in via `--verbose` rather than printed on every session.

Exit codes: `0` launched or plan ok, `1` validity/process-artifact/bundle error (no session started), `2` usage/IO - including `pi` not found on `PATH` (honors a `NOPAL_PI_BIN` override) or a non-unix platform, which has no spawn fallback.

The handoff also sets `PI_SKIP_VERSION_CHECK=1` for the launched Pi process, since nopal already owns readiness/drift reporting - Pi's own update-check network call and banner would otherwise be redundant noise on every launch.

Curating the real v1 nopal resource bundle is a deferred follow-up; this repo's own `.nopal/bundle.jsonc` currently pins zero resources (valid, `ok: true`). See `examples/bundle-valid` and `examples/bundle-invalid` for a resolvable bundle and a deliberately broken one.

## Coordinator and AFK runs

Alongside the config/envelope surface above, `nopal` is also the product-facing coordinator over Nopal readiness, policy placement, and Rondo Core execution:

- `nopal status` reports readiness and missing modules through the coordinator's own envelope (bare invocation opens the Field; `nopal cli` launches instead - see Launch above).
- `nopal placement` explains the runtime placement Nopal would use for an action, reusing the same policy decision the config surface evaluates.
- `nopal rondo start|health|restart` manage a local, contract-backed Rondo Core service stub; a `blocked` placement (explicit or policy-derived) never starts or submits to it.
- `nopal run start` remains a dry-run-only preview.
- `nopal run submit --manifest <path> --plot-id <id> [--state-dir <path>]` submits one approved per-slice export after readiness, policy, placement, and repository path checks pass.
- `nopal run observe --repo-id <id> --plot-id <id> --run-id <id> [--cursor <cursor>] [--state-dir <path>]` fetches one bounded status and event page.

The coordinator's own config lives alongside the manifest at `.nopal/config.jsonc`.
The optional Rondo Core block is:

```jsonc
{
  "version": "nopal.config/v1",
  "rondo_core": {
    "base_url": "http://127.0.0.1:4400",
    "request_timeout_ms": 10000,
    "repo_id": "optional-stable-repository-id"
  }
}
```

Only literal loopback HTTP origins are accepted.
`NOPAL_RONDO_CORE_URL` overrides `base_url`, which is useful for isolated runs and ephemeral ports.
If `repo_id` is omitted, Nopal derives a stable opaque identifier from the canonical checkout path without sending that path over the wire.
Submission and observation fail closed when no effective endpoint exists.

Retry the same manifest normally after a timeout or uncertain response.
Rondo deduplicates the repository identifier plus exact manifest digest and returns the original run identity instead of starting the work twice.
An `ask`, `deny`, blocked placement, failed readiness check, unsafe manifest path, invalid configuration, or Core error produces an `ok: false` envelope and exit code 1 without an implicit approval.

Observation is one page per command.
Pass `next_event_cursor` back as `--cursor` while `has_more` is true.
Rondo cursors use the strict `rondo.core/v1:<decimal>` contract with at most 20 decimal digits; Nopal rejects malformed or oversized cursors before observation.
The run is settled only when its status is `completed`, `failed`, `terminated`, or `paused` and the returned page has caught up.
Stopping observation never cancels the durable run.

The Pi extension registers `nopal_afk_start` and `nopal_afk_result` without requiring an interactive UI.
`nopal_afk_start` accepts only `manifestPath`, invokes the pinned JSON submission command, and returns the complete `nopal.run_submit/v1` envelope so the caller can retain the opaque repository id, run id, and initial event cursor.
It does not approve an export, bypass Nopal policy, or retry a rejected submission.

`nopal_afk_result` accepts the opaque `repoId` and `runId` from the start envelope plus an optional prior `eventCursor`.
With `block: false` or when omitted, it performs one observation and returns `nopal.afk_result/v1` with outcome `observed` or `settled`.
With `block: true`, it drains immediately available pages, then polls locally until the run settles, the timeout expires, the host aborts, the cursor contract fails, or the per-call accumulation budget is reached.
The default timeout is 60 seconds, the default caught-up poll interval is 1 second, and one call accumulates at most 500 events or 2 MiB of serialized event data.
The result returns the resumable `next_event_cursor`, evidence pointers, accumulated events, poll count, and a precise outcome.
Timeout, abort, and accumulation-budget outcomes preserve the durable Rondo run and never request cancellation.
Cursor regressions, jumps, or stalls fail closed instead of silently skipping or replaying evidence.

Beislið owns approval and export semantics.
Nopal owns readiness, policy, safe submission, and operator rendering.
Rondo owns execution, capacity, workspaces, run ledgers, and evidence.
Nopal preserves Rondo-owned identifiers and evidence URIs without constructing ledger paths.

Run the harmless production-seam proof against a Rondo checkout with:

```sh
scripts/verify-rondo-core-bridge <path-to-rondo-checkout>
```

The runner starts an isolated ephemeral Core service, drives the registered Pi tools through the production Nopal binary, observes terminal evidence, and verifies idempotent replay.

`.nopal/rondo-core.{json,log}` remains state for the provisional local lifecycle stub and is not the durable AFK run record.

## Info (`nopal.info/v1`)

`nopal info` gives consumers a deterministic, machine-readable feature-detection report instead of parsing `--version` or `--help` text; the motivating incident was a stale installed binary and a fresh worktree build both reporting `nopal 0.1.0` while differing in whole subcommand families. The envelope carries `version` (the crate version), `commit` (set at build time via the `NOPAL_BUILD_COMMIT` env var, `null`/`-` when unset - never a git probe, so builds stay hermetic), and `capabilities`, the sorted list of top-level subcommand families derived at runtime from the real clap CLI surface so it cannot drift out of sync. The intended consumer contract is a membership check against `capabilities` (for example Beislið doctor's `binary` probe checking for `"field"`), never text-parsing help or version output. `nopal info` works outside any project - it needs no `.nopal/` module and never touches one - and is a cold command like `validate`, `gates`, and `preflights`.

## Field state query (`nopal.field/v1`)

`nopal.field/v1` includes an additive `plots` array containing Core-owned `nopal.plot/v1` documents and their explicit Session bindings.
Existing consumers remain compatible because the run `entries` and ask surfaces are unchanged.
The interactive Field uses these Plot facts for primary navigation while showing interactive Sessions and unattended executions as siblings in the dominant panel.
A selected live Session retains the embedded terminal transport, while a selected execution is a read-only durable Core view that cannot receive terminal input.

`nopal field inspect` is the domain and Core projection seam the Field consumes for Plot, run, and ask facts.
It renders and routes, never decides: domain facts come from the Plot store, run ledger, ask store, or optional Rondo feed.
Generated diagnostics and logical ask-expiry views are derived read-only from those facts and the observation clock, never persisted, and the query invents no independent domain state.
tmux remains the live-Session transport, providing seat inventory and embedded VT output rather than domain facts.

Unlike `nopal ledger`/`nopal ask`, which scope to one repo hash, `nopal field inspect` walks every flow and repo hash in the run state root.
An explicit `--state-dir` unifies Plot, run, and ask state under that root.
Without it, Plots resolve through `NOPAL_STATE_DIR` or `~/.local/state/nopal`, while the compatibility run and ask stores resolve through `BEISLID_STATE_DIR` or `~/.local/state/beislid`.
Per live run it returns:

- **placement** - the recorded worktree facts `{ repo, repo_hash, branch, run_dir, flow }` from `run.json`; it does *not* re-run `nopal.policy/v1` (computing a fresh decision would be inventing semantics).
- **ledger state** - status, ticket, skill, timestamps, and the latest attempt per gate (name/scope/status/classification) from the run ledger.
- **pending asks** - the `nopal.ask/v1` asks backed by that run; asks with no backing run land in top-level `asks_unbound`.
- **rondo status/events** - attached from an optional `rondo.core/v1` run-events feed supplied via `--rondo-events <feed>`, matched by run id.

Default is the live Field (incomplete runs, pending asks); `--all` includes completed runs and every ask state.

The Rondo adapter is optional and degradable: no feed yields a `field_rondo_feed_absent` info diagnostic with every entry's `rondo` null; an unreadable feed yields `field_rondo_feed_unreadable` and still renders the rest; a parsed feed whose run ids do not match any ledger run yields `field_rondo_unmatched` (Nopal-to-Rondo run-id bridging is future work).
The Field is partial by construction because only runs written through the Nopal ledger are visible, and it says so with a standing `field_partial_coverage` info diagnostic rather than guessing.
Established Plots additionally project their frozen Workflow provenance, Repository Roots and Proof Requirements, bound Workspaces, unattended executions, Evidence, and explicit Fruit state through `nopal.field/v1`.
The Field keeps Overview, Roots, Evidence, and Fruit independently inspectable and never infers Fruit, Progress, Conditions, or Root satisfaction from execution completion.

Liveness is v1 poll: each invocation is a fresh point-in-time scan (plus a feed replay from cursor zero); the Field re-invokes `nopal field inspect --json` on its own cadence to refresh. The scan is read-only and never materializes ask expiry, so an overdue pending ask reads as expired logically from the observation clock without a cross-repo write.

## Herdr sidebar bridge

`nopal bridge herdr` is a headless client of `nopal.field/v1` that reports matching live run, gate, and pending-ask state through herdr's newline-delimited local socket protocol.
It correlates herdr panes by `foreground_cwd`, then pane `cwd`, against the recorded run placement path; it never inspects Nopal's stores directly.
Pending asks report herdr's semantic `blocked` state, active runs report `working`, and gate/run details stay in the compact visual status.
Approve/deny actions, seat management, process input, and herdr UI ownership are deliberately outside this bridge.

Socket resolution is `--socket`, then `HERDR_SOCKET_PATH`, then `$XDG_CONFIG_HOME/herdr/herdr.sock`, then `$HOME/.config/herdr/herdr.sock`.
The daemon polls every 5 seconds by default; `--interval` overrides that conservative cadence.
Feed state resolution matches `nopal field inspect`.
An explicit `--state-dir` unifies the stores; otherwise Plot state follows `NOPAL_STATE_DIR` and run or ask state follows `BEISLID_STATE_DIR`, each with its XDG default.
Every poll checks the server's `ping` version/protocol metadata, ignores additive response fields, reads a session snapshot with `pane.list` fallback for older servers, and releases `custom:nopal` authority from panes that no longer match a live Nopal run.
A missing socket degrades silently and retries in daemon mode; `--once` treats it as a successful no-op.
Malformed Nopal feed or herdr protocol data is an observable error rather than an invented state.

## Diagnostics

Diagnostic codes are a stable contract: `manifest_missing`, `manifest_parse_error`, `version_unsupported`, `profile_unknown`, `module_missing`, `module_parse_error`; from the gates module `duplicate_id`, `stage_unknown`, `command_missing`, `command_conflict`, `command_invalid`, `placeholder_invalid`, `placeholder_unknown`, `gate_ref_unknown`, `gate_set_unknown`, `field_invalid`; from workflow/integrations/guidance `workflow_event_unknown`, `workflow_action_type_unknown`, `integration_provider_invalid`, `guidance_authority_invalid`; from the policy module `policy_shape_invalid`, `policy_mode_unknown`, `policy_rule_invalid`, `policy_rule_duplicate_id`, `policy_decision_invalid`, `policy_placement_invalid`, `policy_class_unknown`, `policy_env_invalid`, and the warning `policy_key_unknown`; from process artifact checks `process_artifact_missing`, `process_artifact_parse_error`, `process_artifact_drift`, `process_artifact_redacted`; from the bundle module `bundle_missing`, `bundle_parse_error`, `bundle_resource_missing`, and the warning `bundle_ambient_kind_unknown`; from scaffold the info code `scaffold_defaults` and the error `scaffold_template_invalid`; from Beislið imports `beislid_import_parse_error`, `beislid_import_unsupported`, `beislid_import_overwrite_blocked`; from the run ledger `run_id_invalid`, `run_id_collision`, `run_not_found`, `run_ambiguous`, `ledger_status_invalid`, `ledger_entry_invalid`; from Plot Establishment `plot_not_found`, `plot_snapshot_invalid`, `plot_establishment_event_invalid`, `plot_establishment_conflict`, `plot_session_workspace_conflict`; and from the Field query the info/warning codes `field_rondo_feed_absent`, `field_rondo_feed_unreadable`, `field_rondo_unmatched`, `field_partial_coverage`.
Import check failures use `beislid_import_missing`, `beislid_import_check_parse_error`, and `beislid_import_drift`.
Match on codes, never on message text.

The JSONC dialect is strict where silence would mislead: comments and trailing commas are allowed; missing commas between properties/elements and unquoted property names are parse errors, even though the underlying parser would tolerate them.

## Contracts and product surface

Only Rondo and Memento are genuinely separate products from Nopal, so `contracts/` holds exactly two inter-product contracts:

- **execution** (`contracts/execution.md`, formerly C2): the Rondo Core service API.
- **memory** (`contracts/memory.md`, formerly C4): the Memento MemoryProvider.

Everything above (the config/envelope surface and the Beislið process/proof-artifact surface, formerly C1/C3) is Nopal's own versioned product surface, documented under `docs/surface/` and held to the same conformance discipline at `conformance/surface/` - but evolvable: only the closed safety lattices listed above are frozen ABI, and vocabularies stay open. See `contracts/README.md` for the full catalog and versioning rules.

## Development

### Releases

Every non-release merge to `main` runs `.github/workflows/version-bump.yml`, which prepares the next workspace patch version on an automation branch and opens or refreshes a protected pull request.
The workflow explicitly dispatches CI for that branch, requests squash auto-merge, and creates the annotated `vX.Y.Z` tag only after the protected version pull request lands.
GitHub does not start a new workflow for a tag pushed by another workflow's `GITHUB_TOKEN`, so version-bump calls `.github/workflows/release.yml` as a reusable workflow after pushing the tag.
The release workflow also listens for independently pushed `v*` tags.

For each tag, the workflow builds with `NOPAL_BUILD_COMMIT` set to the tagged commit, checks out the Rondo source commit pinned in `rondo-runtime.json`, builds its escript with Erlang/OTP 28, packages both executables and provenance in the three platform archives listed above, generates one `SHA256SUMS` file, and creates the GitHub Release with generated notes.
The workflow verifies that the tag matches the workspace version before packaging.
Publication creates a draft when needed, uploads missing assets, compares already-published assets byte for byte, and publishes only after every asset converges.
An identical rerun is a no-op, while a same-name asset with different content fails closed.

Every third-party action on the release path is pinned to a reviewed full commit SHA with its human-readable major version in a comment.
Dependabot checks GitHub Actions monthly through `.github/dependabot.yml`; action-update pull requests must verify the upstream release ref before accepting the new SHA and updating its version comment.

Release publication is intentionally separate from the deferred Homebrew tap.
Adding or updating a Homebrew formula is not automated yet.

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```
