//! `nopal field` argument surface and launcher.
//!
//! `nopal field native` requires the separately packaged desktop sibling. Explicit
//! `nopal field legacy` and bare `nopal field` attach-or-create the tmux
//! session and exec `tmux attach` (or switch the client when already inside
//! tmux). The hidden `ui` subcommand is what the legacy field pane itself
//! runs; `bench` drives scriptable feel benchmarks against a throwaway
//! session. Bare `nopal` with no subcommand also stays on the legacy
//! route through `run_bare`, with every flag at its default, until the native
//! desktop route replaces it.

use std::ffi::OsString;
use std::io;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use crate::app::{self, Options};
use crate::bench;
use crate::feeds::rondo::RunSpec;
use crate::tmux::Backend;

#[derive(clap::Args, Debug)]
pub struct FieldArgs {
    /// Legacy tmux session name backing the terminal Field
    #[arg(long, default_value = "nopal", global = true)]
    session: String,

    /// Legacy Field nopal binary used by ask/ledger feeds
    #[arg(long)]
    nopal_bin: Option<PathBuf>,

    /// State root for native or legacy Field facts and ask resolution
    #[arg(long)]
    state_dir: Option<PathBuf>,

    /// Legacy Field rondo.core/v1 run.events feed
    #[arg(long)]
    rondo_events: Option<PathBuf>,

    /// Legacy Field directory containing rondo's mix.exs (default: $NOPAL_RONDO_DIR,
    /// then ~/Personal/rondo/elixir)
    #[arg(long)]
    rondo_dir: Option<PathBuf>,

    /// Legacy Field run to tail as repo_id:run_id (repeatable)
    #[arg(long = "rondo-run", value_parser = RunSpec::parse)]
    rondo_runs: Vec<RunSpec>,

    /// Show every tmux session in the legacy sidebar
    #[arg(long)]
    all: bool,

    #[command(subcommand)]
    pub command: Option<FieldCmd>,
}

#[derive(clap::Subcommand, Debug)]
pub enum FieldCmd {
    /// Launch the native desktop Field without requiring a terminal
    Native(NativeArgs),
    /// Launch the legacy tmux-backed terminal Field
    Legacy,
    /// Inspect the live field as a deterministic nopal.field/v1 report
    Inspect(InspectArgs),
    /// Internal: run the field UI inside its tmux pane
    #[command(hide = true)]
    Ui,
    /// Run the scriptable feel benchmarks against a throwaway tmux session
    Bench(bench::BenchArgs),
}

#[derive(clap::Args, Debug)]
pub struct NativeArgs {
    /// State root for the native Field; may also precede the `native` route
    #[arg(long)]
    state_dir: Option<PathBuf>,
}

#[derive(clap::Args, Debug)]
pub struct InspectArgs {
    /// State root to scan; beats BEISLID_STATE_DIR and the XDG default
    #[arg(long)]
    pub state_dir: Option<PathBuf>,

    /// Include completed, stale, and closed runs and every ask state
    #[arg(long)]
    pub all: bool,

    /// Optional rondo.core/v1 run.events feed to attach run status/events
    #[arg(long)]
    pub rondo_events: Option<PathBuf>,

    /// Hours an incomplete, unfinalized run may age before it is stale
    #[arg(long, default_value_t = nopal_core::field::DEFAULT_STALE_AFTER_HOURS)]
    pub stale_after: u64,
}

pub fn run(args: &FieldArgs) -> io::Result<ExitCode> {
    match &args.command {
        Some(FieldCmd::Native(native_args)) => launch_native(args, native_args),
        Some(FieldCmd::Legacy) => launch_legacy(args),
        Some(FieldCmd::Inspect(_)) => Err(io::Error::other(
            "field inspection must be dispatched by nopal-cli",
        )),
        Some(FieldCmd::Ui) => {
            app::run_ui(&options(args)?)?;
            Ok(ExitCode::SUCCESS)
        }
        Some(FieldCmd::Bench(bench_args)) => bench::run(bench_args),
        None => launch_legacy(args),
    }
}

