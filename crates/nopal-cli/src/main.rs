//! `nopal` - deterministic AXI-style CLI and coordinator over nopal-core.
//!
//! Exit codes: 0 = success / ready enough to report, 1 = validation found
//! errors (for policy commands: the policy module is missing or invalid;
//! for ledger commands: a domain problem such as a missing run), 2 = usage
//! or IO failure. Policy verdicts live in the payload, never in the exit
//! code: nopal decides and explains, it does not gate.
//!
//! Cold commands (validate, gates, preflights, policy, info) never contact
//! agents or external services. Commands that consume a project root resolve
//! it through `discover::project_root`, which probes Git once to find the
//! enclosing repository. Bare invocation is deliberately warm: it validates
//! the launch and effective enforcement contracts, initializes a Workflow Run
//! Ledger entry, and replaces itself with Pi. The hidden Field and `nopal cli`
//! routes remain temporary implementation residue until the v0.3 removal
//! slice; they are not public launch surfaces.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use nopal_core::ask::{AskState, Resolution};
use nopal_core::ask_report as ask;
use nopal_core::ask_store::RaiseArgs;
use nopal_core::beislid_import::{self, ImportOptions};
use nopal_core::discover;
use nopal_core::enforcement;
use nopal_core::process_artifact;
use nopal_core::run_ledger as ledger_core;
use nopal_core::run_ledger_report as ledger;
use nopal_core::run_ledger_store::InitArgs;
use nopal_core::scaffold;
use nopal_core::{gates::GateStage, policy};

mod coordinator;
mod herdr_bridge;
mod info;
mod launch;

#[derive(Parser)]
#[command(
    name = "nopal",
    version,
    about = "Deterministic process/config core and Rondo Core coordinator"
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
    /// Deprecated internal spelling for the canonical bare launch
    #[command(hide = true)]
    Cli(CliLaunchArgs),
    /// Validate the nopal.project/v1 manifest and profile-required modules
    Validate,
    /// Inspect nopal.gates/v1 preflights
    Preflights {
        #[command(subcommand)]
        command: PreflightsCmd,
    },
    /// Inspect and select nopal.gates/v1 gates
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
    Enforcement {
        #[command(subcommand)]
        command: EnforcementCmd,
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
    /// Persist and resolve `ask` policy decisions cross-session (nopal.ask/v1)
    Ask {
        /// Ask state root; beats BEISLID_STATE_DIR and the XDG default
        #[arg(long)]
        state_dir: Option<PathBuf>,

        #[command(subcommand)]
        command: AskCmd,
    },
    /// Establish a provisional Plot from an authoritative repository snapshot
    Plot {
        #[command(subcommand)]
        command: PlotCmd,
    },
    /// Bridge Nopal's versioned coordination feeds into external hosts
    Bridge {
        #[command(subcommand)]
        command: BridgeCmd,
    },
    /// Review-risk seam: risk class, fast-path eligibility, and split verdict
    /// from changed files/stats/thresholds (nopal.review_risk/v1)
    ReviewRisk(ReviewRiskArgs),
    /// Legacy management surface retained only until the removal slice
    #[command(hide = true)]
    Field(nopal_field::cli::FieldArgs),
    /// Show Nopal readiness and missing modules through the Nopal product surface
    Status,
    /// Machine-readable version + capability report (nopal.info/v1)
    Info,
    /// Explain the runtime placement Nopal would use for an action
    Placement(CoordinatorPlacementArgs),
    /// Manage the local execution-contract-shaped Rondo Core service stub
    Rondo {
        #[command(subcommand)]
        command: RondoCmd,
    },
    /// Start or preview Nopal run coordination
    Run {
        #[command(subcommand)]
        command: RunCmd,
    },
    /// Inspect nopal.workflow/v1 handoff/babysit config
    Workflow {
        #[command(subcommand)]
        command: WorkflowCmd,
    },
    #[command(name = "__rondo-host", hide = true)]
    RondoHost(RondoHostArgs),
}

#[derive(clap::Args)]
struct RondoHostArgs {
    #[arg(long)]
    state_root: PathBuf,
    #[arg(long)]
    rondo_runtime: PathBuf,
}

#[derive(clap::Args)]
struct CliLaunchArgs {
    /// Print the nopal.launch/v1 plan without exec-ing Pi
    #[arg(long)]
    dry_run: bool,

