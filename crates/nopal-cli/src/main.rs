//! `nopal` - deterministic enforcement and distribution CLI over nopal-core.
//!
//! Exit codes: 0 = success, 1 = a valid blocking domain outcome, and 2 =
//! malformed authority, infrastructure, or effect failure. Policy verdicts
//! live in versioned payloads rather than being encoded as exit statuses.
//!
//! Inspection commands never contact agents or external services. Commands
//! that consume a project root resolve it through `discover::project_root`,
//! which probes Git once to find the enclosing repository. Bare invocation is
//! deliberately warm: it validates launch and enforcement contracts,
//! initializes Workflow Run Ledger evidence, and replaces itself with Pi.
//! Removed command shapes stop at migration diagnostics and never dispatch a
//! compatibility runtime.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use nopal_core::beislid_import::{self, ImportOptions};
use nopal_core::discover;
use nopal_core::enforcement;
use nopal_core::process_artifact;
use nopal_core::run_ledger as ledger_core;
use nopal_core::run_ledger_report as ledger;
use nopal_core::run_ledger_store::InitArgs;
use nopal_core::scaffold;
use nopal_core::{gates::GateStage, policy};
use sha2::{Digest as _, Sha256};

mod distribution_adapter;
mod doctor;
mod enforcement_adapter;
mod gate_executor;
mod info;
mod launch;
mod verification;

#[derive(Parser)]
#[command(
    name = "nopal",
    version,
    about = "Opinionated Pi distribution with deterministic enforcement"
)]
struct Cli {
    /// Starting directory for project discovery (walks up to the git root
    /// to find `.nopal/`)
    #[arg(long, global = true, default_value = ".")]
    dir: PathBuf,

    /// Emit machine-readable JSON instead of TOON
    #[arg(long, global = true)]
    json: bool,

    /// Print the launch plan without scaffolding or starting Pi
    #[arg(long)]
    dry_run: bool,

    /// Layer the pinned bundle on top of ambient Pi resources
    #[arg(long)]
    with_ambient: bool,

    /// Print the launch summary before starting Pi
    #[arg(long)]
    verbose: bool,

    /// Arguments passed unchanged to Pi after `--`
    #[arg(last = true)]
    pi_args: Vec<String>,

    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Removed internal spelling retained only as a migration diagnostic
    #[command(hide = true)]
    Cli(RemovedCommandArgs),
    /// Materialize and verify the exact checked-in distribution lock
    Sync,
    /// Resolve the distribution contract into an exact lock proposal
    Update {
        /// Atomically replace .nopal/nopal.lock with the proposal
        #[arg(long)]
        write: bool,
    },
    /// Explain evidence-backed first-run gate detection without writing files
    Doctor,
    /// Validate the nopal.project/v1 manifest and profile-required modules
    Validate,
    /// Verify the local pre-PR boundary without launching Pi or performing a push
    Verify {
        /// Verification ledger state root; beats BEISLID_STATE_DIR and the XDG default
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Inspect nopal.gates/v1 or generated v2 preflights
    Preflights {
        #[command(subcommand)]
        command: PreflightsCmd,
    },
    /// Inspect and select nopal.gates/v1 or generated v2 gates
    Gates {
        #[command(subcommand)]
        command: GatesCmd,
    },
    /// Evaluate nopal.policy/v1 action decisions and runtime placement
    Policy {
        #[command(subcommand)]
        command: PolicyCmd,
    },
    /// Machine API used by the bundled Pi enforcement adapter
    #[command(hide = true)]
    Enforcement {
        #[command(subcommand)]
        command: Box<EnforcementCmd>,
    },
    /// Export normalized process artifacts
    Export {
        #[command(subcommand)]
        command: ExportCmd,
    },
    /// Import legacy Beislið process artifacts into draft Nopal modules
    Import {
        #[command(subcommand)]
        command: ImportCmd,
    },
    /// Durable run ledger (run-ledger-v1), interoperable with beislid's
    Ledger {
        /// Ledger state root; beats BEISLID_STATE_DIR and the XDG default
        #[arg(long)]
        state_dir: Option<PathBuf>,

        #[command(subcommand)]
        command: LedgerCmd,
    },
    /// Removed cross-session approval surface retained only as a migration diagnostic
    #[command(hide = true)]
    Ask(RemovedCommandArgs),
    /// Removed Plot surface retained only as a migration diagnostic
    #[command(hide = true)]
    Plot(RemovedCommandArgs),
    /// Removed host bridge retained only as a migration diagnostic
    #[command(hide = true)]
    Bridge(RemovedCommandArgs),
    /// Review-risk seam: risk class, fast-path eligibility, and split verdict
    /// from changed files/stats/thresholds (nopal.review_risk/v1)
    ReviewRisk(ReviewRiskArgs),
    /// Removed v0.2 product surface retained only as a migration diagnostic
    #[command(hide = true)]
    Field(RemovedCommandArgs),
    /// Show Nopal readiness and missing modules through the Nopal product surface
    Status,
    /// Machine-readable version + capability report (nopal.info/v1)
    Info,
    /// Removed placement alias retained only as a migration diagnostic
    #[command(hide = true)]
    Placement(RemovedCommandArgs),
    /// Removed Rondo service retained only as a migration diagnostic
    #[command(hide = true)]
    Rondo(RemovedCommandArgs),
    /// Removed run coordinator retained only as a migration diagnostic
    #[command(hide = true)]
    Run(RemovedCommandArgs),
    /// Removed workflow-report surface retained only as a migration diagnostic
    #[command(hide = true)]
    Workflow(RemovedCommandArgs),
}

#[derive(clap::Args)]
#[command(trailing_var_arg = true)]
struct RemovedCommandArgs {
    /// Legacy arguments are accepted only so the removed route can explain migration.
    #[arg(allow_hyphen_values = true)]
    _legacy_args: Vec<OsString>,
}

#[derive(Debug, serde::Serialize)]
struct MigrationReport {
    kind: &'static str,
    ok: bool,
    code: &'static str,
    surface: &'static str,
    removed_in: &'static str,
    migration: &'static str,
}

#[derive(Subcommand)]
enum ExportCmd {
    /// Build or check the normalized nopal.process_artifact/v1 JSON artifact
    Process {
        /// Write artifact JSON to this path; defaults to .nopal/process-artifact.json
        #[arg(long)]
        output: Option<PathBuf>,
        /// Print artifact JSON to stdout instead of writing a report
        #[arg(long, conflicts_with_all = ["output", "check"])]
        stdout: bool,
        /// Compare the output path to the current normalized artifact
        #[arg(long)]
        check: bool,
    },
}

#[derive(Subcommand)]
enum ImportCmd {
    /// Draft .nopal/*.jsonc modules from .beislid/workflow.md fenced blocks
    BeislidWorkflow {
        /// Source workflow markdown path, relative to the discovered
        /// project root unless absolute
        #[arg(long, default_value = ".beislid/workflow.md")]
        source: PathBuf,
        /// Output directory for module drafts; defaults to .nopal
        #[arg(long, default_value = ".nopal")]
        output_dir: PathBuf,
        /// Write module drafts to disk; default is preview only
        #[arg(long)]
        write: bool,
        /// Explicitly replace existing files when used with --write
        #[arg(long)]
        overwrite: bool,
        /// Compare generated module semantics with checked-in JSONC modules
        #[arg(long, conflicts_with_all = ["write", "overwrite"])]
        check: bool,
    },
}

#[derive(Subcommand)]
enum LedgerCmd {
    /// Create a run directory and its run.json entry
    Init {
        /// Skill recording the run
        #[arg(long)]
        skill: String,
        /// Ledger flow name; defaults to --skill
        #[arg(long)]
        flow: Option<String>,
        #[arg(long, default_value = "none")]
        ticket_id: String,
        #[arg(long, default_value = "none")]
        ticket_title: String,
        #[arg(long, default_value = "")]
        ticket_url: String,
        /// Branch to record; defaults to the current git branch
        #[arg(long)]
        branch: Option<String>,
        /// Explicit run id (single path-safe segment); collisions error
        #[arg(long)]
        run_id: Option<String>,
    },
    /// Append an event to events.jsonl and the transcript
    Event {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        flow: Option<String>,
        /// Event type recorded in the ledger
        #[arg(long = "type")]
        event_type: String,
        /// JSON file with the event payload
        #[arg(long)]
        json_file: Option<PathBuf>,
        /// Transcript summary; defaults to the redacted payload
        #[arg(long)]
        summary: Option<String>,
        /// Stable caller identity for retrying an uncertain commit
        #[arg(long)]
        operation_id: Option<String>,
    },
    /// Write a named checkpoint and fold it into run.json
    Checkpoint {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        flow: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        json_file: Option<PathBuf>,
        #[arg(long)]
        resume_hint: Option<String>,
        /// Stable caller identity for retrying an uncertain commit
        #[arg(long)]
        operation_id: Option<String>,
    },
    /// Record a gate attempt envelope with checkpoint and event
    Gate {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        flow: Option<String>,
        #[arg(long)]
        name: String,
        #[arg(long)]
        scope: Option<String>,
        /// JSON file with the gate result envelope
        #[arg(long)]
        envelope_file: PathBuf,
        #[arg(long)]
        resume_hint: Option<String>,
        /// Stable caller identity for retrying an uncertain commit
        #[arg(long)]
        operation_id: Option<String>,
    },
    /// Mark the run interrupted with a reason
    Interrupt {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        flow: Option<String>,
        #[arg(long)]
        reason: String,
        #[arg(long)]
        resume_hint: Option<String>,
        /// Stable caller identity for retrying an uncertain commit
        #[arg(long)]
        operation_id: Option<String>,
    },
    /// Resume one exact interrupted run and require fresh verification
    Continue {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        flow: Option<String>,
        /// Stable caller identity for retrying an uncertain commit
        #[arg(long)]
        operation_id: Option<String>,
    },
    /// Set the final status (interrupted, failed, or completed)
    Finalize {
        #[arg(long)]
        run_id: String,
        #[arg(long)]
        flow: Option<String>,
        #[arg(long)]
        status: String,
        /// Markdown report copied to final-report.md
        #[arg(long)]
        report_file: Option<PathBuf>,
        /// Stable caller identity for retrying an uncertain commit
        #[arg(long)]
        operation_id: Option<String>,
    },
    /// Show the most recently updated matching run
    Resume {
        /// Exact run identity. Omitting it retains the legacy latest-match query.
        #[arg(long, conflicts_with_all = ["ticket_id", "branch", "include_completed"])]
        run_id: Option<String>,
        #[arg(long)]
        flow: Option<String>,
        #[arg(long)]
        ticket_id: Option<String>,
        #[arg(long)]
        branch: Option<String>,
        /// Include completed runs
        #[arg(long)]
        include_completed: bool,
    },
    /// Summarize runs with their latest gate attempts
    Dashboard {
        /// Filter by flow name
        #[arg(long)]
        flow: Option<String>,
        /// Include completed runs
        #[arg(long)]
        all: bool,
        /// Max gates per run
        #[arg(long, default_value_t = 5)]
        limit: usize,
    },
    /// Show the checkpoint pointer file (.nopal/, falling back to .beislid/)
    Pointer,
    /// List (or finalize) stale, unfinalized runs across the whole state dir
    Prune {
        /// Hours an incomplete, unfinalized run's updated_at may age before
        /// it is selected
        #[arg(long, default_value_t = ledger::DEFAULT_STALE_AFTER_HOURS)]
        stale_after: u64,

        /// Finalize each selected run as `interrupted` instead of only
        /// listing it (default: dry run)
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum PreflightsCmd {
    /// List declared preflights with their stages and commands
    List,
}

#[derive(Subcommand)]
enum GatesCmd {
    /// List declared gates, gate sets, and selectors
    List,
    /// Deterministically select gates for a stage and set of changed files
    Select {
        /// Gate stage to select for
        #[arg(long, value_parser = parse_stage)]
        stage: GateStage,
        /// Changed files, comma-separated and/or repeated
        #[arg(long = "changed-files", value_delimiter = ',')]
        changed_files: Vec<String>,
    },
}

#[derive(clap::Args)]
struct ReviewRiskArgs {
    /// Changed files, comma-separated and/or repeated
    #[arg(long = "changed-files", value_delimiter = ',')]
    changed_files: Vec<String>,

    /// Total added+deleted lines; caller sums additions+deletions (arithmetic,
    /// not a decision) and omits this when stats are unknown
    #[arg(long)]
    total_changes: Option<u64>,

    /// The PR base is up to date with its target (external fact nopal cannot derive)
    #[arg(long)]
    base_fresh: bool,

    /// The branch needs a merge/rebase before it can land
    #[arg(long)]
    needs_merge: bool,

    /// A PR already exists for this change
    #[arg(long)]
    existing_pr: bool,

    /// Gate stage to select for when checking multi-scope parallel-safety
    #[arg(long, value_parser = parse_stage, default_value = "pre_pr")]
    stage: GateStage,
}

#[derive(Subcommand)]
enum PolicyCmd {
    /// Action decision: matched rules and the winning allow/ask/deny verdict
    Evaluate(PolicyArgs),
    /// Runtime isolation placement: the strongest matched placement wins
    Placement(PolicyArgs),
    /// Combined decision and placement verdicts
    Decide(PolicyArgs),
}

#[derive(Subcommand)]
enum EnforcementCmd {
    /// Prepare the complete run-private executor manifest for an exact testable run
    PrepareRuntime(EnforcementArgs),
    /// Drive one exact action through planning and evidence-only verification
    VerifyEvidence(EnforcementArgs),
    /// Drive one exact action through planning, gates, and one-shot release
    Advance(EnforcementArgs),
    /// Decide an action and return every missing or stale required gate
    Plan(EnforcementArgs),
    /// Record one gate command executed by the trusted Pi adapter
    RecordGate(RecordGateArgs),
    /// Record the human response to one exact policy ask
    RecordApproval(RecordApprovalArgs),
    /// Consume one exact current authorization before releasing the Pi tool
    Authorize(AuthorizeArgs),
    /// Remove the run-private short gate temporary directory at session end
    CleanupRuntime(EnforcementArgs),
    /// Record the matching released tool's success, error, or interruption
    RecordOutcome(RecordOutcomeArgs),
}

#[derive(clap::Args)]
struct EnforcementArgs {
    #[arg(long, value_parser = parse_mode)]
    mode: policy::Mode,
    #[arg(long, value_parser = parse_action)]
    action: String,
    #[arg(long = "class", value_parser = parse_class)]
    classes: Vec<policy::ActionClass>,
    #[arg(long)]
    run_id: String,
    #[arg(long, default_value = "legacy")]
    launch_id: String,
    #[arg(long, default_value = "legacy")]
    session_id: String,
    #[arg(long, default_value = "legacy")]
    tool_call_id: String,
    #[arg(long, default_value = "legacy")]
    tool_name: String,
    #[arg(long, default_value = "legacy")]
    input_digest: String,
    #[arg(long, default_value = "legacy-executor")]
    executor_digest: String,
    #[arg(long, default_value = "legacy-runtime")]
    runtime_digest: String,
    #[arg(long, default_value = "legacy")]
    target_digest: String,
    #[arg(long = "changed-file")]
    changed_files: Vec<String>,
    #[arg(long)]
    mutates: bool,
    #[arg(long, default_value = "enforcement")]
    flow: String,
    #[arg(long)]
    state_dir: Option<PathBuf>,
}

#[derive(clap::Args)]
struct RecordGateArgs {
    #[command(flatten)]
    enforcement: EnforcementArgs,
    #[arg(long)]
    gate_id: String,
    #[arg(long)]
    exit_code: i32,
    #[arg(long)]
    contract_digest: String,
    #[arg(long)]
    workspace_fingerprint: String,
    #[arg(long)]
    gate_definition_digest: String,
    #[arg(long)]
    authorization_binding: String,
}

#[derive(clap::Args)]
struct RecordApprovalArgs {
    #[command(flatten)]
    enforcement: EnforcementArgs,
    #[arg(long)]
    authorization_binding: String,
    #[arg(long)]
    approved: bool,
    #[arg(long, default_value = "interactive_user")]
    by: String,
}

#[derive(clap::Args)]
struct AuthorizeArgs {
    #[command(flatten)]
    enforcement: EnforcementArgs,
    #[arg(long)]
    authorization_binding: String,
}

#[derive(clap::Args)]
struct RecordOutcomeArgs {
    #[command(flatten)]
    enforcement: EnforcementArgs,
    #[arg(long)]
    authorization_binding: String,
    #[arg(long)]
    release_id: String,
    #[arg(long, value_parser = parse_tool_outcome)]
    outcome: enforcement::ToolOutcome,
}

#[derive(clap::Args)]
struct PolicyArgs {
    /// Run mode
    #[arg(long, value_parser = parse_mode)]
    mode: policy::Mode,

    /// Stable action identity, e.g. git.push or dependency.install
    #[arg(long, value_parser = parse_action)]
    action: String,

    /// Declared action class (repeatable); rule matching is any-of
    #[arg(long = "class", value_name = "CLASS", value_parser = parse_class)]
    classes: Vec<policy::ActionClass>,

    /// Env var the action references (repeatable); classified via policy env refs
    #[arg(long = "env", value_name = "NAME")]
    env: Vec<String>,
}

fn parse_stage(text: &str) -> Result<GateStage, String> {
    Ok(GateStage::parse(text))
}

fn read_adapter_proof() -> std::io::Result<String> {
    use std::io::Read;

    const MAX_PROOF_BYTES: u64 = 4096;
    let mut bytes = Vec::new();
    std::io::stdin()
        .lock()
        .take(MAX_PROOF_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PROOF_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "adapter proof exceeds the bounded private protocol",
        ));
    }
    let proof: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("enforcement machine API requires a private adapter proof: {error}"),
        )
    })?;
    if proof["kind"] != "nopal.enforcement.adapter_proof/v1" {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "enforcement machine API received an unknown adapter proof",
        ));
    }
    proof["capability"]
        .as_str()
        .filter(|capability| !capability.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "enforcement machine API requires the active launch-scoped adapter capability",
            )
        })
}

