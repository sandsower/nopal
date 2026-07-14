//! Field store - the effectful half of `nopal.field/v1`.
//!
//! Walks the whole state dir - every flow, every repo hash - to assemble the
//! live field, unlike the run-ledger and ask stores which scope to one repo
//! hash. It reads three sources and composes nothing itself: run ledgers under
//! `runs/<flow>/<repo_hash>/<run_id>/`, pending asks under
//! `asks/<repo_hash>/<ask_id>/`, and an optional `rondo.core/v1` run-events
//! feed document. The projection lives in `field`; this module only gathers
//! `FieldInputs` and hands them over.
//!
//! The scan is read-only: it never materializes ask expiry (that would mean a
//! cross-repo write storm on a monitoring poll), so `field::project` labels an
//! overdue pending ask as expired logically, from the observation clock.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ledger_json::Value;
use nopal_ledger_json as ledger_json;

use crate::diagnostics::{Code, Diagnostic};
use crate::field::{self, FieldInputs, FieldReport, RondoFeed, RunInput};
use crate::plot_store::{self, PlotEnv};
use crate::run_ledger::Status;
use crate::run_ledger_store::{self as store, LedgerEnv};

/// How many latest-attempt gate rows to keep per run in the field view.
const GATE_LIMIT: usize = 8;