/// Defaults `run_bare` launches with: the "nopal" session, no ledger/rondo
/// overrides, and no subcommand, i.e. attach-or-create only. Kept as its
/// own function so the pinning test can compare it against clap's own
/// parsed defaults without duplicating the values in two places.
fn bare_args() -> FieldArgs {
    FieldArgs {
        session: "nopal".to_owned(),
        nopal_bin: None,
        state_dir: None,
        rondo_events: None,
        rondo_dir: None,
        rondo_runs: Vec::new(),
        all: false,
        command: None,
    }
}

/// Bare `nopal`: attach-or-create the field with default args,
/// identical to running `nopal field` with no flags.
pub fn run_bare() -> io::Result<ExitCode> {
    launch_legacy(&bare_args())
}

/// Launch the separately packaged native Field sibling with only renderer-relevant
/// inputs. tmux session and legacy feed flags deliberately stay on the
/// legacy route instead of leaking into the native product contract.
fn launch_native(args: &FieldArgs, native_args: &NativeArgs) -> io::Result<ExitCode> {
    validate_native_args(args)?;
    let state_dir = match (&args.state_dir, &native_args.state_dir) {
        (Some(before), Some(after)) if before != after => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "native Field state root was specified twice with different values: {} and {}",
                    before.display(),
                    after.display()
                ),
            ));
        }
        (_, Some(after)) => Some(after),
        (Some(before), None) => Some(before),
        (None, None) => None,
    };
    let binary = native_binary()?;
    let mut command = std::process::Command::new(&binary);
    if let Some(state_dir) = state_dir {
        command.arg("--state-dir").arg(state_dir);
    }
    let status = command.status().map_err(|err| {
        io::Error::new(
            err.kind(),
            format!(
                "failed to launch native Field binary {}: {err}; install nopal-field-native beside nopal",
                binary.display()
            ),
        )
    })?;
    match status.code() {
        Some(code) => Ok(ExitCode::from(u8::try_from(code).unwrap_or(1))),
        None => Err(io::Error::other(format!(
            "native Field binary {} terminated without an exit code",
            binary.display()
        ))),
    }
}

fn validate_native_args(args: &FieldArgs) -> io::Result<()> {
    let legacy_flag = if args.session != "nopal" {
        Some("--session")
    } else if args.nopal_bin.is_some() {
        Some("--nopal-bin")
    } else if args.rondo_events.is_some() {
        Some("--rondo-events")
    } else if args.rondo_dir.is_some() {
        Some("--rondo-dir")
    } else if !args.rondo_runs.is_empty() {
        Some("--rondo-run")
    } else if args.all {
        Some("--all")
    } else {
        None
    };
    if let Some(flag) = legacy_flag {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{flag} configures the legacy tmux Field and cannot be used with `nopal field native`"
            ),
        ));
    }
    Ok(())
}

fn native_binary() -> io::Result<PathBuf> {
    #[cfg(debug_assertions)]
    if let Some(override_path) = std::env::var_os("NOPAL_FIELD_NATIVE_BIN") {
        return Ok(PathBuf::from(override_path));
    }
    sibling_native_binary(&std::env::current_exe()?)
}

fn sibling_native_binary(nopal_binary: &Path) -> io::Result<PathBuf> {
    let parent = nopal_binary
        .parent()
        .filter(|path| !path.as_os_str().is_empty());
    let Some(parent) = parent else {
        return Err(io::Error::other(format!(
            "cannot locate nopal-field-native beside nopal executable {}",
            nopal_binary.display()
        )));
    };
    let mut file_name = OsString::from("nopal-field-native");
    file_name.push(std::env::consts::EXE_SUFFIX);
    Ok(parent.join(file_name))
}