#[cfg(unix)]
fn read_inherited_capability() -> std::io::Result<String> {
    use std::io::Read;
    use std::os::unix::io::FromRawFd;

    let fd = std::env::var("NOPAL_ENFORCEMENT_CAPABILITY_FD")
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "enforcement machine API requires an inherited adapter capability channel",
            )
        })?
        .parse::<i32>()
        .map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "enforcement capability descriptor is malformed",
            )
        })?;
    if !(3..=1024).contains(&fd) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "enforcement capability descriptor is outside the allowed range",
        ));
    }
    // The private CLI child owns a fresh one-shot pipe descriptor, while Pi
    // bootstrap owns and closes the original launch descriptor.
    let mut file = unsafe { std::fs::File::from_raw_fd(fd) };
    let mut bytes = [0_u8; 64];
    file.read_exact(&mut bytes)?;
    let capability = String::from_utf8(bytes.to_vec())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if !capability.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "inherited enforcement capability is malformed",
        ));
    }
    Ok(capability)
}

#[cfg(not(unix))]
fn read_inherited_capability() -> std::io::Result<String> {
    Err(std::io::Error::other(
        "inherited enforcement capability channels require unix",
    ))
}

fn parse_mode(s: &str) -> Result<policy::Mode, String> {
    policy::Mode::parse(s).ok_or_else(|| {
        format!(
            "unknown mode {s:?}; expected one of {}",
            policy::known_modes()
        )
    })
}

fn parse_class(s: &str) -> Result<policy::ActionClass, String> {
    policy::ActionClass::parse(s).ok_or_else(|| {
        format!(
            "unknown class {s:?}; expected one of {}",
            policy::known_classes()
        )
    })
}

fn parse_action(s: &str) -> Result<String, String> {
    if s.is_empty() {
        return Err("action must be a non-empty stable id, e.g. git.push".to_owned());
    }
    Ok(s.to_owned())
}

