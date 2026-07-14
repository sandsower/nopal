// Integration tests may panic freely; clippy's in-tests allowance only covers
// #[test] fns, not shared helpers in the tests/ tree.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `nopal field` integration tests: derived staleness and the
//! default live-view filter, driven end to end through the CLI so the clock
//! (`NOPAL_LEDGER_TEST_EPOCH`) can be pinned per process without racing other
//! tests in this binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn nopal_env(repo: &Path, state: &Path, args: &[&str], env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_nopal"));
    cmd.arg("--dir")
        .arg(repo)
        .args(args)
        .env("BEISLID_STATE_DIR", state);
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.output().expect("failed to spawn nopal binary")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout is not utf-8")
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(out)).unwrap_or_else(|err| {
        panic!(
            "stdout is not valid JSON ({err}):\n{}\nstderr: {}",
            stdout(out),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

fn setup() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(tmp.path()).expect("canonicalize tempdir");
    let repo = root.join("repo");
    let state = root.join("state");
    std::fs::create_dir_all(&repo).expect("repo dir");
    (tmp, repo, state)
}

/// Init a run at a pinned epoch, so a later `nopal field inspect`/`ledger prune`
/// invocation (pinned at a later epoch) observes a known age for it.
fn init_at(repo: &Path, state: &Path, run_id: &str, epoch: &str) {
    let out = nopal_env(
        repo,
        state,
        &[
            "ledger", "init", "--skill", "kickoff", "--run-id", run_id, "--json",
        ],
        &[("NOPAL_LEDGER_TEST_EPOCH", epoch)],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

const T0: &str = "1783204993"; // 2026-07-04T22:43:13+00:00
const T0_PLUS_25H: &str = "1783294993"; // +25h
const T0_PLUS_1H: &str = "1783208593"; // +1h

#[test]
fn default_view_hides_stale_runs_and_all_shows_them() {
    let (_tmp, repo, state) = setup();
    init_at(&repo, &state, "old-run", T0);
    init_at(&repo, &state, "fresh-run", T0_PLUS_25H);

    // Observed 25h after old-run's last update (past the 24h default) but
    // only ~0h after fresh-run's.
    let default_view = nopal_env(
        &repo,
        &state,
        &["field", "inspect", "--json"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0_PLUS_25H)],
    );
    assert_eq!(default_view.status.code(), Some(0));
    let default_json = json(&default_view);
    assert_eq!(default_json["kind"], "nopal.field/v1");
    assert_eq!(default_json["total"], 1);
    assert_eq!(default_json["entries"][0]["run_id"], "fresh-run");
    assert_eq!(default_json["stale_total"], 1);

    let all_view = nopal_env(
        &repo,
        &state,
        &["field", "inspect", "--all", "--json"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0_PLUS_25H)],
    );
    assert_eq!(all_view.status.code(), Some(0));
    let all_json = json(&all_view);
    assert_eq!(all_json["total"], 2);
    let old_entry = all_json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["run_id"] == "old-run")
        .expect("old-run present under --all");
    assert_eq!(old_entry["stale"], true);
    let fresh_entry = all_json["entries"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["run_id"] == "fresh-run")
        .expect("fresh-run present under --all");
    assert_eq!(fresh_entry["stale"], false);

    // TOON carries the stale column and the top-level count too.
    let toon = nopal_env(
        &repo,
        &state,
        &["field", "inspect", "--all"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0_PLUS_25H)],
    );
    let toon_text = stdout(&toon);
    assert!(toon_text.contains("stale_total: 1"), "{toon_text}");
}

#[test]
fn stale_after_zero_marks_every_incomplete_run_stale() {
    let (_tmp, repo, state) = setup();
    init_at(&repo, &state, "just-created", T0);

    // Observed a mere second later: with the default 24h threshold this run
    // would not be stale yet, but --stale-after 0 must mark it stale anyway.
    let one_second_later = "1783204994";
    let out = nopal_env(
        &repo,
        &state,
        &["field", "inspect", "--stale-after", "0", "--json"],
        &[("NOPAL_LEDGER_TEST_EPOCH", one_second_later)],
    );
    assert_eq!(out.status.code(), Some(0));
    let body = json(&out);
    // The default live view already excludes it (stale), so it only shows
    // under --all.
    assert_eq!(body["total"], 0);
    assert_eq!(body["stale_total"], 1);

    let all = nopal_env(
        &repo,
        &state,
        &["field", "inspect", "--stale-after", "0", "--all", "--json"],
        &[("NOPAL_LEDGER_TEST_EPOCH", one_second_later)],
    );
    assert_eq!(json(&all)["entries"][0]["stale"], true);
}

#[test]
fn finalized_run_leaves_the_live_view_immediately_even_though_fresh() {
    let (_tmp, repo, state) = setup();
    init_at(&repo, &state, "r1", T0);

    // Interrupt then finalize at T0 + 1h: still well under the 24h
    // threshold, so the run is "fresh", not stale.
    nopal_env(
        &repo,
        &state,
        &["ledger", "interrupt", "--run-id", "r1", "--reason", "pause"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0_PLUS_1H)],
    );
    let finalize = nopal_env(
        &repo,
        &state,
        &[
            "ledger",
            "finalize",
            "--run-id",
            "r1",
            "--status",
            "interrupted",
            "--json",
        ],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0_PLUS_1H)],
    );
    assert_eq!(finalize.status.code(), Some(0));

    // Observed right after finalize: the run is fresh (not stale) but must
    // still be absent from the default live view (closed).
    let field = nopal_env(
        &repo,
        &state,
        &["field", "inspect", "--json"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0_PLUS_1H)],
    );
    assert_eq!(
        json(&field)["total"],
        0,
        "finalized run must not flood the live view"
    );
    assert_eq!(
        json(&field)["stale_total"],
        0,
        "a fresh finalized run is not stale"
    );
}