fn options(args: &FieldArgs) -> io::Result<Options> {
    let nopal_bin = match &args.nopal_bin {
        Some(bin) => bin.clone(),
        None => std::env::current_exe()?,
    };
    Ok(Options {
        session: args.session.clone(),
        nopal_bin,
        state_dir: args.state_dir.clone(),
        rondo_events: args.rondo_events.clone(),
        rondo_dir: default_rondo_dir(args.rondo_dir.clone()),
        rondo_runs: args.rondo_runs.clone(),
        resolve_by: std::env::var("USER").unwrap_or_else(|_| "operator".to_owned()),
        show_all: args.all,
    })
}

fn default_rondo_dir(flag: Option<PathBuf>) -> PathBuf {
    if let Some(dir) = flag {
        return dir;
    }
    if let Ok(dir) = std::env::var("NOPAL_RONDO_DIR") {
        return PathBuf::from(dir);
    }
    let home = std::env::var("HOME").unwrap_or_default();
    PathBuf::from(home).join("Personal/rondo/elixir")
}

/// Attach-or-create, then hand the terminal to tmux.
///
/// Bare `nopal` routes here now, via `run_bare`, alongside
/// `nopal field` itself; it must therefore never start a TUI blindly:
/// without a terminal on stdin/stdout it fails with a clear message
/// instead. `main.rs` already checks the tty before calling `run_bare`
/// and points the operator at `nopal cli`; this check stays as a backstop
/// for direct `nopal field` invocations.
fn launch_legacy(args: &FieldArgs) -> io::Result<ExitCode> {
    use std::io::IsTerminal;
    if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Err(io::Error::other(
            "nopal field needs a terminal (stdin/stdout is not a tty); \
             run it from an interactive terminal",
        ));
    }
    let backend = Backend::new(args.session.clone());
    if !backend.session_exists() {
        backend.create_session(&ui_command(args)?)?;
    } else {
        // Idempotent relaunch: after a resurrect/continuum restore the
        // field pane comes back as a plain shell (and pane user options
        // are gone); revive the UI in place instead of duplicating state.
        repair_session(args)?;
    }
    let target = format!("={}", args.session);
    if std::env::var_os("TMUX").is_some() {
        // Already inside tmux: switch this client instead of nesting.
        let status = std::process::Command::new("tmux")
            .args(["switch-client", "-t", &target])
            .status()?;
        return Ok(if status.success() {
            ExitCode::SUCCESS
        } else {
            ExitCode::from(1)
        });
    }
    exec_tmux_attach(&target)
}

/// Revive the field UI inside an existing session when it is not
/// running (dead shell after a restore, or a killed UI). Detection is by
/// pane role tag, falling back to the field window's leftmost pane;
/// the UI re-tags itself on start, so one repair cycle heals lost tags.
fn repair_session(args: &FieldArgs) -> io::Result<()> {
    let list = std::process::Command::new("tmux")
        .args([
            "list-panes",
            "-s",
            "-t",
            &format!("={}", args.session),
            "-F",
            "#{pane_id}|#{window_name}|#{pane_index}|#{@nopal_role}|#{pane_current_command}",
        ])
        .output()?;
    let text = String::from_utf8_lossy(&list.stdout);
    let mut field_pane: Option<(String, String)> = None;
    for line in text.lines() {
        let fields: Vec<&str> = line.split('|').collect();
        if fields.len() != 5 {
            continue;
        }
        let by_role = fields[3] == "field";
        let by_layout = fields[1] == crate::tmux::FIELD_WINDOW && fields[2] == "0";
        if by_role || (by_layout && field_pane.is_none()) {
            field_pane = Some((fields[0].to_owned(), fields[4].to_owned()));
            if by_role {
                break;
            }
        }
    }
    match field_pane {
        // UI already alive (the pane runs this very binary): nothing to do.
        Some((_, command)) if command == "nopal" => Ok(()),
        Some((pane_id, _)) => {
            let ui = ui_command(args)?;
            std::process::Command::new("tmux")
                .args(["respawn-pane", "-k", "-t", &pane_id, &ui])
                .output()?;
            Backend::tag_field_pane(&pane_id)
        }
        None => {
            // No field window survived; rebuild it inside the session.
            let ui = ui_command(args)?;
            let output = std::process::Command::new("tmux")
                .args([
                    "new-window",
                    "-t",
                    &format!("={}", args.session),
                    "-n",
                    crate::tmux::FIELD_WINDOW,
                    "-P",
                    "-F",
                    "#{pane_id}",
                    &ui,
                ])
                .output()?;
            let pane_id = String::from_utf8_lossy(&output.stdout).trim().to_owned();
            Backend::tag_field_pane(&pane_id)?;
            std::process::Command::new("tmux")
                .args(["split-window", "-h", "-d", "-t", &pane_id])
                .output()?;
            std::process::Command::new("tmux")
                .args([
                    "resize-pane",
                    "-t",
                    &pane_id,
                    "-x",
                    &crate::tmux::SIDEBAR_COLUMNS.to_string(),
                ])
                .output()?;
            Ok(())
        }
    }
}