fn parse_tool_outcome(s: &str) -> Result<enforcement::ToolOutcome, String> {
    enforcement::ToolOutcome::parse(s).ok_or_else(|| {
        "unknown tool outcome; expected success, error, cancelled, or interrupted".to_owned()
    })
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("nopal: {err}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> std::io::Result<ExitCode> {
    // Resolved once, centrally, and lazily: every subcommand below that
    // means "project root" reads from `root`, never the raw `cli.dir`.
    // Discovery spawns a `git rev-parse` subprocess, so migration and info
    // routes that never touch the root must not pay for or expose that probe.
    // `exec_pi` is the one deliberate exception to root-consumption - see
    // its doc comment.
    let root = std::cell::LazyCell::new(|| discover::project_root(&cli.dir));
    match &cli.command {
        None => dispatch_launch(cli, &root, cli.dry_run, cli.with_ambient, cli.verbose),
        Some(Cmd::Cli(_)) => removed_surface(cli, "cli"),
        Some(Cmd::Sync) => run_distribution_sync(cli, &root),
        Some(Cmd::Update { write }) => run_distribution_update(cli, &root, *write),
        Some(Cmd::Doctor) => {
            let report = doctor::inspect(&root)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || doctor::to_toon(&report),
            )
        }
        Some(Cmd::Validate) => {
            let report = nopal_core::status::validation_report(&root)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || nopal_core::status::validation_toon(&report),
            )
        }
        Some(Cmd::Verify { state_dir }) => run_headless_verify(cli, &root, state_dir.as_deref()),
        Some(Cmd::Preflights {
            command: PreflightsCmd::List,
        }) => {
            let report = nopal_core::gates_report::preflights_list(&root)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || nopal_core::gates_report::preflights_list_toon(&report),
            )
        }
        Some(Cmd::Gates {
            command: GatesCmd::List,
        }) => {
            let report = nopal_core::gates_report::gates_list(&root)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || nopal_core::gates_report::gates_list_toon(&report),
            )
        }
        Some(Cmd::Gates {
            command:
                GatesCmd::Select {
                    stage,
                    changed_files,
                },
        }) => {
            let report =
                nopal_core::gates_report::gates_select(&root, stage.clone(), changed_files)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || nopal_core::gates_report::gates_select_toon(&report),
            )
        }
        Some(Cmd::Export { command }) => run_export_cmd(cli, &root, command),
        Some(Cmd::Import { command }) => run_import_cmd(cli, &root, command),
        Some(Cmd::Ledger { state_dir, command }) => {
            run_ledger_cmd(cli, &root, state_dir.as_deref(), command)
        }
        Some(Cmd::Ask(_)) => removed_surface(cli, "ask"),
        Some(Cmd::Plot(_)) => removed_surface(cli, "plot"),
        Some(Cmd::Field(_)) => removed_surface(cli, "field"),
        Some(Cmd::Bridge(_)) => removed_surface(cli, "bridge"),
        Some(Cmd::ReviewRisk(args)) => {
            let req = nopal_core::review_policy::ReviewRiskRequest {
                changed_files: &args.changed_files,
                total_changes: args.total_changes,
                facts: nopal_core::review_policy::FastPathFacts {
                    base_fresh: args.base_fresh,
                    needs_merge: args.needs_merge,
                    existing_pr: args.existing_pr,
                },
                stage: args.stage.clone(),
            };
            let report = nopal_core::review_policy::run(&root, &req)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || nopal_core::review_policy::review_risk_toon(&report),
            )
        }
        Some(Cmd::Status) => {
            let report = nopal_core::status::status(&root)?;
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", nopal_core::status::status_toon(&report));
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Cmd::Info) => {
            use clap::CommandFactory;
            let report = info::info_report(&Cli::command());
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || info::info_toon(&report),
            )
        }
        Some(Cmd::Placement(_)) => removed_surface(cli, "placement"),
        Some(Cmd::Rondo(_)) => removed_surface(cli, "rondo"),
        Some(Cmd::Run(_)) => removed_surface(cli, "run"),
        Some(Cmd::Workflow(_)) => removed_surface(cli, "workflow"),
        Some(Cmd::Enforcement { command }) => {
            let args = match command.as_ref() {
                EnforcementCmd::PrepareRuntime(args) => args,
                EnforcementCmd::VerifyEvidence(args) => args,
                EnforcementCmd::Advance(args) => args,
                EnforcementCmd::Plan(args) => args,
                EnforcementCmd::RecordGate(args) => &args.enforcement,
                EnforcementCmd::RecordApproval(args) => &args.enforcement,
                EnforcementCmd::Authorize(args) => &args.enforcement,
                EnforcementCmd::CleanupRuntime(args) => args,
                EnforcementCmd::RecordOutcome(args) => &args.enforcement,
            };
            let ledger_env =
                nopal_core::run_ledger_store::LedgerEnv::discover(&root, args.state_dir.as_deref());
            let run_dir = nopal_core::run_ledger_store::find_run_dir(
                &ledger_env,
                &args.run_id,
                Some(&args.flow),
            )
            .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
            let config_dir = resolve_config_dir();
            let receipt_key = read_inherited_capability()?;
            let adapter_proof = read_adapter_proof()?;
            if !enforcement::capability_matches(receipt_key.as_bytes(), adapter_proof.as_bytes()) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "enforcement machine API requires the active launch-scoped adapter capability",
                ));
            }
            validate_project_pi_settings(&root)?;
            if let EnforcementCmd::PrepareRuntime(_) = command.as_ref() {
                let requirements =
                    enforcement::gate_executor_requirements(&root, config_dir.as_deref())?;
                let runtime = gate_executor::prepare(&root, &run_dir, &requirements)?;
                let result = serde_json::json!({
                    "kind": "nopal.enforcement.prepare_runtime/v1",
                    "ok": true,
                    "executor_digest": runtime.digest,
                    "runtime_digest": runtime.runtime_digest,
                });
                return print_report_and_exit(
                    true,
                    cli.json,
                    || serde_json::to_string_pretty(&result),
                    || serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_owned()),
                );
            }
            if let EnforcementCmd::CleanupRuntime(_) = command.as_ref() {
                let runtime =
                    gate_executor::load(&run_dir, &args.executor_digest, &args.runtime_digest)?;
                gate_executor::cleanup(&runtime)?;
                let result = serde_json::json!({
                    "kind": "nopal.enforcement.cleanup_runtime/v1",
                    "ok": true,
                    "run_id": args.run_id,
                });
                return print_report_and_exit(
                    true,
                    cli.json,
                    || serde_json::to_string_pretty(&result),
                    || serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_owned()),
                );
            }
            if let EnforcementCmd::RecordOutcome(outcome) = command.as_ref() {
                let evidence = enforcement::tool_outcome_evidence(
                    &outcome.enforcement.action,
                    &outcome.authorization_binding,
                    &outcome.enforcement.tool_call_id,
                    &outcome.release_id,
                    outcome.outcome,
                    receipt_key.as_bytes(),
                )?;
                enforcement_adapter::apply_evidence(&run_dir, evidence)?;
                let result = serde_json::json!({
                    "kind": "nopal.enforcement.record_outcome/v1",
                    "ok": true,
                    "authorization_binding": outcome.authorization_binding,
                    "tool_call_id": outcome.enforcement.tool_call_id,
                    "release_id": outcome.release_id,
                    "outcome": outcome.outcome,
                });
                return print_report_and_exit(
                    true,
                    cli.json,
                    || serde_json::to_string_pretty(&result),
                    || serde_json::to_string_pretty(&result).unwrap_or_else(|_| "{}".to_owned()),
                );
            }
            if !(cfg!(debug_assertions) && args.executor_digest == "legacy-executor") {
                gate_executor::validate(&run_dir, &args.executor_digest, &args.runtime_digest)?;
            }
            let request = enforcement::EnforcementRequest {
                root: &root,
                config_dir: config_dir.as_deref(),
                mode: args.mode,
                action: &args.action,
                classes: &args.classes,
                run_dir: Some(&run_dir),
                receipt_key: Some(receipt_key.as_bytes()),
            };
            let workspace = enforcement_adapter::observe(&root)?;
            let intent = enforcement::EnforcementIntent {
                kind: enforcement::ENFORCEMENT_INTENT_KIND.to_owned(),
                launch_id: args.launch_id.clone(),
                session_id: args.session_id.clone(),
                tool_call_id: args.tool_call_id.clone(),
                tool_name: args.tool_name.clone(),
                input_digest: args.input_digest.clone(),
                target_digest: args.target_digest.clone(),
                executor_digest: args.executor_digest.clone(),
                changed_files: if args.changed_files.is_empty() {
                    workspace.changed_files
                } else {
                    args.changed_files.clone()
                },
                workspace_fingerprint: Some(workspace.fingerprint),
                mutates: args.mutates,
            };
            match command.as_ref() {
                EnforcementCmd::VerifyEvidence(_) | EnforcementCmd::Advance(_) => {
                    let runtime =
                        gate_executor::load(&run_dir, &args.executor_digest, &args.runtime_digest)?;
                    let purpose = if matches!(command.as_ref(), EnforcementCmd::VerifyEvidence(_)) {
                        verification::VerificationPurpose::EvidenceOnly
                    } else {
                        verification::VerificationPurpose::AuthorizeProtectedAction
                    };
                    let outcome = verification::advance(verification::VerificationRequest {
                        root: &root,
                        config_dir: config_dir.as_deref(),
                        run_dir: &run_dir,
                        mode: args.mode,
                        action: &args.action,
                        classes: &args.classes,
                        receipt_key: receipt_key.as_bytes(),
                        runtime: &runtime,
                        intent,
                        purpose,
                    })?;
                    print_report_and_exit(
                        true,
                        cli.json,
                        || serde_json::to_string_pretty(&outcome),
                        || {
                            serde_json::to_string_pretty(&outcome)
                                .unwrap_or_else(|_| "{}".to_owned())
                        },
                    )
                }
                EnforcementCmd::Plan(_) => {
                    let report = enforcement::plan_for_intent(request, intent)?;
                    enforcement_adapter::apply_evidence(
                        &run_dir,
                        enforcement::decision_evidence(&report)?,
                    )?;
                    print_report_and_exit(
                        report.ok,
                        cli.json,
                        || serde_json::to_string_pretty(&report),
                        || {
                            serde_json::to_string_pretty(&report)
                                .unwrap_or_else(|_| "{}".to_owned())
                        },
                    )
                }
                EnforcementCmd::RecordGate(record) => {
                    let evidence = enforcement::gate_evidence_for_intent(
                        request,
                        intent,
                        &record.gate_id,
                        record.exit_code,
                        &enforcement::GateExecutionContext {
                            contract_digest: record.contract_digest.clone(),
                            workspace_fingerprint: record.workspace_fingerprint.clone(),
                            gate_definition_digest: record.gate_definition_digest.clone(),
                            authorization_binding: record.authorization_binding.clone(),
                        },
                    )?;
                    enforcement_adapter::apply_evidence(&run_dir, evidence)?;
                    let report = serde_json::json!({
                        "kind": "nopal.enforcement.record_gate/v2",
                        "ok": true,
                        "gate_id": record.gate_id,
                        "exit_code": record.exit_code,
                    });
                    print_report_and_exit(
                        true,
                        cli.json,
                        || serde_json::to_string_pretty(&report),
                        || {
                            serde_json::to_string_pretty(&report)
                                .unwrap_or_else(|_| "{}".to_owned())
                        },
                    )
                }
                EnforcementCmd::RecordApproval(record) => {
                    let report = enforcement::plan_for_intent(request, intent)?;
                    if report.authorization_binding != record.authorization_binding {
                        return Err(std::io::Error::other(
                            "approval subject changed before the human response was recorded",
                        ));
                    }
                    let evidence = enforcement::approval_evidence(
                        &report,
                        record.approved,
                        &record.by,
                        receipt_key.as_bytes(),
                    )?;
                    enforcement_adapter::apply_evidence(&run_dir, evidence)?;
                    let result = serde_json::json!({
                        "kind": "nopal.enforcement.record_approval/v1",
                        "ok": true,
                        "approved": record.approved,
                        "authorization_binding": record.authorization_binding,
                    });
                    print_report_and_exit(
                        true,
                        cli.json,
                        || serde_json::to_string_pretty(&result),
                        || {
                            serde_json::to_string_pretty(&result)
                                .unwrap_or_else(|_| "{}".to_owned())
                        },
                    )
                }
                EnforcementCmd::Authorize(authorize) => {
                    let report = enforcement::plan_for_intent(request, intent)?;
                    if report.authorization_binding != authorize.authorization_binding {
                        return Err(std::io::Error::other(
                            "authorization subject changed before release",
                        ));
                    }
                    let release_id =
                        enforcement::authorization_release_id(&report, receipt_key.as_bytes())?;
                    let evidence = enforcement::authorization_release_evidence(
                        &report,
                        receipt_key.as_bytes(),
                    )?;
                    enforcement_adapter::apply_evidence(&run_dir, evidence)?;
                    let result = serde_json::json!({
                        "kind": "nopal.enforcement.authorization/v1",
                        "ok": true,
                        "authorization_binding": report.authorization_binding,
                        "tool_call_id": report.intent.tool_call_id,
                        "release_id": release_id,
                    });
                    print_report_and_exit(
                        true,
                        cli.json,
                        || serde_json::to_string_pretty(&result),
                        || {
                            serde_json::to_string_pretty(&result)
                                .unwrap_or_else(|_| "{}".to_owned())
                        },
                    )
                }
                EnforcementCmd::PrepareRuntime(_) => {
                    unreachable!("runtime preparation returns before workspace observation")
                }
                EnforcementCmd::CleanupRuntime(_) => {
                    unreachable!("runtime cleanup returns before workspace observation")
                }
                EnforcementCmd::RecordOutcome(_) => {
                    unreachable!("outcomes return before workspace observation")
                }
            }
        }
        Some(Cmd::Policy { command }) => {
            let (view, args) = match command {
                PolicyCmd::Evaluate(args) => (policy::View::Evaluate, args),
                PolicyCmd::Placement(args) => (policy::View::Placement, args),
                PolicyCmd::Decide(args) => (policy::View::Decide, args),
            };
            let request = policy::EvalRequest {
                mode: args.mode,
                action: &args.action,
                classes: &args.classes,
                env: &args.env,
            };
            let report = policy::run(&root, view, &request)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || policy::report_toon(&report),
            )
        }
    }
}

fn run_export_cmd(cli: &Cli, root: &Path, command: &ExportCmd) -> std::io::Result<ExitCode> {
    match command {
        ExportCmd::Process {
            output,
            stdout,
            check,
        } => {
            let artifact = process_artifact::build(root)?;
            let artifact_json =
                process_artifact::artifact_json(&artifact).map_err(std::io::Error::other)?;
            let output_path = output
                .clone()
                .unwrap_or_else(|| root.join(process_artifact::default_artifact_path()));
            let display_path = output_path.to_string_lossy().into_owned();

            if *check {
                let actual_text = match std::fs::read_to_string(&output_path) {
                    Ok(text) => Some(text),
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
                    Err(err) => return Err(err),
                };
                let report = process_artifact::check_report(
                    display_path,
                    &artifact,
                    &artifact_json,
                    actual_text.as_deref(),
                );
                return print_report_and_exit(
                    report.ok,
                    cli.json,
                    || serde_json::to_string_pretty(&report),
                    || process_artifact::check_report_toon(&report),
                );
            }

            if *stdout {
                print!("{artifact_json}");
                return Ok(if artifact.ok() {
                    ExitCode::SUCCESS
                } else {
                    ExitCode::from(1)
                });
            }

            if let Some(parent) = output_path.parent()
                && !parent.as_os_str().is_empty()
            {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&output_path, artifact_json.as_bytes())?;
            let report = process_artifact::export_report(display_path, &artifact, &artifact_json);
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || process_artifact::export_report_toon(&report),
            )
        }
    }
}

fn run_import_cmd(cli: &Cli, root: &Path, command: &ImportCmd) -> std::io::Result<ExitCode> {
    match command {
        ImportCmd::BeislidWorkflow {
            source,
            output_dir,
            write,
            overwrite,
            check,
        } => {
            let report = beislid_import::import(
                root,
                &ImportOptions {
                    source: source.clone(),
                    output_dir: output_dir.clone(),
                    write: *write,
                    overwrite: *overwrite,
                    check: *check,
                },
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || beislid_import::report_toon(&report),
            )
        }
    }
}

fn run_headless_verify(
    cli: &Cli,
    root: &Path,
    state_dir: Option<&Path>,
) -> std::io::Result<ExitCode> {
    const FLOW: &str = "verification";
    const ACTION: &str = "git.push";
    const CLASSES: [policy::ActionClass; 1] = [policy::ActionClass::GitRemote];

    let ledger_env = nopal_core::run_ledger_store::LedgerEnv::discover(root, state_dir);
    let initialized = nopal_core::run_ledger_store::init_run(
        &ledger_env,
        &InitArgs {
            skill: "verify",
            flow: Some(FLOW),
            ticket_id: "none",
            ticket_title: "Headless pre-PR verification",
            ticket_url: "",
            branch: None,
            run_id: None,
        },
    )
    .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    let run_dir = initialized.run_dir;
    let config_dir = resolve_config_dir();
    let executor_requirements =
        enforcement::gate_executor_requirements(root, config_dir.as_deref())?;
    let runtime = gate_executor::prepare(root, &run_dir, &executor_requirements)?;

    let mut receipt_key = [0u8; 32];
    getrandom::fill(&mut receipt_key)
        .map_err(|error| std::io::Error::other(format!("receipt entropy failed: {error}")))?;
    let invocation = format!("headless:{}", initialized.run_id);
    let input_digest = format!(
        "sha256:{:x}",
        Sha256::digest(b"nopal.verify.pre_pr/v1:git.push:git_remote")
    );
    let canonical_root = root.canonicalize()?;
    let target_digest = format!(
        "sha256:{:x}",
        Sha256::digest(canonical_root.as_os_str().as_encoded_bytes())
    );
    let request = verification::VerificationRequest {
        root,
        config_dir: config_dir.as_deref(),
        run_dir: &run_dir,
        mode: policy::Mode::SupervisedAuto,
        action: ACTION,
        classes: &CLASSES,
        receipt_key: &receipt_key,
        runtime: &runtime,
        intent: enforcement::EnforcementIntent {
            kind: enforcement::ENFORCEMENT_INTENT_KIND.to_owned(),
            launch_id: invocation.clone(),
            session_id: invocation.clone(),
            tool_call_id: invocation,
            tool_name: "headless_verify".to_owned(),
            input_digest,
            target_digest,
            executor_digest: runtime.digest.clone(),
            changed_files: Vec::new(),
            workspace_fingerprint: None,
            mutates: true,
        },
        purpose: verification::VerificationPurpose::EvidenceOnly,
    };

    let outcome = match verification::advance(request) {
        Ok(outcome) => outcome,
        Err(error) => {
            let _ = gate_executor::cleanup(&runtime);
            let _ = nopal_core::run_ledger_store::record_interrupt(
                &run_dir,
                &format!("headless verification infrastructure failed: {error}"),
                Some("repair the reported infrastructure failure and run nopal verify again"),
            );
            return Err(error);
        }
    };
    if let Err(error) = gate_executor::cleanup(&runtime) {
        let _ = nopal_core::run_ledger_store::record_interrupt(
            &run_dir,
            &format!("headless verification cleanup failed: {error}"),
            Some("inspect the run-private gate temporary directory before retrying"),
        );
        return Err(error);
    }
    let verified = matches!(&outcome, verification::VerificationOutcome::Verified { .. });
    let approval_required = matches!(
        &outcome,
        verification::VerificationOutcome::ApprovalRequired { .. }
    );
    if approval_required {
        nopal_core::run_ledger_store::record_interrupt(
            &run_dir,
            "headless verification requires interactive policy approval",
            Some("rerun the protected action through an interactive Nopal-launched Pi session"),
        )
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    } else {
        nopal_core::run_ledger_store::record_finalize(
            &run_dir,
            if verified { "completed" } else { "failed" },
            None,
        )
        .map_err(|error| std::io::Error::other(format!("{error:?}")))?;
    }

    let report = serde_json::json!({
        "kind": "nopal.verification/v1",
        "ok": verified,
        "run_id": initialized.run_id,
        "flow": initialized.flow,
        "outcome": outcome,
    });
    print_report_and_exit(
        verified,
        cli.json,
        || serde_json::to_string_pretty(&report),
        || serde_json::to_string_pretty(&report).unwrap_or_else(|_| "{}".to_owned()),
    )
}