    /// Layer the pinned bundle on top of ambient Pi resources
    #[arg(long)]
    with_ambient: bool,

    /// Print the nopal.launch/v1 stderr summary before exec-ing Pi
    #[arg(long)]
    verbose: bool,
}

#[derive(clap::Args)]
struct CoordinatorPlacementArgs {
    /// Nopal policy mode to evaluate
    #[arg(long, value_parser = parse_mode, default_value = "nopal_tui")]
    mode: policy::Mode,

    /// Stable action id
    #[arg(long, default_value = "run.start")]
    action: String,

    /// Nopal action classes declared by the caller
    #[arg(long = "class", value_parser = parse_class)]
    classes: Vec<policy::ActionClass>,
}

#[derive(Subcommand)]
enum RondoCmd {
    /// Start the verified user-scoped Rondo Core
    Start(RondoPlacementArgs),
    /// Check verified user-scoped Rondo Core health
    Health,
    /// Restart the verified Core when it has no active runs
    Restart(RondoPlacementArgs),
    /// Stop the verified Core when it has no active runs
    Stop,
}

#[derive(Subcommand)]
enum BridgeCmd {
    /// Push live Nopal run/gate/ask state into herdr's native sidebar
    Herdr(HerdrBridgeArgs),
}

#[derive(clap::Args)]
struct HerdrBridgeArgs {
    /// Herdr Unix socket; beats HERDR_SOCKET_PATH and the default config path
    #[arg(long)]
    socket: Option<PathBuf>,

    /// State root to scan; beats BEISLID_STATE_DIR and the XDG default
    #[arg(long)]
    state_dir: Option<PathBuf>,

    /// Seconds between feed polls in daemon mode
    #[arg(long, default_value_t = 5, value_parser = clap::value_parser!(u64).range(1..))]
    interval: u64,

    /// Poll once and exit; a missing herdr socket is a successful no-op
    #[arg(long)]
    once: bool,
}

#[derive(clap::Args)]
struct RondoPlacementArgs {
    /// Optional stricter placement request; Nopal's run-start placement still wins if stronger
    #[arg(long, value_parser = parse_placement)]
    placement: Option<policy::Placement>,
}

#[derive(Subcommand)]
enum RunCmd {
    /// Preview run start coordination without submitting AFK work
    Start {
        /// Keep run-start side-effect-free; this command never submits work
        #[arg(long, default_value_t = true)]
        dry_run: bool,
    },
    /// Submit one approved execution manifest to the configured Rondo Core
    Submit {
        /// Approved per-slice manifest inside the discovered repository
        #[arg(long)]
        manifest: PathBuf,
        /// Established Plot identity; defaults to the current tmux pane's @nopal_plot tag
        #[arg(long)]
        plot_id: Option<String>,
        /// Plot state root; defaults to NOPAL_STATE_DIR and then the user state directory
        #[arg(long)]
        state_dir: Option<PathBuf>,
    },
    /// Fetch one bounded status and event page from Rondo Core
    Observe {
        /// Opaque repository identifier returned by submission
        #[arg(long)]
        repo_id: String,
        /// Plot identifier returned by submission
        #[arg(long)]
        plot_id: String,
        /// Opaque Rondo run identifier returned by submission
        #[arg(long)]
        run_id: String,
        /// Plot state root; defaults to NOPAL_STATE_DIR and then the user state directory
        #[arg(long)]
        state_dir: Option<PathBuf>,
        /// Opaque event cursor returned by an earlier submission or observation
        #[arg(long)]
        cursor: Option<String>,
    },
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
    },
    /// Show the most recently updated matching run
    Resume {
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
        /// it is selected; matches `nopal field inspect --stale-after`
        #[arg(long, default_value_t = nopal_core::field::DEFAULT_STALE_AFTER_HOURS)]
        stale_after: u64,

        /// Finalize each selected run as `interrupted` instead of only
        /// listing it (default: dry run)
        #[arg(long)]
        apply: bool,
    },
}