/// The command line the field pane runs, with every relevant flag
/// forwarded so the in-pane UI sees the same configuration.
fn ui_command(args: &FieldArgs) -> io::Result<String> {
    let exe = std::env::current_exe()?;
    let mut parts = vec![
        quote(&exe.to_string_lossy()),
        "field".to_owned(),
        "--session".to_owned(),
        quote(&args.session),
    ];
    let mut push_path = |flag: &str, value: &Option<PathBuf>| {
        if let Some(value) = value {
            parts.push(flag.to_owned());
            parts.push(quote(&value.to_string_lossy()));
        }
    };
    push_path("--nopal-bin", &args.nopal_bin);
    push_path("--state-dir", &args.state_dir);
    push_path("--rondo-events", &args.rondo_events);
    push_path("--rondo-dir", &args.rondo_dir);
    for spec in &args.rondo_runs {
        parts.push("--rondo-run".to_owned());
        parts.push(quote(&format!("{}:{}", spec.repo_id, spec.run_id)));
    }
    if args.all {
        parts.push("--all".to_owned());
    }
    parts.push("ui".to_owned());
    Ok(parts.join(" "))
}

/// Single-quote for the shell tmux hands the pane command to.
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

#[cfg(unix)]
fn exec_tmux_attach(target: &str) -> io::Result<ExitCode> {
    use std::os::unix::process::CommandExt;
    let err = std::process::Command::new("tmux")
        .args(["attach-session", "-t", target])
        .exec();
    Err(io::Error::new(
        err.kind(),
        format!("failed to exec tmux attach: {err}"),
    ))
}