/// Python `load_payload`: no file means an empty object payload.
fn load_payload(path: Option<&std::path::Path>) -> std::io::Result<ledger_core::JsonValue> {
    match path {
        None => Ok(nopal_ledger_json::json!({})),
        Some(path) => {
            let bytes = nopal_core::run_ledger_store::read_bounded_regular_file(
                path,
                ledger_core::DOCUMENT_LIMIT,
                "ledger payload",
            )?;
            let text = String::from_utf8(bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            nopal_ledger_json::from_str(&text).map_err(std::io::Error::other)
        }
    }
}

fn run_ledger_cmd(
    cli: &Cli,
    root: &Path,
    state_dir: Option<&std::path::Path>,
    command: &LedgerCmd,
) -> std::io::Result<ExitCode> {
    match command {
        // Pointer is repo-local (the discovered project root); it does not
        // touch the run-ledger state dir at all, unlike every other ledger
        // subcommand.
        LedgerCmd::Pointer => {
            let report = ledger::ledger_pointer(root)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::pointer_toon(&report),
            )
        }
        LedgerCmd::Init {
            skill,
            flow,
            ticket_id,
            ticket_title,
            ticket_url,
            branch,
            run_id,
        } => {
            let report = ledger::ledger_init(
                root,
                state_dir,
                &InitArgs {
                    skill,
                    flow: flow.as_deref(),
                    ticket_id,
                    ticket_title,
                    ticket_url,
                    branch: branch.as_deref(),
                    run_id: run_id.as_deref(),
                },
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::init_toon(&report),
            )
        }
        LedgerCmd::Event {
            run_id,
            flow,
            event_type,
            json_file,
            summary,
            operation_id,
        } => {
            let payload = load_payload(json_file.as_deref())?;
            let report = ledger::ledger_event(
                root,
                state_dir,
                &ledger::EventArgs {
                    run_id,
                    flow: flow.as_deref(),
                    event_type,
                    payload,
                    summary: summary.as_deref(),
                    operation_id: operation_id.as_deref(),
                },
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::mutation_toon(&report),
            )
        }
        LedgerCmd::Checkpoint {
            run_id,
            flow,
            name,
            json_file,
            resume_hint,
            operation_id,
        } => {
            let payload = load_payload(json_file.as_deref())?;
            let report = ledger::ledger_checkpoint(
                root,
                state_dir,
                &ledger::CheckpointArgs {
                    run_id,
                    flow: flow.as_deref(),
                    name,
                    payload,
                    resume_hint: resume_hint.as_deref(),
                    operation_id: operation_id.as_deref(),
                },
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::mutation_toon(&report),
            )
        }
        LedgerCmd::Gate {
            run_id,
            flow,
            name,
            scope,
            envelope_file,
            resume_hint,
            operation_id,
        } => {
            let envelope = load_payload(Some(envelope_file))?;
            let report = ledger::ledger_gate(
                root,
                state_dir,
                &ledger::GateArgs {
                    run_id,
                    flow: flow.as_deref(),
                    name,
                    scope: scope.as_deref(),
                    envelope,
                    resume_hint: resume_hint.as_deref(),
                    operation_id: operation_id.as_deref(),
                },
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::mutation_toon(&report),
            )
        }
        LedgerCmd::Interrupt {
            run_id,
            flow,
            reason,
            resume_hint,
            operation_id,
        } => {
            let report = ledger::ledger_interrupt(
                root,
                state_dir,
                run_id,
                flow.as_deref(),
                reason,
                resume_hint.as_deref(),
                operation_id.as_deref(),
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::mutation_toon(&report),
            )
        }
        LedgerCmd::Continue {
            run_id,
            flow,
            operation_id,
        } => {
            let report = ledger::ledger_continue(
                root,
                state_dir,
                run_id,
                flow.as_deref(),
                operation_id.as_deref(),
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::mutation_toon(&report),
            )
        }
        LedgerCmd::Finalize {
            run_id,
            flow,
            status,
            report_file,
            operation_id,
        } => {
            let report = ledger::ledger_finalize(
                root,
                state_dir,
                run_id,
                flow.as_deref(),
                status,
                report_file.as_deref(),
                operation_id.as_deref(),
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::mutation_toon(&report),
            )
        }
        LedgerCmd::Resume {
            run_id,
            flow,
            ticket_id,
            branch,
            include_completed,
        } => {
            let report = ledger::ledger_resume(
                root,
                state_dir,
                &ledger::ResumeArgs {
                    run_id: run_id.as_deref(),
                    flow: flow.as_deref(),
                    ticket_id: ticket_id.as_deref(),
                    branch: branch.as_deref(),
                    include_completed: *include_completed,
                },
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::resume_toon(&report),
            )
        }
        LedgerCmd::Dashboard { flow, all, limit } => {
            let report = ledger::ledger_dashboard(
                root,
                state_dir,
                &ledger::DashboardArgs {
                    flow: flow.as_deref(),
                    all: *all,
                    limit: *limit,
                },
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::dashboard_toon(&report),
            )
        }
        LedgerCmd::Prune { stale_after, apply } => {
            let report = ledger::ledger_prune(
                root,
                state_dir,
                &ledger::PruneArgs {
                    stale_after_hours: *stale_after,
                    apply: *apply,
                },
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::prune_toon(&report),
            )
        }
    }
}

/// Resolve user-level Nopal policy and enforcement state without granting it
/// project-package authority. The checked-in distribution contract is the
/// only source for project resources; this directory remains relevant to the
/// restrictive user-policy composition and enforcement subprocess context.
fn resolve_config_dir() -> Option<PathBuf> {
    if let Some(dir) = std::env::var("NOPAL_CONFIG_DIR")
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Some(PathBuf::from(dir));
    }
    std::env::var("HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|home| Path::new(&home).join(".config").join("nopal"))
}

fn resolve_data_dir() -> std::io::Result<PathBuf> {
    if let Some(dir) = std::env::var("NOPAL_DATA_DIR")
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Ok(PathBuf::from(dir));
    }
    if let Some(dir) = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|value| !value.is_empty())
    {
        return Ok(Path::new(&dir).join("nopal"));
    }
    std::env::var("HOME")
        .ok()
        .filter(|value| !value.is_empty())
        .map(|home| Path::new(&home).join(".local/share/nopal"))
        .ok_or_else(|| {
            std::io::Error::other("cannot resolve Nopal data directory; set NOPAL_DATA_DIR or HOME")
        })
}

/// Resolve the complete built-in Nopal package from an explicit distribution
/// root, the release-relative layout, or the source checkout used for local
/// builds. The lock binds the adapter and Beislið skill trees beneath this
/// root, so an arbitrary candidate cannot become trusted by path alone.
fn resolve_builtin_distribution_root() -> std::io::Result<PathBuf> {
    let mut candidates = Vec::new();
    // Production authority must come from the immutable installed release.
    // The override exists only for debug proofs that construct adversarial
    // distributions without mutating packaged bytes.
    if cfg!(debug_assertions)
        && let Some(root) = std::env::var_os("NOPAL_DISTRIBUTION_ROOT")
    {
        let root = PathBuf::from(root);
        candidates.push(root.clone());
        candidates.push(root.join("share/nopal"));
    }
    if let Ok(executable) = std::env::current_exe() {
        candidates.extend(packaged_distribution_candidates(&executable));
    }
    if cfg!(debug_assertions) {
        candidates.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."));
    }
    resolve_builtin_distribution_root_from(candidates)
}

fn packaged_distribution_candidates(executable: &Path) -> Vec<PathBuf> {
    executable_parents(executable)
        .into_iter()
        .map(|parent| parent.join("share/nopal"))
        .collect()
}

/// An installed launcher is a stable symlink into an immutable release.
/// Check both the invocation parent and the held canonical target parent so
/// release-relative resources work without trusting ambient `PATH`.
fn executable_parents(executable: &Path) -> Vec<PathBuf> {
    let mut parents = Vec::new();
    if let Some(parent) = executable.parent() {
        parents.push(parent.to_owned());
    }
    if let Ok(canonical) = executable.canonicalize()
        && let Some(parent) = canonical.parent()
        && !parents.iter().any(|candidate| candidate == parent)
    {
        parents.push(parent.to_owned());
    }
    parents
}

fn resolve_builtin_distribution_root_from(
    candidates: impl IntoIterator<Item = PathBuf>,
) -> std::io::Result<PathBuf> {
    for candidate in candidates {
        let adapter = candidate.join("extensions/policy-gate");
        let adapter_complete = ["index.ts", "classifier.ts", "guard.ts", "nopal-cli.ts"]
            .iter()
            .all(|name| adapter.join(name).is_file());
        let beislid_complete = candidate.join("resources/beislid/LICENSE").is_file()
            && candidate
                .join("resources/beislid/provenance.json")
                .is_file()
            && candidate.join("resources/beislid/skills").is_dir();
        if adapter_complete && beislid_complete {
            return candidate.canonicalize();
        }
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "cannot locate the complete built-in Nopal adapter and Beislið skill package",
    ))
}

fn run_distribution_update(cli: &Cli, root: &Path, write: bool) -> std::io::Result<ExitCode> {
    let builtin_root = resolve_builtin_distribution_root()?;
    let store_root = resolve_data_dir()?.join("packages");
    let report = distribution_adapter::update(
        root,
        &store_root,
        nopal_core::distribution::BuiltinDistribution {
            version: env!("CARGO_PKG_VERSION"),
            root: &builtin_root,
        },
        &distribution_adapter::npm_program(),
        write,
    )?;
    print_report_and_exit(
        report.ok,
        cli.json,
        || serde_json::to_string_pretty(&report),
        || distribution_adapter::human_update(&report),
    )
}

fn run_distribution_sync(cli: &Cli, root: &Path) -> std::io::Result<ExitCode> {
    let builtin_root = resolve_builtin_distribution_root()?;
    let store_root = resolve_data_dir()?.join("packages");
    let report = distribution_adapter::sync(
        nopal_core::distribution::DistributionContext {
            project_root: root,
            store_root: &store_root,
            builtin: nopal_core::distribution::BuiltinDistribution {
                version: env!("CARGO_PKG_VERSION"),
                root: &builtin_root,
            },
        },
        &distribution_adapter::npm_program(),
    )?;
    print_report_and_exit(
        report.ok,
        cli.json,
        || serde_json::to_string_pretty(&report),
        || distribution_adapter::human_sync(&report),
    )
}