#[derive(Subcommand)]
enum AskCmd {
    /// Persist a new pending ask with the context needed to decide it
    Raise {
        /// Session or run raising the ask (who is blocked)
        #[arg(long)]
        session: String,
        /// Backing run id; when set, ask lifecycle events land in that ledger
        #[arg(long)]
        run_id: Option<String>,
        /// Ledger flow of the backing run (disambiguates the run)
        #[arg(long)]
        flow: Option<String>,
        /// Policy mode that produced the ask
        #[arg(long)]
        mode: String,
        /// Stable action id the ask is gating
        #[arg(long)]
        action: String,
        /// Policy rule id that set the ask (evidence pointer)
        #[arg(long)]
        rule: Option<String>,
        /// Declared action class (repeatable)
        #[arg(long = "class", value_name = "CLASS")]
        classes: Vec<String>,
        /// Human-readable reason/context (redacted like ledger events)
        #[arg(long)]
        reason: String,
        /// Evidence pointer (path/url/run ref), redacted
        #[arg(long)]
        evidence: Option<String>,
        /// Seconds until the ask expires to deny; 0 disables auto-expiry
        #[arg(long, default_value_t = nopal_core::ask::DEFAULT_TTL_SECONDS)]
        ttl_seconds: i64,
    },
    /// List this repo's asks (pending only by default)
    List {
        /// Filter to one state
        #[arg(long, value_parser = parse_ask_state)]
        state: Option<AskState>,
        /// Show asks in every state
        #[arg(long, conflicts_with = "state")]
        all: bool,
    },
    /// Show one ask in full
    Show {
        #[arg(long)]
        id: String,
    },
    /// Resolve a pending ask (approve or deny); expiry/double-resolve fail closed
    Resolve {
        #[arg(long)]
        id: String,
        /// approve unblocks the caller; deny fails it closed
        #[arg(long, value_parser = parse_resolution)]
        decision: Resolution,
        /// Who resolved it (recorded, redacted)
        #[arg(long)]
        by: String,
        /// Optional note (redacted)
        #[arg(long)]
        note: Option<String>,
    },
    /// Poll until an ask resolves; exit 0 approved, 3 denied/expired, 4 pending
    Await {
        #[arg(long)]
        id: String,
        /// Max seconds to wait; 0 checks once and returns
        #[arg(long, default_value_t = 0)]
        timeout_seconds: u64,
        /// Poll interval in milliseconds
        #[arg(long, default_value_t = 500)]
        poll_ms: u64,
    },
}

#[derive(Subcommand)]
enum PreflightsCmd {
    /// List declared preflights with their stages and commands
    List,
}

#[derive(Subcommand)]
enum WorkflowCmd {
    /// Show the effective handoff/babysit config, defaults applied
    Show,
}

#[derive(Subcommand)]
enum PlotCmd {
    /// Freeze the primary Workflow and bind one Session to one Workspace
    Establish(PlotEstablishArgs),
}

#[derive(clap::Args)]
struct PlotEstablishArgs {
    /// Plot state root; beats BEISLID_STATE_DIR and the XDG default
    #[arg(long)]
    state_dir: Option<PathBuf>,

    /// Plot identity; defaults to the selected Plot for --field-session
    #[arg(long)]
    plot_id: Option<String>,

    /// Field session used when resolving the selected Plot
    #[arg(long, default_value = "nopal")]
    field_session: String,

    /// Configured establishment event that opened this boundary
    #[arg(long)]
    event: String,

    /// Workspace whose repository/configuration Nopal snapshots
    #[arg(long, default_value = ".")]
    workspace: PathBuf,

    /// Owning tmux session; defaults to the session containing TMUX_PANE
    #[arg(long)]
    host_session: Option<String>,

    /// Owning tmux pane; defaults to TMUX_PANE
    #[arg(long)]
    host_pane: Option<String>,

    /// Structured Session protocol Unix socket address
    #[arg(long)]
    protocol_address: Option<String>,

    /// Structured Session endpoint capability kind; defaults to the current durable capability
    #[arg(long, requires = "protocol_address")]
    protocol_kind: Option<String>,