fn sorted_dirs(path: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

/// Read every run under `runs/<flow>/<repo_hash>/<run_id>/`, across all flows
/// and repos. Unreadable runs and runs with an unknown status surface as
/// warnings instead of vanishing (mirrors `run_ledger_store::scan_runs`).
/// `pub(crate)`: `run_ledger_report::ledger_prune` reuses this same
/// global scan rather than duplicating the flow/repo walk.
pub(crate) fn scan_all_runs(
    state_dir: &Path,
    warnings: &mut Vec<Diagnostic>,
) -> io::Result<Vec<RunInput>> {
    let mut runs = Vec::new();
    let runs_root = state_dir.join("runs");
    if !runs_root.is_dir() {
        return Ok(runs);
    }
    for flow_dir in sorted_dirs(&runs_root) {
        for repo_dir in sorted_dirs(&flow_dir) {
            for run_dir in sorted_dirs(&repo_dir) {
                let run_file = run_dir.join("run.json");
                if !run_file.is_file() {
                    continue;
                }
                let entry = match store::read_json(&run_file) {
                    Ok(entry) => entry,
                    Err(store::StoreError::Domain(diag)) => {
                        warnings.push(Diagnostic::warning(
                            Code::LedgerEntryInvalid,
                            run_file.display().to_string(),
                            format!("skipping unreadable run file: {}", diag.message),
                        ));
                        continue;
                    }
                    Err(store::StoreError::Io(err)) => {
                        warnings.push(Diagnostic::warning(
                            Code::LedgerEntryInvalid,
                            run_file.display().to_string(),
                            format!("skipping unreadable run file: {err}"),
                        ));
                        continue;
                    }
                };
                let status_text = entry.get("status").and_then(Value::as_str).unwrap_or("");
                if Status::parse(status_text).is_none() {
                    warnings.push(Diagnostic::warning(
                        Code::LedgerStatusInvalid,
                        run_file.display().to_string(),
                        format!(
                            "skipping run with unknown status {status_text:?} (run-ledger-v1 statuses: running, interrupted, failed, completed)"
                        ),
                    ));
                    continue;
                }
                let gates = store::collect_gate_history(&run_dir, GATE_LIMIT);
                runs.push(RunInput { entry, gates });
            }
        }
    }
    Ok(runs)
}

/// Read every ask under `asks/<repo_hash>/<ask_id>/`, across all repos.
/// Unreadable asks warn and are skipped; expiry is not materialized here.
fn scan_all_asks(state_dir: &Path, warnings: &mut Vec<Diagnostic>) -> io::Result<Vec<Value>> {
    let mut asks = Vec::new();
    let asks_root = state_dir.join("asks");
    if !asks_root.is_dir() {
        return Ok(asks);
    }
    for repo_dir in sorted_dirs(&asks_root) {
        for ask_dir in sorted_dirs(&repo_dir) {
            let ask_file = ask_dir.join("ask.json");
            if !ask_file.is_file() {
                continue;
            }
            match fs::read_to_string(&ask_file)
                .ok()
                .and_then(|text| ledger_json::from_str(&text).ok())
            {
                Some(doc) => asks.push(doc),
                None => warnings.push(Diagnostic::warning(
                    Code::AskEntryInvalid,
                    ask_file.display().to_string(),
                    "skipping unreadable ask file".to_owned(),
                )),
            }
        }
    }
    Ok(asks)
}

/// Read the optional rondo `run.events` feed document. A supplied-but-broken
/// feed degrades to `RondoFeed::Unreadable` plus a path-bearing warning; the
/// field view still renders (composition is degradable, never fatal).
fn read_rondo_feed(rondo_events: Option<&Path>, warnings: &mut Vec<Diagnostic>) -> RondoFeed {
    let Some(path) = rondo_events else {
        return RondoFeed::NotRequested;
    };
    match fs::read_to_string(path) {
        Ok(text) => match ledger_json::from_str(&text) {
            Ok(feed) if feed.get("events").and_then(Value::as_array).is_some() => {
                RondoFeed::Parsed(feed)
            }
            _ => {
                warnings.push(Diagnostic::warning(
                    Code::FieldRondoFeedUnreadable,
                    path.display().to_string(),
                    "rondo feed is not a rondo.core/v1 run.events document".to_owned(),
                ));
                RondoFeed::Unreadable
            }
        },
        Err(err) => {
            warnings.push(Diagnostic::warning(
                Code::FieldRondoFeedUnreadable,
                path.display().to_string(),
                format!("cannot read rondo feed: {err}"),
            ));
            RondoFeed::Unreadable
        }
    }
}

/// Assemble the field inputs from the state dir and project them. `dir` is only
/// used to resolve the state dir (flag > BEISLID_STATE_DIR > XDG); the field
/// itself spans every repo, so the caller's repo hash is not a filter.
pub fn field_status(
    dir: &Path,
    state_dir: Option<&Path>,
    rondo_events: Option<&Path>,
    include_all: bool,
    stale_after_hours: u64,
) -> io::Result<FieldReport> {
    let env = LedgerEnv::discover(dir, state_dir);
    let plot_env = PlotEnv::discover(state_dir);
    let mut warnings = Vec::new();
    let plots = plot_store::scan(&plot_env, &mut warnings)?;
    let runs = scan_all_runs(&env.state_dir, &mut warnings)?;
    let asks = scan_all_asks(&env.state_dir, &mut warnings)?;
    let rondo = read_rondo_feed(rondo_events, &mut warnings);
    let inputs = FieldInputs {
        plots,
        runs,
        asks,
        rondo,
        include_all,
        now_iso: store::now_iso(),
        stale_after_hours,
        scan_warnings: warnings,
    };
    Ok(field::project(&inputs))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_ledger_store::InitArgs;

    fn env_at(state: &Path, repo: &Path, repo_hash: &str) -> LedgerEnv {
        LedgerEnv {
            state_dir: state.to_path_buf(),
            repo: repo.to_path_buf(),
            repo_hash: repo_hash.to_owned(),
        }
    }

    #[test]
    fn scans_runs_across_flows_and_repos() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        // Two repos, different flows, one completed.
        let e1 = env_at(&state, &tmp.path().join("r1"), "repohash0001");
        let e2 = env_at(&state, &tmp.path().join("r2"), "repohash0002");
        store::init_run(
            &e1,
            &InitArgs {
                skill: "kickoff",
                flow: Some("kickoff"),
                ticket_id: "TASK-30",
                ticket_title: "Field",
                ticket_url: "",
                branch: Some("feature/a"),
                run_id: Some("run-a"),
            },
        )
        .unwrap();
        let b = store::init_run(
            &e2,
            &InitArgs {
                skill: "handoff",
                flow: Some("handoff"),
                ticket_id: "TASK-30",
                ticket_title: "Field",
                ticket_url: "",
                branch: Some("feature/b"),
                run_id: Some("run-b"),
            },
        )
        .unwrap();
        store::record_finalize(&b.run_dir, "completed", None).unwrap();

        // Default: only the live run-a is visible.
        let live = field_status(
            tmp.path(),
            Some(&state),
            None,
            false,
            field::DEFAULT_STALE_AFTER_HOURS,
        )
        .unwrap();
        assert_eq!(live.total, 1);
        assert_eq!(live.entries[0].run_id, "run-a");
        assert_eq!(live.entries[0].placement.repo_hash, "repohash0001");

        // --all surfaces the completed run too.
        let all = field_status(
            tmp.path(),
            Some(&state),
            None,
            true,
            field::DEFAULT_STALE_AFTER_HOURS,
        )
        .unwrap();
        assert_eq!(all.total, 2);
    }

    #[test]
    fn empty_state_dir_yields_an_empty_but_ok_field() {
        let tmp = tempfile::tempdir().unwrap();
        let report = field_status(
            tmp.path(),
            Some(&tmp.path().join("state")),
            None,
            false,
            field::DEFAULT_STALE_AFTER_HOURS,
        )
        .unwrap();
        assert!(report.ok);
        assert_eq!(report.total, 0);
        assert_eq!(report.rondo_feed.status, "absent");
    }

    #[test]
    fn field_projection_includes_persisted_plots_without_creating_them() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let plot_env = PlotEnv::discover(Some(&state));
        let plot = plot_store::ensure_provisional(&plot_env, "nopal").unwrap();
        let plot =
            plot_store::bind_session(&plot_env, &plot.plot_id, "nopal-work", Some("%4")).unwrap();

        let report = field_status(
            tmp.path(),
            Some(&state),
            None,
            false,
            field::DEFAULT_STALE_AFTER_HOURS,
        )
        .unwrap();

        assert_eq!(report.plots, vec![plot]);
        assert_eq!(report.total, 0, "run total remains backward-compatible");

        let absent = tmp.path().join("absent");
        let report = field_status(
            tmp.path(),
            Some(&absent),
            None,
            false,
            field::DEFAULT_STALE_AFTER_HOURS,
        )
        .unwrap();
        assert!(report.plots.is_empty());
        assert!(!absent.exists(), "read-only projection never creates state");
    }

    #[test]
    fn field_projection_skips_an_unreadable_plot_with_a_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let plot_env = PlotEnv::discover(Some(&state));
        let plot = plot_store::ensure_provisional(&plot_env, "nopal").unwrap();
        fs::write(state.join("plots/plot-broken.json"), "not json").unwrap();

        let report = field_status(
            tmp.path(),
            Some(&state),
            None,
            false,
            field::DEFAULT_STALE_AFTER_HOURS,
        )
        .unwrap();

        assert!(report.ok);
        assert_eq!(report.plots, vec![plot]);
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == Code::PlotSnapshotInvalid
                && diagnostic.path == "plots/plot-broken.json"
        }));
    }

    #[test]
    fn broken_rondo_feed_degrades_to_unreadable_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let feed = tmp.path().join("feed.json");
        fs::write(&feed, "not json").unwrap();
        let report = field_status(
            tmp.path(),
            Some(&state),
            Some(&feed),
            false,
            field::DEFAULT_STALE_AFTER_HOURS,
        )
        .unwrap();
        assert!(report.ok);
        assert_eq!(report.rondo_feed.status, "absent");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == Code::FieldRondoFeedUnreadable)
        );
    }
}