/// Runs the cold `nopal.launch/v1` gates for `nopal cli`. On `--dry-run` or
/// any gate failure, renders the plan and exits without touching Pi.
///
/// A real launch against an unconfigured Git repository writes the complete
/// six-file project, policy, gate, distribution-lock, and Beislið baseline,
/// then re-runs this same planner against the committed bytes. That second
/// validation, not the write itself, is the launch gate. A partial write or
/// unexpectedly invalid generated contract fails closed exactly like any
/// other invalid project. Only a passing plan with `dry_run` false reaches
/// `exec_pi`.
///
/// Two stderr notices are always-on, never gated by `--verbose`
/// unlike `launch::summary_line`: a scaffold-provenance line only on a
/// launch that just scaffolded, and a resource-surface line
/// (`launch::resource_surface_line`) on every real launch, printed
/// immediately before `exec_pi`.
fn dispatch_launch(
    cli: &Cli,
    root: &Path,
    dry_run: bool,
    with_ambient: bool,
    verbose: bool,
) -> std::io::Result<ExitCode> {
    if with_ambient {
        return Err(std::io::Error::other(
            "ambient Pi resources may only be enabled by the checked-in distribution contract; --with-ambient is not an authority source",
        ));
    }
    let store_root = resolve_data_dir()?.join("packages");
    let builtin_root = resolve_builtin_distribution_root()?;
    let builtin = nopal_core::distribution::BuiltinDistribution {
        version: env!("CARGO_PKG_VERSION"),
        root: &builtin_root,
    };
    let context = launch::LaunchContext {
        store_root: &store_root,
        builtin,
    };
    let mut plan = launch::plan(root, context)?;
    if dry_run || (!plan.ok && plan.scaffold != launch::Scaffold::WouldCreate) {
        return print_report_and_exit(
            plan.ok,
            cli.json,
            || serde_json::to_string_pretty(&plan),
            || launch::launch_toon(&plan),
        );
    }

    let mut created_paths = None;
    let plan = if plan.scaffold == launch::Scaffold::WouldCreate {
        let baseline = plan.prepared_baseline.take().ok_or_else(|| {
            std::io::Error::other("launch plan omitted its prepared scaffold baseline")
        })?;
        let scaffolded = scaffold::write_planned_baseline(root, baseline.clone())?;
        let rescaffolded = launch::plan(root, context)?;
        let marked = launch::mark_scaffolded(rescaffolded, &baseline);
        eprintln!("{}", scaffold_notice(&scaffolded));
        if !marked.ok {
            return print_report_and_exit(
                false,
                cli.json,
                || serde_json::to_string_pretty(&marked),
                || launch::launch_toon(&marked),
            );
        }
        created_paths = Some(scaffolded.rel_paths);
        marked
    } else {
        plan
    };

    if plan.ambient_kinds.contains(&"extensions") {
        return Err(std::io::Error::other(
            "enforcement initialization failed: ambient Pi extensions are not part of the trusted executable bundle",
        ));
    }
    if cli.pi_args.iter().any(|argument| {
        matches!(
            argument.as_str(),
            "-e" | "--extension"
                | "--skill"
                | "--prompt-template"
                | "--theme"
                | "--tools"
                | "--no-tools"
        ) || argument.starts_with("--extension=")
            || argument.starts_with("--skill=")
            || argument.starts_with("--prompt-template=")
            || argument.starts_with("--theme=")
            || argument.starts_with("--tools=")
    }) {
        return Err(std::io::Error::other(
            "enforcement initialization failed: Pi resources and the active tool catalog must come from the checked-in Nopal distribution contract",
        ));
    }
    let enforcement_extension = verify_trusted_extensions(&plan.pi_argv)?;
    let enforcement_adapter_dir = enforcement_extension.parent().ok_or_else(|| {
        std::io::Error::other(
            "enforcement initialization failed: adapter path has no parent directory",
        )
    })?;
    let enforcement_cli = std::env::current_exe()?.canonicalize()?;

    let config_dir = resolve_config_dir();
    let workspace = enforcement_adapter::observe(root)?;
    let enforcement_plan = enforcement::plan_for_intent(
        enforcement::EnforcementRequest {
            root,
            config_dir: config_dir.as_deref(),
            mode: policy::Mode::SupervisedAuto,
            action: "git.push",
            classes: &[policy::ActionClass::GitRemote],
            run_dir: None,
            receipt_key: None,
        },
        enforcement::EnforcementIntent {
            kind: enforcement::ENFORCEMENT_INTENT_KIND.to_owned(),
            launch_id: "launch-preflight".to_owned(),
            session_id: "launch-preflight".to_owned(),
            tool_call_id: "launch-preflight".to_owned(),
            tool_name: "bash".to_owned(),
            input_digest: "launch-preflight".to_owned(),
            target_digest: "bound-repository".to_owned(),
            executor_digest: "launch-preflight".to_owned(),
            changed_files: workspace.changed_files,
            workspace_fingerprint: Some(workspace.fingerprint),
            mutates: true,
        },
    )?;
    if !enforcement_plan.ok {
        return print_report_and_exit(
            false,
            cli.json,
            || serde_json::to_string_pretty(&enforcement_plan),
            || serde_json::to_string_pretty(&enforcement_plan).unwrap_or_else(|_| "{}".to_owned()),
        );
    }
    let gate_executor_requirements =
        enforcement::gate_executor_requirements(root, config_dir.as_deref())?;

    let ledger_env = nopal_core::run_ledger_store::LedgerEnv::discover(root, None);
    let run = nopal_core::run_ledger_store::init_run(
        &ledger_env,
        &InitArgs {
            skill: "nopal",
            flow: Some("enforcement"),
            ticket_id: "none",
            ticket_title: "Nopal Pi session",
            ticket_url: "",
            branch: None,
            run_id: None,
        },
    )
    .map_err(|error| {
        std::io::Error::other(format!(
            "enforcement ledger initialization failed: {error:?}"
        ))
    })?;

    if let Some(paths) = created_paths {
        eprintln!(
            "nopal: check in generated project baseline: {}",
            paths.join(", ")
        );
    }
    eprintln!("{}", launch::resource_surface_line(&plan));
    if verbose {
        eprintln!("{}", launch::summary_line(&plan));
    }
    std::fs::create_dir_all(run.run_dir.join("artifacts/enforcement"))?;
    validate_project_pi_settings(root)?;
    let gate_runtime = gate_executor::prepare(root, &run.run_dir, &gate_executor_requirements)?;
    let pi_runtime_dir = prepare_pi_runtime_dir(&run.run_dir)?;
    let adapter_capability = LaunchCapability::new()?;
    // The enforced distribution pins its runtime policy mode. Ambient process
    // state is not contract authority and therefore cannot select a weaker mode.
    let policy_mode = "supervised_auto".to_owned();
    let mut pi_argv = plan.pi_argv;
    pi_argv.extend([
        "--tools".to_owned(),
        "bash,edit,find,grep,ls,read,write".to_owned(),
    ]);
    pi_argv.extend(cli.pi_args.iter().cloned());
    let launch_env = EnforcementLaunchEnv {
        run_id: &run.run_id,
        root,
        state_dir: &ledger_env.state_dir,
        config_dir: config_dir.as_deref(),
        adapter_dir: enforcement_adapter_dir,
        cli: &enforcement_cli,
        pi_runtime_dir: &pi_runtime_dir,
        capability_fd: adapter_capability.fd(),
        policy_mode: &policy_mode,
        gate_executor_bin: &gate_runtime.bin_dir,
        gate_home: &gate_runtime.home_dir,
        gate_executor_digest: &gate_runtime.digest,
        gate_runtime_digest: &gate_runtime.runtime_digest,
    };
    let pi_binary = resolve_pi_binary()?;
    let pi_node = resolve_pi_node()?;
    let (pi_binary, pi_node) =
        snapshot_locked_pi_runtime(&pi_binary, pi_node.as_deref(), &run.run_dir)?;
    let pi_identity = pi_binary_identity(&pi_binary)?;
    let node_identity = pi_node.as_deref().map(pi_binary_identity).transpose()?;
    probe_pi_runtime(
        &pi_binary,
        pi_node.as_deref(),
        root,
        &pi_argv,
        &launch_env,
        &run.run_dir,
    )?;
    if !(cfg!(debug_assertions) && std::env::var_os("NOPAL_TEST_PI_BIN").is_some()) {
        validate_pi_package_identity(&pi_binary)?;
    }
    if pi_binary_identity(&pi_binary)? != pi_identity {
        return Err(std::io::Error::other(
            "installed Pi executable changed between enforcement probe and session handoff",
        ));
    }
    if pi_node.as_deref().map(pi_binary_identity).transpose()? != node_identity {
        return Err(std::io::Error::other(
            "installed Node runtime changed between enforcement probe and session handoff",
        ));
    }
    exec_pi(&pi_binary, pi_node.as_deref(), root, &pi_argv, &launch_env)
}

fn verify_trusted_extensions(pi_argv: &[String]) -> std::io::Result<PathBuf> {
    let mut enforcement_extension = None;
    for pair in pi_argv
        .windows(2)
        .filter(|pair| pair[0] == "-e" || pair[0] == "--extension")
    {
        let path = PathBuf::from(&pair[1]);
        let normalized = pair[1].replace('\\', "/");
        if normalized.ends_with("/extensions/policy-gate/index.ts") {
            if enforcement_extension.is_some() {
                return Err(std::io::Error::other(
                    "enforcement initialization failed: the trusted adapter is pinned more than once",
                ));
            }
            verify_enforcement_adapter(&path)?;
            enforcement_extension = Some(path);
        } else if normalized.ends_with("/deterministic-enforcement-provider.mjs") {
            verify_exact_source(
                &path,
                include_bytes!("../tests/fixtures/deterministic-enforcement-provider.mjs"),
                "deterministic enforcement proof provider",
            )?;
        } else {
            return Err(std::io::Error::other(format!(
                "enforcement initialization failed: untrusted executable Pi extension {}",
                path.display()
            )));
        }
    }
    enforcement_extension.ok_or_else(|| {
        std::io::Error::other(
            "enforcement initialization failed: the pinned bundle does not contain extensions/policy-gate/index.ts",
        )
    })
}

fn verify_exact_source(path: &Path, expected: &[u8], label: &str) -> std::io::Result<()> {
    let actual = std::fs::read(path).map_err(|error| {
        std::io::Error::other(format!(
            "enforcement initialization failed: could not read trusted {label} file {}: {error}",
            path.display()
        ))
    })?;
    if actual != expected {
        return Err(std::io::Error::other(format!(
            "enforcement initialization failed: {label} identity mismatch for {}",
            path.display()
        )));
    }
    Ok(())
}

fn verify_enforcement_adapter(index_path: &Path) -> std::io::Result<()> {
    const SOURCES: [(&str, &[u8]); 4] = [
        (
            "index.ts",
            include_bytes!("../../../extensions/policy-gate/index.ts"),
        ),
        (
            "classifier.ts",
            include_bytes!("../../../extensions/policy-gate/classifier.ts"),
        ),
        (
            "guard.ts",
            include_bytes!("../../../extensions/policy-gate/guard.ts"),
        ),
        (
            "nopal-cli.ts",
            include_bytes!("../../../extensions/policy-gate/nopal-cli.ts"),
        ),
    ];
    let adapter_dir = index_path.parent().ok_or_else(|| {
        std::io::Error::other(
            "enforcement initialization failed: adapter path has no parent directory",
        )
    })?;
    for (name, expected) in SOURCES {
        let path = adapter_dir.join(name);
        verify_exact_source(&path, expected, "adapter")?;
    }
    Ok(())
}

/// The always-on created-notice, shared by the success path and the
/// scaffolded-then-failed re-plan path so a write is never silent.
fn scaffold_notice(scaffolded: &scaffold::Scaffolded) -> String {
    format!(
        "nopal: created complete project baseline [{}] ({})",
        scaffolded.rel_paths.join(", "),
        scaffolded.source.describe()
    )
}

#[cfg(unix)]
fn validate_project_pi_settings(cwd: &std::path::Path) -> std::io::Result<()> {
    use std::io::Read;
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

    let settings_dir = cwd.join(".pi");
    match std::fs::symlink_metadata(&settings_dir) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_dir() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "project Pi settings directory must be a real directory",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let path = settings_dir.join("settings.json");
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || !metadata.is_file() => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "project Pi settings must be a regular no-follow file",
            ));
        }
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path)?;
    let metadata = file.metadata()?;
    if metadata.nlink() != 1 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "project Pi settings must not have hardlink aliases",
        ));
    }
    if metadata.len() > 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "project Pi settings exceed the one MiB validation bound",
        ));
    }
    let mut text = String::with_capacity(metadata.len() as usize);
    file.read_to_string(&mut text)?;
    let value = jsonc_parser::parse_to_serde_value(&text, &jsonc_parser::ParseOptions::default())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error.to_string()))?
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "project Pi settings contain no JSON value",
            )
        })?;
    let object = value.as_object().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "project Pi settings must be a JSON object",
        )
    })?;
    for field in ["shellPath", "shellCommandPrefix"] {
        if object.contains_key(field) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("project Pi setting {field} can carry executable authority"),
            ));
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_project_pi_settings(_cwd: &std::path::Path) -> std::io::Result<()> {
    Err(std::io::Error::other(
        "project Pi settings validation requires unix",
    ))
}

#[cfg(unix)]
fn prepare_pi_runtime_dir(run_dir: &std::path::Path) -> std::io::Result<PathBuf> {
    let source_dir = std::env::var_os("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".pi/agent")));
    prepare_pi_runtime_dir_from(run_dir, source_dir.as_deref())
}

#[cfg(unix)]
fn prepare_pi_runtime_dir_from(
    run_dir: &std::path::Path,
    source_dir: Option<&std::path::Path>,
) -> std::io::Result<PathBuf> {
    use std::io::{Read, Write};
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

    let runtime_dir = run_dir.join("artifacts/pi-runtime");
    let runtime_home = runtime_dir.join("home");
    std::fs::create_dir_all(runtime_home.join("config"))?;
    std::fs::set_permissions(&runtime_dir, std::fs::Permissions::from_mode(0o700))?;
    std::fs::set_permissions(&runtime_home, std::fs::Permissions::from_mode(0o700))?;
    std::fs::set_permissions(
        runtime_home.join("config"),
        std::fs::Permissions::from_mode(0o700),
    )?;
    for name in [
        ".gitconfig",
        "kubeconfig",
        "npm-globalconfig",
        "npm-userconfig",
    ] {
        std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(runtime_home.join(name))?;
    }
    let Some(source) = source_dir.map(|directory| directory.join("auth.json")) else {
        return Ok(runtime_dir);
    };
    if !source.exists() {
        return Ok(runtime_dir);
    }
    let mut source_file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&source)?;
    let metadata = source_file.metadata()?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Pi authentication state is not a bounded regular file",
        ));
    }
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    source_file.read_to_end(&mut bytes)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    if !value.is_object() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Pi authentication state must be a JSON object",
        ));
    }
    let destination = runtime_dir.join("auth.json");
    let mut destination_file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(destination)?;
    destination_file.write_all(&bytes)?;
    destination_file.sync_all()?;
    Ok(runtime_dir)
}

#[cfg(not(unix))]
fn prepare_pi_runtime_dir(_run_dir: &std::path::Path) -> std::io::Result<PathBuf> {
    Err(std::io::Error::other(
        "isolated Pi runtime preparation requires unix",
    ))
}

#[cfg(unix)]
struct LaunchCapability {
    file: std::fs::File,
}