#[cfg(not(unix))]
fn exec_tmux_attach(_target: &str) -> io::Result<ExitCode> {
    Err(io::Error::other("nopal field requires a unix platform"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn quote_wraps_and_escapes() {
        assert_eq!(quote("plain"), "'plain'");
        assert_eq!(quote("a b"), "'a b'");
        assert_eq!(quote("don't"), "'don'\\''t'");
    }

    /// Pins `bare_args` against clap's own parsed defaults for a bare
    /// `nopal field` invocation, so the two cannot silently drift apart.
    #[test]
    fn bare_args_match_clap_defaults() {
        #[derive(clap::Parser)]
        struct Harness {
            #[command(flatten)]
            args: FieldArgs,
        }

        let parsed = Harness::parse_from(["nopal-field-test-harness"]).args;
        let bare = bare_args();
        assert_eq!(bare.session, parsed.session);
        assert_eq!(bare.nopal_bin, parsed.nopal_bin);
        assert_eq!(bare.state_dir, parsed.state_dir);
        assert_eq!(bare.rondo_events, parsed.rondo_events);
        assert_eq!(bare.rondo_dir, parsed.rondo_dir);
        assert_eq!(bare.rondo_runs, parsed.rondo_runs);
        assert_eq!(bare.all, parsed.all);
        assert!(bare.command.is_none());
        assert!(parsed.command.is_none());
    }

    #[test]
    fn inspect_stale_after_uses_core_default_and_accepts_override() {
        #[derive(clap::Parser)]
        struct Harness {
            #[command(flatten)]
            args: FieldArgs,
        }

        let parsed = Harness::parse_from(["nopal-field-test-harness", "inspect"]).args;
        let Some(FieldCmd::Inspect(parsed)) = parsed.command else {
            panic!("expected inspect command");
        };
        assert_eq!(
            parsed.stale_after,
            nopal_core::field::DEFAULT_STALE_AFTER_HOURS
        );

        let parsed =
            Harness::parse_from(["nopal-field-test-harness", "inspect", "--stale-after", "7"]).args;
        let Some(FieldCmd::Inspect(parsed)) = parsed.command else {
            panic!("expected inspect command");
        };
        assert_eq!(parsed.stale_after, 7);
    }

    #[test]
    fn native_and_legacy_are_explicit_routes() {
        #[derive(clap::Parser)]
        struct Harness {
            #[command(flatten)]
            args: FieldArgs,
        }

        let native = Harness::parse_from([
            "nopal-field-test-harness",
            "native",
            "--state-dir",
            "/tmp/native-state",
        ])
        .args;
        let Some(FieldCmd::Native(native_args)) = native.command else {
            panic!("expected native command");
        };
        assert_eq!(
            native_args.state_dir,
            Some(PathBuf::from("/tmp/native-state"))
        );

        let native = Harness::parse_from([
            "nopal-field-test-harness",
            "--state-dir",
            "/tmp/native-state-before",
            "native",
        ])
        .args;
        assert_eq!(
            native.state_dir,
            Some(PathBuf::from("/tmp/native-state-before"))
        );
        assert!(matches!(native.command, Some(FieldCmd::Native(_))));

        let legacy = Harness::parse_from(["nopal-field-test-harness", "legacy"]).args;
        assert!(matches!(legacy.command, Some(FieldCmd::Legacy)));

        let bare = Harness::parse_from(["nopal-field-test-harness"]).args;
        assert!(
            bare.command.is_none(),
            "bare field remains the legacy route"
        );
    }

    #[test]
    fn production_native_binary_is_a_sibling_of_nopal() {
        let nopal = if cfg!(windows) {
            PathBuf::from(r"C:\Program Files\Nopal\nopal.exe")
        } else {
            PathBuf::from("/opt/nopal/bin/nopal")
        };
        let expected = if cfg!(windows) {
            PathBuf::from(r"C:\Program Files\Nopal\nopal-field-native.exe")
        } else {
            PathBuf::from("/opt/nopal/bin/nopal-field-native")
        };

        assert_eq!(sibling_native_binary(&nopal).unwrap(), expected);
    }

    #[test]
    fn native_route_rejects_legacy_only_configuration() {
        let mut args = bare_args();
        args.session = "other-session".to_owned();

        let error = validate_native_args(&args).unwrap_err();
        assert!(error.to_string().contains("--session"), "{error}");
        assert!(error.to_string().contains("legacy"), "{error}");
    }

    #[test]
    fn explicit_legacy_accepts_session_before_or_after_the_route() {
        #[derive(clap::Parser)]
        struct Harness {
            #[command(flatten)]
            args: FieldArgs,
        }

        let before =
            Harness::parse_from(["nopal-field-test-harness", "--session", "work", "legacy"]).args;
        let after =
            Harness::parse_from(["nopal-field-test-harness", "legacy", "--session", "work"]).args;

        assert_eq!(before.session, "work");
        assert_eq!(after.session, "work");
        assert!(matches!(before.command, Some(FieldCmd::Legacy)));
        assert!(matches!(after.command, Some(FieldCmd::Legacy)));
    }

    #[test]
    fn native_rejects_session_after_its_route_during_dispatch_validation() {
        #[derive(clap::Parser)]
        struct Harness {
            #[command(flatten)]
            args: FieldArgs,
        }

        let parsed =
            Harness::parse_from(["nopal-field-test-harness", "native", "--session", "work"]).args;
        let Some(FieldCmd::Native(_)) = parsed.command else {
            panic!("expected native command");
        };
        let error = validate_native_args(&parsed).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        assert!(error.to_string().contains("--session"), "{error}");
        assert!(error.to_string().contains("legacy"), "{error}");
    }
}