    /// Structured Session protocol readiness state; defaults to ready when an address is supplied
    #[arg(long, requires = "protocol_address")]
    protocol_state: Option<String>,
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
    /// Decide an action and return every missing or stale required gate
    Plan(EnforcementArgs),
    /// Record one gate command executed by the trusted Pi adapter
    RecordGate(RecordGateArgs),
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

fn parse_placement(s: &str) -> Result<policy::Placement, String> {
    policy::Placement::parse(s).ok_or_else(|| {
        "unknown placement; expected one of \"shared_user_runtime\", \"dedicated_repo_runtime\", \"dedicated_run_runtime\", \"blocked\"".to_owned()
    })
}

fn parse_ask_state(s: &str) -> Result<AskState, String> {
    AskState::parse(s).ok_or_else(|| {
        format!("unknown ask state {s:?}; expected pending, approved, denied, or expired")
    })
}

fn parse_resolution(s: &str) -> Result<Resolution, String> {
    Resolution::parse(s)
        .ok_or_else(|| format!("unknown decision {s:?}; expected \"approve\" or \"deny\""))
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
    // means "project root" reads from `root`, never the raw `cli.dir`
    // Lazy because discovery spawns a `git rev-parse` subprocess;
    // arms that never touch the root (bare invocation, field, info) must
    // not pay for - or be observable through - a git probe they don't use.
    // `exec_pi` is the one deliberate exception to root-consumption - see
    // its doc comment.
    let root = std::cell::LazyCell::new(|| discover::project_root(&cli.dir));
    match &cli.command {
        None => dispatch_launch(cli, &root, cli.dry_run, cli.with_ambient, cli.verbose),
        Some(Cmd::Cli(args)) => {
            dispatch_launch(cli, &root, args.dry_run, args.with_ambient, args.verbose)
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
        Some(Cmd::Ask { state_dir, command }) => {
            run_ask_cmd(cli, &root, state_dir.as_deref(), command)
        }
        Some(Cmd::Plot { command }) => run_plot_cmd(cli, command),
        Some(Cmd::Field(args))
            if matches!(args.command, Some(nopal_field::cli::FieldCmd::Inspect(_))) =>
        {
            let Some(nopal_field::cli::FieldCmd::Inspect(inspect)) = &args.command else {
                unreachable!()
            };
            let report = nopal_core::field_store::field_status(
                &root,
                inspect.state_dir.as_deref(),
                inspect.rondo_events.as_deref(),
                inspect.all,
                inspect.stale_after,
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || nopal_core::field::report_toon(&report),
            )
        }
        Some(Cmd::Bridge {
            command: BridgeCmd::Herdr(args),
        }) => herdr_bridge::run(&herdr_bridge::Options {
            dir: cli.dir.clone(),
            socket: args.socket.clone(),
            state_dir: args.state_dir.clone(),
            interval: std::time::Duration::from_secs(args.interval),
            once: args.once,
        }),
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
        Some(Cmd::Field(args)) => {
            if args.command.is_none()
                || matches!(args.command, Some(nopal_field::cli::FieldCmd::Legacy))
            {
                use std::io::IsTerminal;
                if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
                    warn_for_interactive_rondo(&root);
                }
            }
            nopal_field::cli::run(args)
        }
        Some(Cmd::Status) => {
            let report = coordinator::status(&root)?;
            print_report(cli.json, &report, coordinator::status_toon)?;
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
        Some(Cmd::Placement(args)) => {
            let classes = if args.classes.is_empty() {
                vec![policy::ActionClass::WorkspaceWrite]
            } else {
                args.classes.clone()
            };
            let report = coordinator::placement(&root, args.mode, &args.action, &classes)?;
            let ok = report.ok;
            print_report(cli.json, &report, coordinator::placement_toon)?;
            Ok(if ok {
                ExitCode::SUCCESS
            } else {
                ExitCode::from(1)
            })
        }
        Some(Cmd::Rondo { command }) => {
            let report = match command {
                RondoCmd::Start(args) => coordinator::rondo_start(&root, args.placement)?,
                RondoCmd::Health => coordinator::rondo_health(&root)?,
                RondoCmd::Restart(args) => coordinator::rondo_restart(&root, args.placement)?,
                RondoCmd::Stop => coordinator::rondo_stop(&root)?,
            };
            print_report(cli.json, &report, coordinator::rondo_service_toon)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Cmd::RondoHost(args)) => {
            nopal_rondo_lifecycle::run_host(&nopal_rondo_lifecycle::HostOptions::new(
                nopal_rondo_lifecycle::StatePaths::new(args.state_root.clone()),
                args.rondo_runtime.clone(),
            ))?;
            Ok(ExitCode::SUCCESS)
        }
        Some(Cmd::Run { command }) => match command {
            RunCmd::Start { dry_run } => {
                if !dry_run {
                    eprintln!(
                        "nopal: `run start` is a dry-run preview; use `nopal run submit --manifest <path>` to submit approved work"
                    );
                    return Ok(ExitCode::from(2));
                }
                let report = coordinator::run_start_dry_run(&root)?;
                print_report(cli.json, &report, coordinator::run_start_dry_run_toon)?;
                Ok(ExitCode::SUCCESS)
            }
            RunCmd::Submit {
                manifest,
                plot_id,
                state_dir,
            } => {
                let plot_id = match resolve_run_plot_id(plot_id.as_deref()) {
                    Ok(plot_id) => plot_id,
                    Err(message) => {
                        eprintln!("nopal: {message}");
                        return Ok(ExitCode::from(2));
                    }
                };
                let report =
                    coordinator::run_submit(&root, manifest, &plot_id, state_dir.as_deref());
                print_report_and_exit(
                    report.ok,
                    cli.json,
                    || serde_json::to_string_pretty(&report),
                    || coordinator::run_submit_toon(&report),
                )
            }
            RunCmd::Observe {
                repo_id,
                plot_id,
                run_id,
                state_dir,
                cursor,
            } => {
                let report = coordinator::run_observe(
                    &root,
                    repo_id,
                    plot_id,
                    run_id,
                    cursor.as_deref(),
                    state_dir.as_deref(),
                );
                print_report_and_exit(
                    report.ok,
                    cli.json,
                    || serde_json::to_string_pretty(&report),
                    || coordinator::run_observation_toon(&report),
                )
            }
        },
        Some(Cmd::Workflow {
            command: WorkflowCmd::Show,
        }) => {
            let report = nopal_core::workflow_report::workflow_show(&root)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || nopal_core::workflow_report::workflow_show_toon(&report),
            )
        }
        Some(Cmd::Enforcement { command }) => {
            let args = match command {
                EnforcementCmd::Plan(args) => args,
                EnforcementCmd::RecordGate(args) => &args.enforcement,
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
            let request = enforcement::EnforcementRequest {
                root: &root,
                config_dir: config_dir.as_deref(),
                mode: args.mode,
                action: &args.action,
                classes: &args.classes,
                run_dir: Some(&run_dir),
            };
            match command {
                EnforcementCmd::Plan(_) => {
                    let report = enforcement::plan(request)?;
                    enforcement::record_decision(&run_dir, &report)?;
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
                    enforcement::record_gate(request, &run_dir, &record.gate_id, record.exit_code)?;
                    let report = serde_json::json!({
                        "kind": "nopal.enforcement.record_gate/v1",
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

fn run_plot_cmd(cli: &Cli, command: &PlotCmd) -> std::io::Result<ExitCode> {
    match command {
        PlotCmd::Establish(args) => {
            let tmux =
                resolve_tmux_identity(args.host_session.as_deref(), args.host_pane.as_deref());
            let (host_session, host_pane, tagged_plot_id) = match tmux {
                Ok(identity) => identity,
                Err(message) => {
                    let report = nopal_core::plot_report::failure(
                        nopal_core::diagnostics::Code::PlotSnapshotInvalid,
                        "tmux",
                        message,
                    );
                    return print_report_and_exit(
                        false,
                        cli.json,
                        || serde_json::to_string_pretty(&report),
                        || nopal_core::plot_report::establishment_toon(&report),
                    );
                }
            };
            let protocol = args.protocol_address.as_ref().map(|address| {
                nopal_core::plot::SessionProtocolEndpoint::unix_with_kind(
                    args.protocol_kind
                        .as_deref()
                        .unwrap_or(nopal_core::plot::SESSION_PROTOCOL_KIND),
                    address,
                    args.protocol_state.as_deref().unwrap_or("ready"),
                )
            });
            let report = nopal_core::plot_report::establish_with_protocol(
                args.state_dir.as_deref(),
                args.plot_id.as_deref().or(tagged_plot_id.as_deref()),
                &args.field_session,
                &args.event,
                &args.workspace,
                &host_session,
                host_pane.as_deref(),
                protocol,
            );
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || nopal_core::plot_report::establishment_toon(&report),
            )
        }
    }
}

fn resolve_tmux_identity(
    host_session: Option<&str>,
    host_pane: Option<&str>,
) -> Result<(String, Option<String>, Option<String>), String> {
    if let Some(host_session) = host_session {
        return Ok((host_session.to_owned(), host_pane.map(str::to_owned), None));
    }
    let pane = host_pane
        .map(str::to_owned)
        .or_else(|| std::env::var("TMUX_PANE").ok())
        .ok_or_else(|| "--host-session is required outside a tmux pane".to_owned())?;
    let output = std::process::Command::new("tmux")
        .args([
            "display-message",
            "-p",
            "-t",
            &pane,
            "#{session_name}|#{pane_id}|#{@nopal_plot}",
        ])
        .output()
        .map_err(|error| format!("failed to resolve tmux Session: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "failed to resolve tmux Session for pane {pane}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    parse_tmux_plot_identity(String::from_utf8_lossy(&output.stdout).trim(), &pane)
}

fn resolve_run_plot_id(explicit_plot_id: Option<&str>) -> Result<String, String> {
    if let Some(plot_id) = explicit_plot_id {
        return Ok(plot_id.to_owned());
    }
    let (_session, _pane, tagged_plot_id) = resolve_tmux_identity(None, None)?;
    tagged_plot_id.ok_or_else(|| {
        "--plot-id is required when the current tmux pane has no @nopal_plot tag".to_owned()
    })
}

fn parse_tmux_plot_identity(
    value: &str,
    expected_pane: &str,
) -> Result<(String, Option<String>, Option<String>), String> {
    let mut fields = value.splitn(3, '|');
    let session = fields.next().unwrap_or_default();
    let pane = fields.next().unwrap_or_default();
    let plot_id = fields.next().unwrap_or_default();
    if session.is_empty() || pane != expected_pane {
        return Err(format!(
            "tmux returned mismatched identity for pane {expected_pane}"
        ));
    }
    Ok((
        session.to_owned(),
        Some(pane.to_owned()),
        (!plot_id.is_empty()).then(|| plot_id.to_owned()),
    ))
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

/// Python `load_payload`: no file means an empty object payload.
fn load_payload(path: Option<&std::path::Path>) -> std::io::Result<ledger_core::JsonValue> {
    match path {
        None => Ok(nopal_ledger_json::json!({})),
        Some(path) => {
            let text = std::fs::read_to_string(path)?;
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
        } => {
            let report = ledger::ledger_interrupt(
                root,
                state_dir,
                run_id,
                flow.as_deref(),
                reason,
                resume_hint.as_deref(),
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
        } => {
            let report = ledger::ledger_finalize(
                root,
                state_dir,
                run_id,
                flow.as_deref(),
                status,
                report_file.as_deref(),
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ledger::mutation_toon(&report),
            )
        }
        LedgerCmd::Resume {
            flow,
            ticket_id,
            branch,
            include_completed,
        } => {
            let report = ledger::ledger_resume(
                root,
                state_dir,
                &ledger::ResumeArgs {
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

fn run_ask_cmd(
    cli: &Cli,
    root: &Path,
    state_dir: Option<&std::path::Path>,
    command: &AskCmd,
) -> std::io::Result<ExitCode> {
    match command {
        AskCmd::Raise {
            session,
            run_id,
            flow,
            mode,
            action,
            rule,
            classes,
            reason,
            evidence,
            ttl_seconds,
        } => {
            let report = ask::ask_raise(
                root,
                state_dir,
                &RaiseArgs {
                    session_id: session,
                    run_id: run_id.as_deref(),
                    flow: flow.as_deref(),
                    mode,
                    action,
                    rule: rule.as_deref(),
                    classes,
                    reason,
                    evidence: evidence.as_deref(),
                    ttl_seconds: *ttl_seconds,
                },
            )?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ask::raise_toon(&report),
            )
        }
        AskCmd::List { state, all } => {
            let filter = if *all {
                None
            } else {
                state.or(Some(AskState::Pending))
            };
            let report = ask::ask_list(root, state_dir, filter)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ask::list_toon(&report),
            )
        }
        AskCmd::Show { id } => {
            let report = ask::ask_show(root, state_dir, id)?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ask::show_toon(&report),
            )
        }
        AskCmd::Resolve {
            id,
            decision,
            by,
            note,
        } => {
            let report = ask::ask_resolve(root, state_dir, id, *decision, by, note.as_deref())?;
            print_report_and_exit(
                report.ok,
                cli.json,
                || serde_json::to_string_pretty(&report),
                || ask::resolve_toon(&report),
            )
        }
        AskCmd::Await {
            id,
            timeout_seconds,
            poll_ms,
        } => {
            let report = ask::ask_await(
                root,
                state_dir,
                &ask::AwaitArgs {
                    ask_id: id,
                    timeout_seconds: *timeout_seconds,
                    poll_ms: *poll_ms,
                },
            )?;
            // await encodes the outcome in the exit code so a blocked caller
            // can fail closed without parsing the payload.
            if cli.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", ask::await_toon(&report));
            }
            Ok(ExitCode::from(ask::await_exit_code(&report)))
        }
    }
}

/// User-level config dir for cross-repo nopal defaults:
/// `$NOPAL_CONFIG_DIR` when set, else `$HOME/.config/nopal`; `None` when
/// neither is available (no lookup possible - `scaffold::
/// resolve_bundle_scaffold` treats that identically to "no template found").
/// Read once here, at the CLI boundary, and threaded down from here on as a
/// plain `Option<PathBuf>` - every nopal-core function that might consult it
/// (`scaffold::write_defaults`, `launch::plan`'s unconfigured branch) takes
/// the already-resolved directory as a parameter instead of reading the
/// environment itself, so tests can isolate template lookup by construction
/// (an explicit temp dir, or `None`) instead of by mutating process env -
/// see `nopal-core::scaffold`'s module tests and this crate's
/// `tests/coordinator.rs`, which sets `NOPAL_CONFIG_DIR` on every spawned
/// `nopal` subprocess for exactly this reason.
fn resolve_config_dir() -> Option<PathBuf> {
    // Empty env values are treated as unset (XDG convention). Without the
    // filter, `NOPAL_CONFIG_DIR=` would make the template path the bare
    // relative `bundle-default.jsonc`, resolved against the process cwd -
    // letting a repo-local file of that name in whatever directory nopal
    // runs from silently become the user's standing template.
    if let Some(dir) = std::env::var("NOPAL_CONFIG_DIR")
        .ok()
        .filter(|v| !v.is_empty())
    {
        return Some(PathBuf::from(dir));
    }
    std::env::var("HOME")
        .ok()
        .filter(|v| !v.is_empty())
        .map(|home| Path::new(&home).join(".config").join("nopal"))
}

/// Runs the cold `nopal.launch/v1` gates for `nopal cli`. On `--dry-run` or
/// any gate failure, renders the plan and exits without touching Pi.
///
/// A real (non-dry-run) launch against an unconfigured repo
/// (`plan.scaffold == WouldCreate`) writes `.nopal/nopal.jsonc` and
/// `.nopal/bundle.jsonc` first, silently and without prompting, then re-runs
/// `plan` against
/// the files it just wrote - that re-validation, not the write itself, is
/// the actual gate: a half-written or unexpectedly rejected scaffold fails
/// closed exactly like any other misconfigured repo instead of launching
/// anyway. An invalid user-level bundle template is caught earlier
/// still: `launch::plan` already resolves the same template/hermetic source
/// `write_defaults` would use, so `plan.ok` is already `false` and this
/// function returns at the first check below - `write_defaults` is never
/// even called, let alone anything written. Only a passing plan with
/// `dry_run` false reaches `exec_pi`.
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
    let config_dir = resolve_config_dir();
    let plan = launch::plan(root, with_ambient, config_dir.as_deref())?;
    if dry_run || !plan.ok {
        return print_report_and_exit(
            plan.ok,
            cli.json,
            || serde_json::to_string_pretty(&plan),
            || launch::launch_toon(&plan),
        );
    }

    let mut created_source = None;
    let plan = if plan.scaffold == launch::Scaffold::WouldCreate {
        let scaffolded = scaffold::write_defaults(root, config_dir.as_deref())?;
        let rescaffolded = launch::plan(root, with_ambient, config_dir.as_deref())?;
        if !rescaffolded.ok {
            // Even a failing re-plan must record that this launch just wrote
            // two files into the repo: mark the report and print the
            // created-notice, or the one path that scaffolded and then
            // failed would be the one path with no visible record of it.
            eprintln!("{}", scaffold_notice(&scaffolded.source));
            let marked = launch::mark_scaffolded(rescaffolded, &scaffolded.source);
            return print_report_and_exit(
                false,
                cli.json,
                || serde_json::to_string_pretty(&marked),
                || launch::launch_toon(&marked),
            );
        }
        let marked = launch::mark_scaffolded(rescaffolded, &scaffolded.source);
        created_source = Some(scaffolded.source);
        marked
    } else {
        plan
    };

    if let Some(source) = &created_source {
        eprintln!("{}", scaffold_notice(source));
    }
    let enforcement_extension_pinned = plan.pi_argv.windows(2).any(|pair| {
        (pair[0] == "-e" || pair[0] == "--extension")
            && pair[1]
                .replace('\\', "/")
                .ends_with("/extensions/policy-gate/index.ts")
    });
    if !enforcement_extension_pinned {
        return Err(std::io::Error::other(
            "enforcement initialization failed: the pinned bundle does not contain extensions/policy-gate/index.ts",
        ));
    }

    let config_dir = resolve_config_dir();
    let enforcement_plan = enforcement::plan(enforcement::EnforcementRequest {
        root,
        config_dir: config_dir.as_deref(),
        mode: policy::Mode::SupervisedAuto,
        action: "git.push",
        classes: &[policy::ActionClass::GitRemote],
        run_dir: None,
    })?;
    if !enforcement_plan.ok {
        return print_report_and_exit(
            false,
            cli.json,
            || serde_json::to_string_pretty(&enforcement_plan),
            || serde_json::to_string_pretty(&enforcement_plan).unwrap_or_else(|_| "{}".to_owned()),
        );
    }

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

    eprintln!("{}", launch::resource_surface_line(&plan));
    if verbose {
        eprintln!("{}", launch::summary_line(&plan));
    }
    // exec_pi's cwd is the ORIGINAL invocation dir (`cli.dir`, as given),
    // not the discovered `root`: config/gates/bundle
    // resolve at the discovered project root, but pi itself starts where
    // the user stands. Bundle resource paths in `plan.pi_argv` are already
    // absolutized (`bundle::bundle_report` absolutizes `root` before
    // resolving them), so this split is safe - nothing in pi's argv depends
    // on `cli.dir`.
    let mut pi_argv = plan.pi_argv;
    pi_argv.extend(cli.pi_args.iter().cloned());
    exec_pi(&cli.dir, &pi_argv, &run.run_id)
}

fn warn_for_interactive_rondo(root: &Path) {
    if let Some(warning) = coordinator::ensure_interactive_rondo(root) {
        eprintln!("{warning}");
    }
}

/// The always-on created-notice, shared by the success path and the
/// scaffolded-then-failed re-plan path so a write is never silent.
fn scaffold_notice(source: &scaffold::ScaffoldSource) -> String {
    format!(
        "nopal: created {} + {} ({})",
        discover::manifest_rel_path(),
        nopal_core::bundle::bundle_rel_path(),
        source.describe()
    )
}

#[cfg(unix)]
fn exec_pi(
    dir: &std::path::Path,
    argv: &[String],
    enforcement_run_id: &str,
) -> std::io::Result<ExitCode> {
    use std::os::unix::process::CommandExt;
    let pi_bin = std::env::var("NOPAL_PI_BIN").unwrap_or_else(|_| "pi".to_owned());
    let err = std::process::Command::new(&pi_bin)
        .args(argv)
        .current_dir(dir)
        // Pi's own update-check network call and banner are noise nopal
        // already owns readiness reporting for; skip it every launch.
        .env("PI_SKIP_VERSION_CHECK", "1")
        .env("NOPAL_ENFORCEMENT_RUN_ID", enforcement_run_id)
        .exec();
    // `exec` only returns on failure; success replaces this process image.
    Err(std::io::Error::new(
        err.kind(),
        format!("failed to exec {pi_bin:?}: {err}"),
    ))
}

#[cfg(not(unix))]
fn exec_pi(
    _dir: &std::path::Path,
    _argv: &[String],
    _enforcement_run_id: &str,
) -> std::io::Result<ExitCode> {
    Err(std::io::Error::other(
        "nopal cli requires a unix platform; there is no non-unix spawn fallback (D7)",
    ))
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

fn print_report<T, F>(json: bool, report: &T, render_toon: F) -> std::io::Result<()>
where
    T: serde::Serialize,
    F: Fn(&T) -> String,
{
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
    } else {
        print!("{}", render_toon(report));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_tmux_plot_identity;

    #[test]
    fn tmux_identity_prefers_the_explicit_plot_tag_and_verifies_the_pane() {
        assert_eq!(
            parse_tmux_plot_identity("nopal-work|%4|plot-1", "%4"),
            Ok((
                "nopal-work".to_owned(),
                Some("%4".to_owned()),
                Some("plot-1".to_owned())
            ))
        );
        assert!(parse_tmux_plot_identity("nopal-work|%9|plot-1", "%4").is_err());
    }
}