#[cfg(unix)]
impl LaunchCapability {
    fn new() -> std::io::Result<Self> {
        use std::io::{Seek, Write};
        use std::os::unix::io::AsRawFd;

        let mut file = tempfile::tempfile()?;
        file.write_all(enforcement::generate_receipt_key()?.as_bytes())?;
        file.seek(std::io::SeekFrom::Start(0))?;
        let fd = file.as_raw_fd();
        // SAFETY: fcntl operates on the live descriptor owned by `file`.
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            return Err(std::io::Error::last_os_error());
        }
        // The anonymous descriptor must survive the probe and final exec.
        // Child adapters map it explicitly; gate children do not inherit it.
        // SAFETY: the descriptor remains owned by `file`, and the flag value
        // comes from F_GETFD with only FD_CLOEXEC removed.
        if unsafe { libc::fcntl(fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC) } < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self { file })
    }

    fn fd(&self) -> i32 {
        use std::os::unix::io::AsRawFd;
        self.file.as_raw_fd()
    }
}

#[cfg(not(unix))]
struct LaunchCapability;

#[cfg(not(unix))]
impl LaunchCapability {
    fn new() -> std::io::Result<Self> {
        Err(std::io::Error::other(
            "launch capabilities require a unix descriptor channel",
        ))
    }

    fn fd(&self) -> i32 {
        -1
    }
}

struct EnforcementLaunchEnv<'a> {
    run_id: &'a str,
    root: &'a std::path::Path,
    state_dir: &'a std::path::Path,
    config_dir: Option<&'a std::path::Path>,
    adapter_dir: &'a std::path::Path,
    cli: &'a std::path::Path,
    pi_runtime_dir: &'a std::path::Path,
    capability_fd: i32,
    policy_mode: &'a str,
    gate_executor_bin: &'a std::path::Path,
    gate_home: &'a std::path::Path,
    gate_executor_digest: &'a str,
    gate_runtime_digest: &'a str,
}

#[cfg(unix)]
fn pi_binary_identity(path: &std::path::Path) -> std::io::Result<[u8; 32]> {
    use sha2::Digest;

    let bytes = std::fs::read(path)?;
    Ok(sha2::Sha256::digest(bytes).into())
}

const TRUSTED_PI_VERSION: &str = "0.80.6";
const TRUSTED_PI_DIST_INTEGRITY: &str =
    "sha256:e17228fa4d155a734026dc737eb71a790356e639ae29bca9c0a2b6105260d279";
// Generated from the exact npm 0.80.6 artifact with SRI
// sha512-vcfD6tOk402isLl3Cm/qbn2O10TvgroMp1+/fEGM24ZdvETFCdOYv5VZ7m59EI5fPsjfSJh+CpQ5bhBrhfOg7g==
// by the npm client shipped in official Node 22.22.0. Optional native
// dependencies make the complete tree platform-specific. Each tree hash
// includes dependency manifests, bytes, symlink targets, and executable modes.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
const TRUSTED_PI_RUNTIME_INTEGRITY: &str =
    "sha256:1849e5f3271e6386319323a9e0dbf0f171c6d22558c5e9c05717a20b116915e8";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
const TRUSTED_PI_RUNTIME_INTEGRITY: &str =
    "sha256:2a49edc0cbfae11a095051da5d6d79cd994c85c555ee4f24c74ec3700ef34b4e";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
const TRUSTED_PI_RUNTIME_INTEGRITY: &str =
    "sha256:1b7f4f85e0f36eafd10f3db15a6d4ba58087cf10e96ec3cee2e0d5bd00c5e2c1";

fn packaged_pi_binary(executable: &Path) -> Option<PathBuf> {
    executable_parents(executable)
        .into_iter()
        .map(|parent| parent.join("runtime/pi/dist/cli.js"))
        .find(|candidate| candidate.is_file())
}

fn packaged_node_binary(executable: &Path) -> Option<PathBuf> {
    executable_parents(executable)
        .into_iter()
        .map(|parent| parent.join("runtime/node"))
        .find(|candidate| candidate.is_file())
}

fn resolve_pi_binary() -> std::io::Result<PathBuf> {
    let configured = if cfg!(debug_assertions) {
        std::env::var_os("NOPAL_TEST_PI_BIN").map(PathBuf::from)
    } else {
        None
    };
    let uses_test_override = configured.is_some();
    let candidate = if let Some(path) = configured {
        path
    } else if let Some(path) = std::env::current_exe()
        .ok()
        .and_then(|executable| packaged_pi_binary(&executable))
    {
        path
    } else if cfg!(debug_assertions) {
        let path = std::env::var_os("PATH").ok_or_else(|| {
            std::io::Error::other("cannot resolve trusted Pi binary without PATH")
        })?;
        std::env::split_paths(&path)
            .map(|directory| directory.join("pi"))
            .find(|candidate| candidate.is_file())
            .ok_or_else(|| {
                std::io::Error::other(
                    "cannot resolve packaged Pi runtime or an exact installed Pi binary on PATH",
                )
            })?
    } else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "the installed Nopal release has no packaged Pi runtime",
        ));
    };
    let canonical = candidate.canonicalize().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "cannot canonicalize Pi executable {}: {error}",
                candidate.display()
            ),
        )
    })?;
    let metadata = canonical.metadata()?;
    if !metadata.is_file() {
        return Err(std::io::Error::other(format!(
            "Pi executable {} is not a regular file",
            canonical.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Pi executable {} is not executable", canonical.display()),
            ));
        }
    }
    if !uses_test_override {
        validate_pi_package_identity(&canonical)?;
    }
    Ok(canonical)
}

// Official Node v22.22.0 archives are byte-locked per released platform.
// Their executable loader closures contain only operating-system libraries;
// split package-manager builds with mutable non-system libraries are rejected.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
// Archive SHA-256: 5ed4db0fcf1eaf84d91ad12462631d73bf4576c1377e192d222e48026a902640.
const TRUSTED_NODE_INTEGRITY: &str =
    "sha256:913b144fdb40638b1acef7974ab3c33fbd527cc0974cb5da467ab1e6ac51b4d4";
#[cfg(all(target_os = "macos", target_arch = "x86_64"))]
// Archive SHA-256: 5ea50c9d6dea3dfa3abb66b2656f7a4e1c8cef23432b558d45fb538c7b5dedce.
const TRUSTED_NODE_INTEGRITY: &str =
    "sha256:bf0e0ff20d4e5a16436d1ec372e47161e52be8e487db8070ae3f06b01efbba0c";
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
// Archive SHA-256: c33c39ed9c80deddde77c960d00119918b9e352426fd604ba41638d6526a4744.
const TRUSTED_NODE_INTEGRITY: &str =
    "sha256:1bec56ef7cfa9a76f3e0b7c0a87f220eb73f23102b9c0b4c7529a3f7c3ce7c31";

fn resolve_pi_node() -> std::io::Result<Option<PathBuf>> {
    if cfg!(debug_assertions) && std::env::var_os("NOPAL_TEST_PI_BIN").is_some() {
        return Ok(None);
    }
    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    {
        let packaged = std::env::current_exe()
            .ok()
            .and_then(|executable| packaged_node_binary(&executable));
        let candidate = if let Some(packaged) = packaged {
            packaged
        } else if cfg!(debug_assertions) {
            std::env::var_os("PATH")
                .and_then(|path| {
                    std::env::split_paths(&path)
                        .map(|directory| directory.join("node"))
                        .find(|candidate| candidate.is_file())
                })
                .ok_or_else(|| {
                    std::io::Error::other(
                        "cannot resolve packaged Node runtime or an exact installed Node on PATH",
                    )
                })?
        } else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                "the installed Nopal release has no packaged Node runtime",
            ));
        };
        let path = candidate.canonicalize()?;
        validate_executable_identity_against(&path, TRUSTED_NODE_INTEGRITY)?;
        Ok(Some(path))
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this Nopal release has no locked Node runtime for the current platform",
        ))
    }
}

fn validate_executable_identity_against(
    executable: &std::path::Path,
    expected_integrity: &str,
) -> std::io::Result<()> {
    use sha2::Digest;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(executable)?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "runtime executable must be a canonical regular file",
        ));
    }
    #[cfg(unix)]
    if metadata.permissions().mode() & 0o111 == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "runtime executable is not executable",
        ));
    }
    let integrity = format!(
        "sha256:{:x}",
        sha2::Sha256::digest(std::fs::read(executable)?)
    );
    if integrity != expected_integrity {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!("runtime executable has integrity {integrity}, expected {expected_integrity}"),
        ));
    }
    Ok(())
}

fn pi_process_command(
    pi_bin: &std::path::Path,
    node_bin: Option<&std::path::Path>,
) -> std::process::Command {
    if let Some(node_bin) = node_bin {
        let mut command = std::process::Command::new(node_bin);
        command.arg(pi_bin);
        command
    } else {
        std::process::Command::new(pi_bin)
    }
}

fn validate_pi_package_identity(executable: &std::path::Path) -> std::io::Result<()> {
    validate_pi_package_identity_against(
        executable,
        TRUSTED_PI_VERSION,
        TRUSTED_PI_DIST_INTEGRITY,
    )?;
    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    {
        let package_root = pi_package_root(executable)?;
        let integrity = hash_pi_runtime_tree(&package_root)?;
        if integrity != TRUSTED_PI_RUNTIME_INTEGRITY {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!(
                    "installed Pi runtime closure has integrity {integrity}, expected {TRUSTED_PI_RUNTIME_INTEGRITY}"
                ),
            ));
        }
        Ok(())
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this Nopal release has no locked Pi runtime closure for the current platform",
        ))
    }
}

fn validate_pi_package_identity_against(
    executable: &std::path::Path,
    expected_version: &str,
    expected_dist_integrity: &str,
) -> std::io::Result<()> {
    let mut directory = executable.parent();
    for _ in 0..8 {
        let Some(current) = directory else { break };
        let manifest = current.join("package.json");
        if manifest.is_file() {
            let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest)?)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            if value.get("name").and_then(serde_json::Value::as_str)
                == Some("@earendil-works/pi-coding-agent")
            {
                let bin = value
                    .get("bin")
                    .and_then(|value| value.get("pi"))
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| {
                        std::io::Error::other(
                            "installed Pi package does not declare its canonical pi entrypoint",
                        )
                    })?;
                let declared = current.join(bin).canonicalize()?;
                if declared != executable {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "resolved Pi executable is not the entrypoint declared by the installed Pi package",
                    ));
                }
                let version = value
                    .get("version")
                    .and_then(serde_json::Value::as_str)
                    .filter(|version| !version.is_empty())
                    .ok_or_else(|| {
                        std::io::Error::other("installed Pi package has no exact version identity")
                    })?;
                if version != expected_version {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "installed Pi package version {version} does not match locked version {expected_version}"
                        ),
                    ));
                }
                let dist_integrity = nopal_core::distribution::hash_tree(&current.join("dist"))?;
                if dist_integrity != expected_dist_integrity {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        format!(
                            "installed Pi package tree has integrity {dist_integrity}, expected {expected_dist_integrity}"
                        ),
                    ));
                }
                return Ok(());
            }
        }
        directory = current.parent();
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "resolved Pi executable is not owned by the trusted @earendil-works/pi-coding-agent package",
    ))
}

fn pi_package_root(executable: &std::path::Path) -> std::io::Result<PathBuf> {
    let mut directory = executable.parent();
    for _ in 0..8 {
        let Some(current) = directory else { break };
        let manifest = current.join("package.json");
        if manifest.is_file() {
            let value: serde_json::Value = serde_json::from_slice(&std::fs::read(&manifest)?)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            if value.get("name").and_then(serde_json::Value::as_str)
                == Some("@earendil-works/pi-coding-agent")
            {
                return current.canonicalize();
            }
        }
        directory = current.parent();
    }
    Err(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "resolved Pi executable has no package root",
    ))
}

fn hash_pi_runtime_tree(root: &std::path::Path) -> std::io::Result<String> {
    use sha2::Digest;

    let canonical_root = root.canonicalize()?;
    let mut hasher = sha2::Sha256::new();
    hash_pi_runtime_entry(&canonical_root, &canonical_root, &mut hasher)?;
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn hash_pi_runtime_entry(
    root: &std::path::Path,
    path: &std::path::Path,
    hasher: &mut sha2::Sha256,
) -> std::io::Result<()> {
    use sha2::Digest;
    use std::io::Read;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path)?;
    let relative = path.strip_prefix(root).map_err(std::io::Error::other)?;
    let relative = if relative.as_os_str().is_empty() {
        ".".to_owned()
    } else {
        relative
            .components()
            .map(|component| component.as_os_str().to_str())
            .collect::<Option<Vec<_>>>()
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "Pi runtime tree path is not UTF-8",
                )
            })?
            .join("/")
    };
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(path)?;
        let resolved = path.canonicalize()?;
        if !resolved.starts_with(root) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                format!("Pi runtime symlink {} escapes its package", path.display()),
            ));
        }
        let target = target.to_str().ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Pi runtime symlink target is not UTF-8",
            )
        })?;
        hasher.update(b"link\0");
        hasher.update(relative.as_bytes());
        hasher.update(b"\0");
        hasher.update(target.as_bytes());
        hasher.update(b"\0");
        return Ok(());
    }
    if metadata.is_file() {
        hasher.update(b"file\0");
        hasher.update(relative.as_bytes());
        hasher.update(b"\0");
        #[cfg(unix)]
        hasher.update(if metadata.permissions().mode() & 0o111 == 0 {
            b"-\0"
        } else {
            b"x\0"
        });
        #[cfg(not(unix))]
        hasher.update(b"-\0");
        let mut file = std::fs::File::open(path)?;
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            hasher.update(&buffer[..read]);
        }
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Pi runtime contains unsupported entry {}", path.display()),
        ));
    }
    hasher.update(b"dir\0");
    hasher.update(relative.as_bytes());
    hasher.update(b"\0");
    let mut children = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        hash_pi_runtime_entry(root, &child.path(), hasher)?;
    }
    Ok(())
}

fn snapshot_locked_pi_runtime(
    pi_entrypoint: &std::path::Path,
    node: Option<&std::path::Path>,
    run_dir: &std::path::Path,
) -> std::io::Result<(PathBuf, Option<PathBuf>)> {
    if cfg!(debug_assertions) && std::env::var_os("NOPAL_TEST_PI_BIN").is_some() {
        return Ok((pi_entrypoint.to_owned(), node.map(PathBuf::from)));
    }
    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    {
        use std::os::unix::fs::PermissionsExt;

        let source_root = pi_package_root(pi_entrypoint)?;
        let relative_entrypoint = pi_entrypoint.strip_prefix(&source_root).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Pi entrypoint is outside its runtime closure",
            )
        })?;
        let snapshot_parent = run_dir.join("artifacts/runtime-closures");
        std::fs::create_dir_all(&snapshot_parent)?;
        std::fs::set_permissions(&snapshot_parent, std::fs::Permissions::from_mode(0o700))?;
        let closure_name = TRUSTED_PI_RUNTIME_INTEGRITY
            .strip_prefix("sha256:")
            .ok_or_else(|| std::io::Error::other("invalid locked Pi closure identity"))?;
        let snapshot_root = snapshot_parent.join(closure_name);
        copy_pi_runtime_tree(&source_root, &snapshot_root)?;
        let snapshot_integrity = hash_pi_runtime_tree(&snapshot_root)?;
        if snapshot_integrity != TRUSTED_PI_RUNTIME_INTEGRITY {
            return Err(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "Pi runtime changed while creating its private snapshot",
            ));
        }
        let node = node.ok_or_else(|| std::io::Error::other("locked Node runtime is missing"))?;
        validate_executable_identity_against(node, TRUSTED_NODE_INTEGRITY)?;
        let node_snapshot = snapshot_parent.join("node");
        copy_pi_runtime_tree(node, &node_snapshot)?;
        validate_executable_identity_against(&node_snapshot, TRUSTED_NODE_INTEGRITY)?;
        Ok((snapshot_root.join(relative_entrypoint), Some(node_snapshot)))
    }
    #[cfg(not(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    )))]
    {
        let _ = (pi_entrypoint, node, run_dir);
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "this Nopal release has no snapshot contract for the current platform",
        ))
    }
}

#[cfg(target_os = "macos")]
fn copy_pi_runtime_tree(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let destination_path = destination.to_owned();
    let source = CString::new(source.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Pi runtime source path contains a NUL byte",
        )
    })?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "Pi runtime destination path contains a NUL byte",
        )
    })?;
    let flags = libc::COPYFILE_DATA
        | libc::COPYFILE_STAT
        | libc::COPYFILE_RECURSIVE
        | libc::COPYFILE_EXCL
        | libc::COPYFILE_NOFOLLOW
        | libc::COPYFILE_CLONE;
    // SAFETY: both paths are owned NUL-terminated strings, the state pointer is
    // intentionally null, and copyfile completes before either string is dropped.
    let result = unsafe {
        libc::copyfile(
            source.as_ptr(),
            destination.as_ptr(),
            std::ptr::null_mut(),
            flags,
        )
    };
    if result != 0 {
        return Err(std::io::Error::last_os_error());
    }
    harden_pi_runtime_tree(&destination_path)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn copy_pi_runtime_tree(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    copy_pi_runtime_tree_portable(source, destination)
}

#[cfg(all(unix, not(target_os = "macos")))]
fn copy_pi_runtime_tree_portable(
    source: &std::path::Path,
    destination: &std::path::Path,
) -> std::io::Result<()> {
    use std::os::unix::fs::{OpenOptionsExt, PermissionsExt, symlink};

    let metadata = std::fs::symlink_metadata(source)?;
    if metadata.file_type().is_symlink() {
        let target = std::fs::read_link(source)?;
        symlink(target, destination)?;
        return Ok(());
    }
    if metadata.is_file() {
        let mut source_file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(source)?;
        let executable = metadata.permissions().mode() & 0o111 != 0;
        let mut destination_file = std::fs::OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(if executable { 0o500 } else { 0o400 })
            .open(destination)?;
        std::io::copy(&mut source_file, &mut destination_file)?;
        destination_file.sync_all()?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("Pi runtime contains unsupported entry {}", source.display()),
        ));
    }
    std::fs::create_dir(destination)?;
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o700))?;
    let mut children = std::fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        copy_pi_runtime_tree_portable(&child.path(), &destination.join(child.file_name()))?;
    }
    std::fs::set_permissions(destination, std::fs::Permissions::from_mode(0o500))?;
    Ok(())
}

#[cfg(all(unix, target_os = "macos"))]
fn harden_pi_runtime_tree(path: &std::path::Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let metadata = std::fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Ok(());
    }
    if metadata.is_file() {
        let executable = metadata.permissions().mode() & 0o111 != 0;
        std::fs::set_permissions(
            path,
            std::fs::Permissions::from_mode(if executable { 0o500 } else { 0o400 }),
        )?;
        return Ok(());
    }
    let mut children = std::fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(std::fs::DirEntry::file_name);
    for child in children {
        harden_pi_runtime_tree(&child.path())?;
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o500))?;
    Ok(())
}

#[cfg(not(unix))]
fn copy_pi_runtime_tree(
    _source: &std::path::Path,
    _destination: &std::path::Path,
) -> std::io::Result<()> {
    Err(std::io::Error::other("Pi runtime snapshots require unix"))
}

fn configure_pi_environment(command: &mut std::process::Command, runtime_home: &std::path::Path) {
    const RETAINED: &[&str] = &[
        "ANTHROPIC_API_KEY",
        "AWS_ACCESS_KEY_ID",
        "AWS_DEFAULT_REGION",
        "AWS_REGION",
        "AWS_SECRET_ACCESS_KEY",
        "AWS_SESSION_TOKEN",
        "AZURE_OPENAI_API_KEY",
        "COLORTERM",
        "GEMINI_API_KEY",
        "GITHUB_TOKEN",
        "GOOGLE_API_KEY",
        "LANG",
        "LC_ALL",
        "LC_CTYPE",
        "LOGNAME",
        "OPENAI_API_KEY",
        "OPENROUTER_API_KEY",
        "SSH_AUTH_SOCK",
        "TERM",
        "TZ",
        "USER",
        "XAI_API_KEY",
    ];
    let retained = RETAINED
        .iter()
        .filter_map(|name| std::env::var_os(name).map(|value| ((*name).to_owned(), value)))
        .collect::<Vec<_>>();
    #[cfg(debug_assertions)]
    let test_only = std::env::vars_os()
        .filter(|(name, _)| {
            name.to_str()
                .is_some_and(|name| name.starts_with("PROOF_") || matches!(name, "AUTHORITY_FILE"))
        })
        .collect::<Vec<_>>();
    command.env_clear();
    command.envs(retained);
    #[cfg(debug_assertions)]
    command.envs(test_only);
    #[cfg(debug_assertions)]
    let trusted_path = std::env::var_os("PATH")
        .unwrap_or_else(|| "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin".into());
    #[cfg(not(debug_assertions))]
    let trusted_path = "/usr/bin:/bin";
    command
        .env("PATH", trusted_path)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("TMPDIR", "/tmp")
        .env("HOME", runtime_home)
        .env("CURL_HOME", runtime_home)
        .env("XDG_CONFIG_HOME", runtime_home.join("config"))
        .env("CARGO_HOME", runtime_home.join("cargo"))
        .env("COMPOSER_HOME", runtime_home.join("composer"))
        .env("KUBECONFIG", runtime_home.join("kubeconfig"))
        .env("PIP_CONFIG_FILE", "/dev/null")
        .env("NPM_CONFIG_USERCONFIG", runtime_home.join("npm-userconfig"))
        .env(
            "NPM_CONFIG_GLOBALCONFIG",
            runtime_home.join("npm-globalconfig"),
        );
}

fn probe_pi_runtime(
    pi_bin: &std::path::Path,
    node_bin: Option<&std::path::Path>,
    dir: &std::path::Path,
    argv: &[String],
    enforcement: &EnforcementLaunchEnv<'_>,
    run_dir: &std::path::Path,
) -> std::io::Result<()> {
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    let acknowledgement = run_dir.join("artifacts/enforcement/runtime-ack");
    let token = enforcement::generate_receipt_key()?;
    let mut command = pi_process_command(pi_bin, node_bin);
    configure_pi_environment(&mut command, &enforcement.pi_runtime_dir.join("home"));
    command
        .args(argv)
        .args([
            "--mode",
            "json",
            "--print",
            "--no-session",
            "nopal enforcement runtime probe",
        ])
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .env("PI_SKIP_VERSION_CHECK", "1")
        .env("PI_OFFLINE", "1")
        .env("PI_CODING_AGENT_DIR", enforcement.pi_runtime_dir)
        .env("NOPAL_ENFORCEMENT_RUN_ID", enforcement.run_id)
        .env("NOPAL_ENFORCEMENT_ROOT", enforcement.root)
        .env("NOPAL_ENFORCEMENT_STATE_DIR", enforcement.state_dir)
        .env("NOPAL_ENFORCEMENT_ADAPTER_DIR", enforcement.adapter_dir)
        .env("NOPAL_ENFORCEMENT_CLI", enforcement.cli)
        .env(
            "NOPAL_ENFORCEMENT_CAPABILITY_FD",
            enforcement.capability_fd.to_string(),
        )
        .env("NOPAL_POLICY_MODE", enforcement.policy_mode)
        .env("NOPAL_GATE_EXECUTOR_BIN", enforcement.gate_executor_bin)
        .env("NOPAL_GATE_HOME", enforcement.gate_home)
        .env(
            "NOPAL_GATE_EXECUTOR_DIGEST",
            enforcement.gate_executor_digest,
        )
        .env("NOPAL_GATE_RUNTIME_DIGEST", enforcement.gate_runtime_digest)
        .env("NOPAL_ENFORCEMENT_PROBE", "1")
        .env("NOPAL_ENFORCEMENT_PROBE_ACK", &acknowledgement)
        .env("NOPAL_ENFORCEMENT_PROBE_TOKEN", &token);
    if let Some(config_dir) = enforcement.config_dir {
        command.env("NOPAL_ENFORCEMENT_CONFIG_DIR", config_dir);
    }
    let mut child = command.spawn().map_err(|error| {
        std::io::Error::new(
            error.kind(),
            format!(
                "failed to start Pi enforcement capability probe {}: {error}",
                pi_bin.display()
            ),
        )
    })?;
    let deadline = Instant::now() + Duration::from_secs(10);
    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "Pi did not acknowledge the enforcement hook within 10 seconds",
            ));
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    if !status.success() {
        return Err(std::io::Error::other(format!(
            "Pi enforcement capability probe exited with {status}"
        )));
    }
    let observed = std::fs::read_to_string(&acknowledgement).map_err(|error| {
        std::io::Error::other(format!(
            "Pi exited without a readable enforcement acknowledgement: {error}"
        ))
    })?;
    let _ = std::fs::remove_file(&acknowledgement);
    if !enforcement::capability_matches(observed.as_bytes(), token.as_bytes()) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "Pi enforcement acknowledgement did not match this launch",
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn exec_pi(
    pi_bin: &std::path::Path,
    node_bin: Option<&std::path::Path>,
    dir: &std::path::Path,
    argv: &[String],
    enforcement: &EnforcementLaunchEnv<'_>,
) -> std::io::Result<ExitCode> {
    use std::os::unix::process::CommandExt;
    let mut command = pi_process_command(pi_bin, node_bin);
    configure_pi_environment(&mut command, &enforcement.pi_runtime_dir.join("home"));
    command
        .args(argv)
        .current_dir(dir)
        // Pi's own update-check network call and banner are noise nopal
        // already owns readiness reporting for; skip it every launch.
        .env("PI_SKIP_VERSION_CHECK", "1")
        .env("PI_OFFLINE", "1")
        .env("PI_CODING_AGENT_DIR", enforcement.pi_runtime_dir)
        .env("NOPAL_ENFORCEMENT_RUN_ID", enforcement.run_id)
        .env("NOPAL_ENFORCEMENT_ROOT", enforcement.root)
        .env("NOPAL_ENFORCEMENT_STATE_DIR", enforcement.state_dir)
        .env("NOPAL_ENFORCEMENT_ADAPTER_DIR", enforcement.adapter_dir)
        .env("NOPAL_ENFORCEMENT_CLI", enforcement.cli)
        .env(
            "NOPAL_ENFORCEMENT_CAPABILITY_FD",
            enforcement.capability_fd.to_string(),
        )
        .env("NOPAL_POLICY_MODE", enforcement.policy_mode)
        .env("NOPAL_GATE_EXECUTOR_BIN", enforcement.gate_executor_bin)
        .env("NOPAL_GATE_HOME", enforcement.gate_home)
        .env(
            "NOPAL_GATE_EXECUTOR_DIGEST",
            enforcement.gate_executor_digest,
        )
        .env("NOPAL_GATE_RUNTIME_DIGEST", enforcement.gate_runtime_digest);
    if let Some(config_dir) = enforcement.config_dir {
        command.env("NOPAL_ENFORCEMENT_CONFIG_DIR", config_dir);
    }
    let err = command.exec();
    // `exec` only returns on failure; success replaces this process image.
    Err(std::io::Error::new(
        err.kind(),
        format!("failed to exec {}: {err}", pi_bin.display()),
    ))
}

#[cfg(not(unix))]
fn exec_pi(
    _pi_bin: &std::path::Path,
    _node_bin: Option<&std::path::Path>,
    _dir: &std::path::Path,
    _argv: &[String],
    _enforcement: &EnforcementLaunchEnv<'_>,
) -> std::io::Result<ExitCode> {
    Err(std::io::Error::other(
        "nopal cli requires a unix platform; there is no non-unix spawn fallback (D7)",
    ))
}

fn removed_surface(cli: &Cli, surface: &'static str) -> std::io::Result<ExitCode> {
    let migration = match surface {
        "field" => {
            "Field, tmux seats, and desktop Sessions were removed in v0.3; use bare `nopal` to launch the enforced Pi distribution"
        }
        _ => {
            "this agent-management surface was removed in v0.3; use bare `nopal` and the documented deterministic enforcement commands"
        }
    };
    let report = MigrationReport {
        kind: "nopal.migration/v1",
        ok: false,
        code: "product_surface_removed",
        surface,
        removed_in: "0.3.0",
        migration,
    };
    print_report_and_exit(
        false,
        cli.json,
        || serde_json::to_string_pretty(&report),
        || {
            format!(
                "kind: {}\nok: false\ncode: {}\nsurface: {}\nremoved_in: {}\nmigration: {:?}\n",
                report.kind, report.code, report.surface, report.removed_in, report.migration
            )
        },
    )
}

fn print_report_and_exit(
    ok: bool,
    json: bool,
    json_output: impl FnOnce() -> serde_json::Result<String>,
    toon_output: impl FnOnce() -> String,
) -> std::io::Result<ExitCode> {
    if json {
        let text = json_output().map_err(std::io::Error::other)?;
        println!("{text}");
    } else {
        print!("{}", toon_output());
    }
    Ok(exit_for(ok))
}

fn exit_for(ok: bool) -> ExitCode {
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        configure_pi_environment, copy_pi_runtime_tree, hash_pi_runtime_tree,
        packaged_distribution_candidates, packaged_node_binary, packaged_pi_binary,
        prepare_pi_runtime_dir_from, resolve_builtin_distribution_root_from,
        validate_executable_identity_against, validate_pi_package_identity_against,
        validate_project_pi_settings, verify_enforcement_adapter,
    };

    #[test]
    fn pi_runtime_ignores_system_git_configuration() {
        let temp = tempfile::tempdir().unwrap();
        let mut command = std::process::Command::new("unused-test-program");
        configure_pi_environment(&mut command, temp.path());
        let clean_system_config = command
            .get_envs()
            .find(|(name, _)| *name == "GIT_CONFIG_NOSYSTEM")
            .and_then(|(_, value)| value);
        assert_eq!(clean_system_config, Some(std::ffi::OsStr::new("1")));
    }

    #[test]
    #[cfg(any(
        all(target_os = "macos", target_arch = "aarch64"),
        all(target_os = "macos", target_arch = "x86_64"),
        all(target_os = "linux", target_arch = "x86_64")
    ))]
    fn trusted_runtime_profile_is_complete_for_this_release_target() {
        assert!(super::TRUSTED_PI_RUNTIME_INTEGRITY.starts_with("sha256:"));
        assert_eq!(super::TRUSTED_PI_RUNTIME_INTEGRITY.len(), 71);
        assert!(super::TRUSTED_NODE_INTEGRITY.starts_with("sha256:"));
        assert_eq!(super::TRUSTED_NODE_INTEGRITY.len(), 71);
    }

    #[test]
    fn source_free_archive_resolves_the_complete_builtin_distribution() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("nopal-v0.3.0/nopal");
        let distribution = temp.path().join("nopal-v0.3.0/share/nopal");
        let adapter = distribution.join("extensions/policy-gate");
        let beislid = distribution.join("resources/beislid");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::create_dir_all(&adapter).unwrap();
        fs::create_dir_all(beislid.join("skills/kickoff")).unwrap();
        fs::write(&executable, b"standalone nopal binary\n").unwrap();
        fs::write(beislid.join("LICENSE"), "MIT\n").unwrap();
        fs::write(beislid.join("provenance.json"), "{}\n").unwrap();
        fs::write(beislid.join("skills/kickoff/SKILL.md"), "# Kickoff\n").unwrap();
        for (name, bytes) in [
            (
                "index.ts",
                include_bytes!("../../../extensions/policy-gate/index.ts").as_slice(),
            ),
            (
                "classifier.ts",
                include_bytes!("../../../extensions/policy-gate/classifier.ts").as_slice(),
            ),
            (
                "guard.ts",
                include_bytes!("../../../extensions/policy-gate/guard.ts").as_slice(),
            ),
            (
                "nopal-cli.ts",
                include_bytes!("../../../extensions/policy-gate/nopal-cli.ts").as_slice(),
            ),
        ] {
            fs::write(adapter.join(name), bytes).unwrap();
        }

        let resolved =
            resolve_builtin_distribution_root_from(packaged_distribution_candidates(&executable))
                .unwrap();
        assert_eq!(resolved, distribution.canonicalize().unwrap());
        verify_enforcement_adapter(&resolved.join("extensions/policy-gate/index.ts")).unwrap();

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            fs::create_dir(temp.path().join("bin")).unwrap();
            let launcher = temp.path().join("bin/nopal");
            symlink("../nopal-v0.3.0/nopal", &launcher).unwrap();
            let linked =
                resolve_builtin_distribution_root_from(packaged_distribution_candidates(&launcher))
                    .unwrap();
            assert_eq!(linked, distribution.canonicalize().unwrap());
        }
    }

    #[test]
    fn release_relative_runtime_candidates_do_not_depend_on_path() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("nopal-v0.3.0/nopal");
        let pi = temp.path().join("nopal-v0.3.0/runtime/pi/dist/cli.js");
        let node = temp.path().join("nopal-v0.3.0/runtime/node");
        fs::create_dir_all(pi.parent().unwrap()).unwrap();
        fs::write(&executable, "nopal\n").unwrap();
        fs::write(&pi, "pi\n").unwrap();
        fs::write(&node, "node\n").unwrap();

        assert_eq!(packaged_pi_binary(&executable), Some(pi.clone()));
        assert_eq!(packaged_node_binary(&executable), Some(node.clone()));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            fs::create_dir(temp.path().join("bin")).unwrap();
            let launcher = temp.path().join("bin/nopal");
            symlink("../nopal-v0.3.0/nopal", &launcher).unwrap();
            assert_eq!(
                packaged_pi_binary(&launcher),
                Some(pi.canonicalize().unwrap())
            );
            assert_eq!(
                packaged_node_binary(&launcher),
                Some(node.canonicalize().unwrap())
            );
        }
    }

    #[test]
    #[cfg(unix)]
    fn executable_project_pi_settings_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".pi")).unwrap();
        fs::write(
            temp.path().join(".pi/settings.json"),
            r#"{ // executable carrier
 "shellCommandPrefix": "./attack &&"
}"#,
        )
        .unwrap();

        let error = validate_project_pi_settings(temp.path()).unwrap_err();
        assert!(error.to_string().contains("executable authority"));
    }

    #[test]
    #[cfg(unix)]
    fn symlinked_project_pi_settings_directory_fails_closed() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let external = temp.path().join("external");
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("settings.json"), "{}").unwrap();
        symlink(&external, temp.path().join(".pi")).unwrap();

        let error = validate_project_pi_settings(temp.path()).unwrap_err();
        assert!(error.to_string().contains("real directory"));
    }

    #[test]
    #[cfg(unix)]
    fn hardlinked_project_pi_settings_fail_closed() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir_all(temp.path().join(".pi")).unwrap();
        let settings = temp.path().join(".pi/settings.json");
        fs::write(&settings, "{}").unwrap();
        fs::hard_link(&settings, temp.path().join("settings-alias.json")).unwrap();

        let error = validate_project_pi_settings(temp.path()).unwrap_err();
        assert!(error.to_string().contains("hardlink aliases"));
    }

    #[test]
    #[cfg(unix)]
    fn isolated_pi_runtime_copies_only_bounded_authentication_state() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let run = temp.path().join("run");
        fs::create_dir_all(&source).unwrap();
        fs::write(
            source.join("auth.json"),
            r#"{"provider":{"token":"secret"}}"#,
        )
        .unwrap();
        fs::write(
            source.join("settings.json"),
            r#"{"shellPath":"/tmp/attack"}"#,
        )
        .unwrap();

        let runtime = prepare_pi_runtime_dir_from(&run, Some(&source)).unwrap();
        assert!(runtime.join("auth.json").is_file());
        assert!(runtime.join("home").is_dir());
        for name in [
            ".gitconfig",
            "kubeconfig",
            "npm-globalconfig",
            "npm-userconfig",
        ] {
            assert_eq!(
                fs::metadata(runtime.join("home").join(name)).unwrap().len(),
                0
            );
        }
        assert!(!runtime.join("settings.json").exists());
        assert_eq!(
            fs::metadata(&runtime).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(runtime.join("auth.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(runtime.join("home"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    #[cfg(unix)]
    fn runtime_executable_substitution_fails_identity_validation() {
        use sha2::Digest;
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("node");
        fs::write(&executable, "trusted runtime").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        let expected = format!(
            "sha256:{:x}",
            sha2::Sha256::digest(fs::read(&executable).unwrap())
        );
        validate_executable_identity_against(&executable, &expected).unwrap();

        fs::write(&executable, "substituted runtime").unwrap();
        assert!(validate_executable_identity_against(&executable, &expected).is_err());
    }

    #[test]
    #[cfg(unix)]
    fn pi_runtime_closure_hash_binds_dependencies_symlinks_and_executable_modes() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("pi");
        fs::create_dir_all(root.join("node_modules/dependency/bin")).unwrap();
        fs::create_dir_all(root.join("node_modules/.bin")).unwrap();
        let executable = root.join("node_modules/dependency/bin/tool.js");
        fs::write(&executable, "console.log('trusted')\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(
            "../dependency/bin/tool.js",
            root.join("node_modules/.bin/tool"),
        )
        .unwrap();
        let before = hash_pi_runtime_tree(&root).unwrap();
        let snapshot = temp.path().join("snapshot");
        copy_pi_runtime_tree(&root, &snapshot).unwrap();
        assert_eq!(before, hash_pi_runtime_tree(&snapshot).unwrap());
        assert_eq!(
            fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
            0o500
        );

        fs::write(&executable, "console.log('substituted')\n").unwrap();
        assert_ne!(before, hash_pi_runtime_tree(&root).unwrap());
        fs::write(&executable, "console.log('trusted')\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o644)).unwrap();
        assert_ne!(before, hash_pi_runtime_tree(&root).unwrap());
    }

    #[test]
    fn pi_package_identity_requires_the_declared_versioned_entrypoint() {
        let temp = tempfile::tempdir().unwrap();
        let package = temp
            .path()
            .join("node_modules/@earendil-works/pi-coding-agent");
        let executable = package.join("dist/cli.js");
        fs::create_dir_all(executable.parent().unwrap()).unwrap();
        fs::write(&executable, "#!/usr/bin/env node\n").unwrap();
        let manifest = package.join("package.json");

        fs::write(
            &manifest,
            r#"{"name":"@earendil-works/pi-coding-agent","version":"1.2.3","bin":{"pi":"dist/cli.js"}}"#,
        )
        .unwrap();
        let expected_integrity =
            nopal_core::distribution::hash_tree(&package.join("dist")).unwrap();
        let validate = || {
            validate_pi_package_identity_against(
                &executable.canonicalize().unwrap(),
                "1.2.3",
                &expected_integrity,
            )
        };
        validate().unwrap();
        fs::write(&executable, "#!/usr/bin/env node\n// substituted\n").unwrap();
        assert!(
            validate()
                .unwrap_err()
                .to_string()
                .contains("package tree has integrity")
        );
        fs::write(&executable, "#!/usr/bin/env node\n").unwrap();

        fs::write(
            &manifest,
            r#"{"name":"substituted-pi","version":"1.2.3","bin":{"pi":"dist/cli.js"}}"#,
        )
        .unwrap();
        assert!(
            validate()
                .unwrap_err()
                .to_string()
                .contains("not owned by the trusted")
        );

        fs::write(
            &manifest,
            r#"{"name":"@earendil-works/pi-coding-agent","version":"1.2.3","bin":{"pi":"dist/other.js"}}"#,
        )
        .unwrap();
        fs::write(package.join("dist/other.js"), "#!/usr/bin/env node\n").unwrap();
        assert!(
            validate()
                .unwrap_err()
                .to_string()
                .contains("not the entrypoint declared")
        );

        fs::write(
            &manifest,
            r#"{"name":"@earendil-works/pi-coding-agent","bin":{"pi":"dist/cli.js"}}"#,
        )
        .unwrap();
        assert!(
            validate()
                .unwrap_err()
                .to_string()
                .contains("no exact version identity")
        );
    }
}
