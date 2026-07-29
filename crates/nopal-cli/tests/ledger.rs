// Integration tests may panic freely; clippy's in-tests allowance only covers
// #[test] fns, not shared helpers in the tests/ tree.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `nopal ledger` integration tests: lifecycle roundtrip, domain failures,
//! the three concurrency cases ported from beislid's
//! `test_run_ledger_concurrency.py`, and byte-level write equivalence
//! against the vendored Python reference tool.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn nopal(repo: &Path, state: &Path, args: &[&str]) -> Output {
    nopal_env(repo, state, args, &[])
}

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
    // Canonicalize so paths embedded by this process and by Python's
    // Path.resolve() agree (macOS /var vs /private/var).
    let root = fs::canonicalize(tmp.path()).expect("canonicalize tempdir");
    let repo = root.join("repo");
    let state = root.join("state");
    fs::create_dir_all(&repo).expect("repo dir");
    (tmp, repo, state)
}

fn write_file(path: &Path, content: &str) {
    fs::write(path, content).expect("write file");
}

fn run_dir_of(state: &Path, flow: &str, run_id: &str) -> PathBuf {
    // Non-git repo dirs hash to the same fallback the Python tool uses.
    state
        .join("runs")
        .join(flow)
        .join("unknown-repo")
        .join(run_id)
}

// ---------------------------------------------------------------------------
// Lifecycle and domain failures
// ---------------------------------------------------------------------------

#[test]
fn ledger_lifecycle_roundtrip() {
    let (_tmp, repo, state) = setup();

    let init = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "init",
            "--skill",
            "kickoff",
            "--ticket-id",
            "TASK-19",
            "--ticket-title",
            "Port",
            "--branch",
            "feature/x",
            "--run-id",
            "r1",
            "--json",
        ],
    );
    assert_eq!(
        init.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let init_json = json(&init);
    assert_eq!(init_json["kind"], "nopal.run_ledger.init/v1");
    assert_eq!(init_json["ok"], true);
    assert_eq!(init_json["run_id"], "r1");
    assert_eq!(init_json["flow"], "kickoff");

    let run_dir = run_dir_of(&state, "kickoff", "r1");
    assert!(run_dir.join("run.json").is_file());
    assert!(run_dir.join("artifacts/gates").is_dir());

    let payload = repo.join("payload.json");
    write_file(&payload, r#"{"github_token": "leak", "n": 1}"#);
    let event = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "event",
            "--run-id",
            "r1",
            "--type",
            "step",
            "--json-file",
            payload.to_str().unwrap(),
            "--summary",
            "did a step",
            "--json",
        ],
    );
    assert_eq!(event.status.code(), Some(0));
    assert_eq!(json(&event)["kind"], "nopal.run_ledger.event/v1");
    let events_text = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(events_text.contains("\"github_token\": \"[REDACTED]\""));

    let checkpoint = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "checkpoint",
            "--run-id",
            "r1",
            "--name",
            "ctx ready",
            "--resume-hint",
            "resume at step 2",
            "--json",
        ],
    );
    assert_eq!(checkpoint.status.code(), Some(0));
    let checkpoint_json = json(&checkpoint);
    assert!(
        checkpoint_json["checkpoint"]
            .as_str()
            .unwrap()
            .ends_with("checkpoints/ctx-ready.json")
    );

    let envelope = repo.join("envelope.json");
    write_file(
        &envelope,
        r#"{"status": "fail", "environment_failure": true, "gate": {"name": "fmt", "scope": "repo", "timestamp": "T1"}}"#,
    );
    let gate = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "gate",
            "--run-id",
            "r1",
            "--name",
            "fmt",
            "--envelope-file",
            envelope.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(gate.status.code(), Some(0));
    let gate_json = json(&gate);
    assert!(
        gate_json["gate_log"]
            .as_str()
            .unwrap()
            .ends_with("/1/envelope.json")
    );

    let resume = nopal(&repo, &state, &["ledger", "resume", "--json"]);
    assert_eq!(resume.status.code(), Some(0));
    let resume_json = json(&resume);
    assert_eq!(resume_json["kind"], "nopal.run_ledger.resume/v1");
    assert_eq!(resume_json["run_id"], "r1");
    assert_eq!(resume_json["status"], "running");

    let interrupt = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "interrupt",
            "--run-id",
            "r1",
            "--reason",
            "pause",
            "--json",
        ],
    );
    assert_eq!(interrupt.status.code(), Some(0));
    assert_eq!(json(&interrupt)["status"], "interrupted");

    let report_md = repo.join("report.md");
    write_file(&report_md, "# done\n");
    let finalize = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "finalize",
            "--run-id",
            "r1",
            "--status",
            "completed",
            "--report-file",
            report_md.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(finalize.status.code(), Some(0));
    let finalize_json = json(&finalize);
    assert!(
        finalize_json["final_report"]
            .as_str()
            .unwrap()
            .ends_with("final-report.md")
    );
    assert!(run_dir.join("final-report.md").is_file());

    // Completed runs are gated out of default resume, back with the flag.
    let gone = nopal(&repo, &state, &["ledger", "resume", "--json"]);
    assert_eq!(gone.status.code(), Some(1));
    assert_eq!(json(&gone)["ok"], false);
    let back = nopal(
        &repo,
        &state,
        &["ledger", "resume", "--include-completed", "--json"],
    );
    assert_eq!(back.status.code(), Some(0));
    assert_eq!(json(&back)["status"], "completed");

    // Dashboard: default hides completed, --all shows it with gates.
    let empty = nopal(&repo, &state, &["ledger", "dashboard", "--json"]);
    assert_eq!(empty.status.code(), Some(0));
    assert_eq!(json(&empty)["total"], 0);
    let all = nopal(&repo, &state, &["ledger", "dashboard", "--all", "--json"]);
    let all_json = json(&all);
    assert_eq!(all_json["total"], 1);
    assert_eq!(
        all_json["runs"][0]["gates"][0]["classification"],
        "environment_failure"
    );
    assert!(all_json["runs"][0]["final_report"].as_str().is_some());

    // TOON flavors carry the same kinds.
    let toon = nopal(&repo, &state, &["ledger", "dashboard", "--all"]);
    let toon_text = stdout(&toon);
    assert!(
        toon_text.contains("kind: nopal.run_ledger.dashboard/v1"),
        "{toon_text}"
    );
    assert!(toon_text.contains("environment_failure"), "{toon_text}");
}

#[test]
fn exact_resume_returns_revision_stamped_bounded_continuation() {
    let (_tmp, repo, state) = setup();
    for run_id in ["r1", "r2"] {
        let initialized = nopal(
            &repo,
            &state,
            &["ledger", "init", "--skill", "resume", "--run-id", run_id],
        );
        assert_eq!(initialized.status.code(), Some(0));
    }
    let checkpoint = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "checkpoint",
            "--run-id",
            "r1",
            "--name",
            "ready",
            "--resume-hint",
            "rerun exact local proof",
        ],
    );
    assert_eq!(checkpoint.status.code(), Some(0));

    let exact = nopal(
        &repo,
        &state,
        &[
            "ledger", "resume", "--run-id", "r1", "--flow", "resume", "--json",
        ],
    );
    assert_eq!(exact.status.code(), Some(0));
    let body = json(&exact);
    assert_eq!(body["run_id"], "r1");
    assert_eq!(body["exact"], true);
    assert!(body["revision"].as_u64().unwrap() >= 2);
    assert!(body["transaction_digest"].as_str().is_some());
    assert_eq!(body["resume_epoch"], 0);
    assert_eq!(body["must_reverify"], true);
    assert_eq!(
        body["expected_next_action"],
        "recover_interrupted_operation"
    );
    assert_eq!(body["resume_hint"], "rerun exact local proof");

    let interrupted = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "interrupt",
            "--run-id",
            "r1",
            "--flow",
            "resume",
            "--reason",
            "restart",
        ],
    );
    assert_eq!(interrupted.status.code(), Some(0));
    let continued = nopal(
        &repo,
        &state,
        &[
            "ledger", "continue", "--run-id", "r1", "--flow", "resume", "--json",
        ],
    );
    assert_eq!(continued.status.code(), Some(0));
    assert_eq!(json(&continued)["status"], "running");
    let resumed = nopal(
        &repo,
        &state,
        &[
            "ledger", "resume", "--run-id", "r1", "--flow", "resume", "--json",
        ],
    );
    let resumed = json(&resumed);
    assert_eq!(resumed["resume_epoch"], 1);
    assert_eq!(resumed["must_reverify"], true);
    assert_eq!(
        resumed["expected_next_action"],
        "recover_interrupted_operation"
    );
}

#[test]
fn explicit_run_id_collision_exits_one_with_stable_code() {
    let (_tmp, repo, state) = setup();
    let first = nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "s", "--run-id", "dup"],
    );
    assert_eq!(first.status.code(), Some(0));
    let second = nopal(
        &repo,
        &state,
        &[
            "ledger", "init", "--skill", "s", "--run-id", "dup", "--json",
        ],
    );
    assert_eq!(second.status.code(), Some(1));
    let body = json(&second);
    assert_eq!(body["ok"], false);
    assert_eq!(body["diagnostics"][0]["code"], "run_id_collision");
}

#[test]
fn unsafe_run_id_is_rejected() {
    let (_tmp, repo, state) = setup();
    let out = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "init",
            "--skill",
            "s",
            "--run-id",
            "../escape",
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json(&out)["diagnostics"][0]["code"], "run_id_invalid");
}

#[test]
fn missing_run_and_bad_final_status_are_domain_failures() {
    let (_tmp, repo, state) = setup();
    let missing = nopal(
        &repo,
        &state,
        &[
            "ledger", "event", "--run-id", "absent", "--type", "x", "--json",
        ],
    );
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(json(&missing)["diagnostics"][0]["code"], "run_not_found");

    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "s", "--run-id", "r1"],
    );
    let bad_status = nopal(
        &repo,
        &state,
        &[
            "ledger", "finalize", "--run-id", "r1", "--status", "running", "--json",
        ],
    );
    assert_eq!(bad_status.status.code(), Some(1));
    assert_eq!(
        json(&bad_status)["diagnostics"][0]["code"],
        "ledger_status_invalid"
    );
}

#[cfg(unix)]
#[test]
fn symlinked_or_hardlinked_transaction_authority_fails_closed() {
    use std::os::unix::fs::symlink;

    let (_tmp, repo, state) = setup();
    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "s", "--run-id", "symlink"],
    );
    let run_dir = run_dir_of(&state, "s", "symlink");
    let external = repo.join("external-transactions");
    fs::create_dir(&external).unwrap();
    fs::remove_dir_all(run_dir.join("transactions")).unwrap();
    symlink(&external, run_dir.join("transactions")).unwrap();
    let blocked = nopal(
        &repo,
        &state,
        &[
            "ledger", "event", "--run-id", "symlink", "--type", "late", "--json",
        ],
    );
    assert_eq!(blocked.status.code(), Some(1));
    assert_eq!(
        json(&blocked)["diagnostics"][0]["code"],
        "ledger_entry_invalid"
    );
    assert_eq!(fs::read_dir(&external).unwrap().count(), 0);

    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "s", "--run-id", "hardlink"],
    );
    let run_dir = run_dir_of(&state, "s", "hardlink");
    let first = run_dir.join("transactions/00000000000000000001.json");
    let alias = run_dir.join("transactions/00000000000000000002.json");
    fs::hard_link(first, alias).unwrap();
    let blocked = nopal(
        &repo,
        &state,
        &[
            "ledger", "event", "--run-id", "hardlink", "--type", "late", "--json",
        ],
    );
    assert_eq!(blocked.status.code(), Some(1));
    assert_eq!(
        json(&blocked)["diagnostics"][0]["code"],
        "ledger_entry_invalid"
    );
}

#[test]
fn terminal_runs_reject_every_mutation() {
    let (_tmp, repo, state) = setup();
    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "s", "--run-id", "r1"],
    );
    let finalized = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "finalize",
            "--run-id",
            "r1",
            "--status",
            "completed",
            "--json",
        ],
    );
    assert_eq!(finalized.status.code(), Some(0));

    for command in [
        vec![
            "ledger", "event", "--run-id", "r1", "--type", "late", "--json",
        ],
        vec![
            "ledger",
            "checkpoint",
            "--run-id",
            "r1",
            "--name",
            "late",
            "--json",
        ],
        vec![
            "ledger",
            "interrupt",
            "--run-id",
            "r1",
            "--reason",
            "late",
            "--json",
        ],
        vec![
            "ledger", "finalize", "--run-id", "r1", "--status", "failed", "--json",
        ],
    ] {
        let out = nopal(&repo, &state, &command);
        assert_eq!(
            out.status.code(),
            Some(1),
            "terminal mutation unexpectedly succeeded: {command:?}"
        );
        assert_eq!(
            json(&out)["diagnostics"][0]["code"],
            "ledger_transition_invalid"
        );
    }
}

#[test]
fn final_report_and_initial_metadata_are_redacted_before_persistence() {
    let (_tmp, repo, state) = setup();
    let init = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "init",
            "--skill",
            "kickoff",
            "--ticket-title",
            "TOKEN=init-secret",
            "--run-id",
            "r1",
            "--json",
        ],
    );
    assert_eq!(init.status.code(), Some(0));

    let report = repo.join("report.md");
    write_file(
        &report,
        "# Result\n\nAuthorization: Bearer report-secret\nPASSWORD=hunter2\nghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ123456\nsk-abcdefgh12345678\nAKIA1234567890ABCDEF\n-----BEGIN PRIVATE KEY-----\nprivate material\n-----END PRIVATE KEY-----\n",
    );
    let finalized = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "finalize",
            "--run-id",
            "r1",
            "--status",
            "completed",
            "--report-file",
            report.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(finalized.status.code(), Some(0));

    let run_dir = run_dir_of(&state, "kickoff", "r1");
    for entry in tree_files(&run_dir) {
        let bytes = fs::read(run_dir.join(&entry)).unwrap();
        let text = String::from_utf8_lossy(&bytes);
        assert!(!text.contains("init-secret"), "secret in {entry}");
        assert!(!text.contains("report-secret"), "secret in {entry}");
        assert!(!text.contains("hunter2"), "secret in {entry}");
        assert!(
            !text.contains("ghp_ABCDEFGHIJKLMNOPQRSTUVWXYZ123456"),
            "secret in {entry}"
        );
        assert!(!text.contains("sk-abcdefgh12345678"), "secret in {entry}");
        assert!(!text.contains("AKIA1234567890ABCDEF"), "secret in {entry}");
        assert!(!text.contains("private material"), "secret in {entry}");
    }
    assert!(
        fs::read_to_string(run_dir.join("final-report.md"))
            .unwrap()
            .contains("[REDACTED]")
    );
}

#[cfg(unix)]
#[test]
fn symlinked_state_flow_cannot_redirect_run_creation_outside_the_state_root() {
    use std::os::unix::fs::symlink;

    let (_tmp, repo, state) = setup();
    let external = tempfile::tempdir().unwrap();
    fs::create_dir_all(state.join("runs")).unwrap();
    symlink(external.path(), state.join("runs/evil")).unwrap();

    let blocked = nopal(
        &repo,
        &state,
        &[
            "ledger", "init", "--skill", "s", "--flow", "evil", "--run-id", "r1", "--json",
        ],
    );
    assert_ne!(blocked.status.code(), Some(0));
    assert_eq!(fs::read_dir(external.path()).unwrap().count(), 0);
}

#[cfg(unix)]
#[test]
fn symlinked_final_report_is_rejected_without_following_it() {
    use std::os::unix::fs::symlink;

    let (_tmp, repo, state) = setup();
    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "s", "--run-id", "r1"],
    );
    let target = repo.join("secret-report.md");
    write_file(&target, "TOKEN=must-not-copy\n");
    let alias = repo.join("report-link.md");
    symlink(&target, &alias).unwrap();
    let blocked = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "finalize",
            "--run-id",
            "r1",
            "--status",
            "completed",
            "--report-file",
            alias.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(blocked.status.code(), Some(1));
    assert_eq!(
        json(&blocked)["diagnostics"][0]["code"],
        "ledger_entry_invalid"
    );
    let run_dir = run_dir_of(&state, "s", "r1");
    assert!(!run_dir.join("final-report.md").exists());
}

#[test]
fn oversized_event_payload_fails_closed_without_mutating_the_run() {
    let (_tmp, repo, state) = setup();
    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "s", "--run-id", "r1"],
    );
    let run_dir = run_dir_of(&state, "s", "r1");
    let before_run = fs::read(run_dir.join("run.json")).unwrap();
    let before_events = fs::read(run_dir.join("events.jsonl")).unwrap();
    let payload = repo.join("oversized.json");
    write_file(
        &payload,
        &serde_json::json!({ "payload": "x".repeat(256 * 1024) }).to_string(),
    );

    let out = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "event",
            "--run-id",
            "r1",
            "--type",
            "oversized",
            "--json-file",
            payload.to_str().unwrap(),
            "--json",
        ],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        json(&out)["diagnostics"][0]["code"],
        "ledger_limit_exceeded"
    );
    assert_eq!(fs::read(run_dir.join("run.json")).unwrap(), before_run);
    assert_eq!(
        fs::read(run_dir.join("events.jsonl")).unwrap(),
        before_events
    );
}

#[test]
fn ambiguous_run_id_requires_flow() {
    let (_tmp, repo, state) = setup();
    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "a", "--run-id", "same"],
    );
    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "b", "--run-id", "same"],
    );
    let ambiguous = nopal(
        &repo,
        &state,
        &[
            "ledger", "event", "--run-id", "same", "--type", "x", "--json",
        ],
    );
    assert_eq!(ambiguous.status.code(), Some(1));
    assert_eq!(json(&ambiguous)["diagnostics"][0]["code"], "run_ambiguous");
    let scoped = nopal(
        &repo,
        &state,
        &[
            "ledger", "event", "--run-id", "same", "--flow", "b", "--type", "x", "--json",
        ],
    );
    assert_eq!(scoped.status.code(), Some(0));
}

// ---------------------------------------------------------------------------
// prune
// ---------------------------------------------------------------------------

const T0: &str = "1783204993"; // 2026-07-04T22:43:13+00:00
const T0_PLUS_25H: &str = "1783294993"; // +25h

#[test]
fn prune_dry_run_lists_stale_runs_without_writing() {
    let (_tmp, repo, state) = setup();
    nopal_env(
        &repo,
        &state,
        &["ledger", "init", "--skill", "kickoff", "--run-id", "old"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0)],
    );
    nopal_env(
        &repo,
        &state,
        &["ledger", "init", "--skill", "kickoff", "--run-id", "fresh"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0_PLUS_25H)],
    );
    let run_dir = run_dir_of(&state, "kickoff", "old");
    let before = fs::read_to_string(run_dir.join("run.json")).unwrap();

    let dry_run = nopal_env(
        &repo,
        &state,
        &["ledger", "prune", "--json"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0_PLUS_25H)],
    );
    assert_eq!(dry_run.status.code(), Some(0));
    let body = json(&dry_run);
    assert_eq!(body["kind"], "nopal.run_ledger.prune/v1");
    assert_eq!(body["apply"], false);
    assert_eq!(body["selected"], 1);
    assert_eq!(body["applied"], 0);
    assert_eq!(body["candidates"][0]["run_id"], "old");
    assert_eq!(body["candidates"][0]["finalized"], false);

    let after = fs::read_to_string(run_dir.join("run.json")).unwrap();
    assert_eq!(before, after, "dry-run must leave run.json byte-identical");

    let toon = nopal_env(
        &repo,
        &state,
        &["ledger", "prune"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0_PLUS_25H)],
    );
    assert!(stdout(&toon).contains("kind: nopal.run_ledger.prune/v1"));
}

#[test]
fn prune_apply_finalizes_the_selected_run() {
    let (_tmp, repo, state) = setup();
    nopal_env(
        &repo,
        &state,
        &["ledger", "init", "--skill", "kickoff", "--run-id", "old"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0)],
    );

    let apply = nopal_env(
        &repo,
        &state,
        &["ledger", "prune", "--apply", "--json"],
        &[("NOPAL_LEDGER_TEST_EPOCH", T0_PLUS_25H)],
    );
    assert_eq!(apply.status.code(), Some(0));
    let body = json(&apply);
    assert_eq!(body["apply"], true);
    assert_eq!(body["selected"], 1);
    assert_eq!(body["applied"], 1);
    assert_eq!(body["candidates"][0]["finalized"], true);

    let run_dir = run_dir_of(&state, "kickoff", "old");
    let run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(run["status"], "interrupted");
    assert!(run["finalized_at"].as_str().is_some());
}

// ---------------------------------------------------------------------------
// Concurrency (ported from test_run_ledger_concurrency.py)
// ---------------------------------------------------------------------------

fn journal_files(run_dir: &Path) -> Vec<PathBuf> {
    let mut files: Vec<PathBuf> = fs::read_dir(run_dir.join("transactions"))
        .expect("transaction directory")
        .map(|entry| entry.expect("transaction entry").path())
        .filter(|path| path.extension().and_then(|value| value.to_str()) == Some("json"))
        .collect();
    files.sort();
    files
}

#[test]
fn committed_event_repairs_torn_projections_exactly_once() {
    let (_tmp, repo, state) = setup();
    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "repair", "--run-id", "r1"],
    );
    let run_dir = run_dir_of(&state, "repair", "r1");
    let before = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();

    let torn = nopal_env(
        &repo,
        &state,
        &[
            "ledger",
            "event",
            "--run-id",
            "r1",
            "--type",
            "committed",
            "--json",
        ],
        &[("NOPAL_LEDGER_TEST_FAILPOINT", "after_commit")],
    );
    assert_eq!(torn.status.code(), Some(2));
    assert_eq!(
        fs::read_to_string(run_dir.join("events.jsonl")).unwrap(),
        before,
        "the failpoint must leave the committed projection unapplied"
    );

    let repair = nopal(
        &repo,
        &state,
        &[
            "ledger", "event", "--run-id", "r1", "--type", "after", "--json",
        ],
    );
    assert_eq!(repair.status.code(), Some(0));
    let events = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert_eq!(events.matches("\"type\": \"committed\"").count(), 1);
    assert_eq!(events.matches("\"type\": \"after\"").count(), 1);
}

#[test]
fn caller_operation_identity_replays_an_uncertain_event_commit_exactly_once() {
    let (_tmp, repo, state) = setup();
    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "retry", "--run-id", "r1"],
    );
    let run_dir = run_dir_of(&state, "retry", "r1");
    let args = [
        "ledger",
        "event",
        "--run-id",
        "r1",
        "--type",
        "once",
        "--operation-id",
        "event-once-1",
        "--json",
    ];
    let uncertain = nopal_env(
        &repo,
        &state,
        &args,
        &[("NOPAL_LEDGER_TEST_FAILPOINT", "after_commit")],
    );
    assert_eq!(uncertain.status.code(), Some(2));

    let retried = nopal(&repo, &state, &args);
    assert_eq!(retried.status.code(), Some(0), "{retried:?}");
    let events = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert_eq!(events.matches("\"type\": \"once\"").count(), 1);
    assert_eq!(journal_files(&run_dir).len(), 2);

    let conflict = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "event",
            "--run-id",
            "r1",
            "--type",
            "different",
            "--operation-id",
            "event-once-1",
            "--json",
        ],
    );
    assert_eq!(conflict.status.code(), Some(1));
    assert_eq!(
        json(&conflict)["diagnostics"][0]["code"],
        "ledger_entry_invalid"
    );
    assert_eq!(journal_files(&run_dir).len(), 2);
}

#[test]
fn concurrent_event_appends_stay_consistent() {
    let (_tmp, repo, state) = setup();
    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "conc", "--run-id", "r1"],
    );

    std::thread::scope(|scope| {
        for i in 0..20 {
            let repo = &repo;
            let state = &state;
            scope.spawn(move || {
                let out = nopal(
                    repo,
                    state,
                    &[
                        "ledger",
                        "event",
                        "--run-id",
                        "r1",
                        "--type",
                        "tick",
                        "--summary",
                        &format!("tick {i}"),
                    ],
                );
                assert_eq!(out.status.code(), Some(0), "event {i} failed");
            });
        }
    });

    let run_dir = run_dir_of(&state, "conc", "r1");
    let run: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(run_dir.join("run.json")).unwrap()).unwrap();
    // 20 ticks + the run_initialized event.
    assert_eq!(run["events"]["count"], 21);
    let events = fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert_eq!(events.lines().count(), 21);
    let transcript = fs::read_to_string(run_dir.join("transcript.md")).unwrap();
    assert_eq!(transcript.matches("\n## ").count(), 21);

    let transactions = journal_files(&run_dir);
    assert_eq!(transactions.len(), 21);
    for (index, path) in transactions.iter().enumerate() {
        assert_eq!(
            path.file_stem().and_then(|value| value.to_str()),
            Some(format!("{:020}", index + 1).as_str())
        );
        let record: serde_json::Value =
            serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap();
        assert_eq!(record["revision"], index + 1);
        assert!(record["digest"].as_str().unwrap().starts_with("sha256:"));
        if index > 0 {
            let previous: serde_json::Value =
                serde_json::from_str(&fs::read_to_string(&transactions[index - 1]).unwrap())
                    .unwrap();
            assert_eq!(record["previous_digest"], previous["digest"]);
        }
    }
}

#[test]
fn concurrent_gate_attempts_get_distinct_sequential_dirs() {
    let (_tmp, repo, state) = setup();
    nopal(
        &repo,
        &state,
        &["ledger", "init", "--skill", "conc", "--run-id", "r1"],
    );
    let envelope = repo.join("envelope.json");
    write_file(
        &envelope,
        r#"{"status": "pass", "gate": {"name": "fmt", "timestamp": "T"}}"#,
    );

    std::thread::scope(|scope| {
        for i in 0..10 {
            let repo = &repo;
            let state = &state;
            let envelope = &envelope;
            scope.spawn(move || {
                let out = nopal(
                    repo,
                    state,
                    &[
                        "ledger",
                        "gate",
                        "--run-id",
                        "r1",
                        "--name",
                        "fmt",
                        "--envelope-file",
                        envelope.to_str().unwrap(),
                    ],
                );
                assert_eq!(out.status.code(), Some(0), "gate {i} failed");
            });
        }
    });

    let gate_root = run_dir_of(&state, "conc", "r1").join("artifacts/gates/repo/fmt");
    for attempt in 1..=10 {
        assert!(
            gate_root
                .join(attempt.to_string())
                .join("envelope.json")
                .is_file(),
            "missing attempt {attempt}"
        );
    }
    assert!(!gate_root.join("11").exists());
}

// ---------------------------------------------------------------------------
// Interop with the vendored Python reference
// ---------------------------------------------------------------------------

const PY_DRIVER: &str = r#"
import sys
sys.path.insert(0, sys.argv[1])
import run_ledger as rl
rl.now = lambda: "2026-07-04T22:43:13+00:00"
rl.stamp = lambda: "20260704T224313Z"
rl.secrets.token_hex = lambda n: "a7f3c9deadbeef"[: 2 * n]
sys.exit(rl.main(sys.argv[2:]))
"#;

fn python3() -> Option<PathBuf> {
    let out = Command::new("python3").arg("--version").output().ok()?;
    out.status.success().then(|| PathBuf::from("python3"))
}

fn reference_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/reference")
}

/// Every ledger-writing command, with pinned clock and token, driven twice:
/// once through the vendored Python tool, once through nopal. The trees must
/// match byte for byte after normalizing each side's temp root out of the
/// embedded absolute paths.
#[test]
fn write_equivalence_with_python_reference() {
    let Some(python) = python3() else {
        assert!(
            std::env::var_os("CI").is_none(),
            "python3 is required to run the interop equivalence test in CI"
        );
        eprintln!("skipping: python3 not available");
        return;
    };
    // Deliberately NOT canonicalized: on macOS the tempdir sits behind the
    // /var -> /private/var symlink, and Python's Path.resolve() records the
    // resolved form. nopal must resolve the same way or the embedded paths
    // diverge - this test covers exactly that.
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    let repo = root.join("repo");
    fs::create_dir_all(&repo).expect("repo dir");
    let py_state = root.join("py-state");
    let rs_state = root.join("rs-state");

    let payload = repo.join("payload.json");
    // Python-canonical numeric literals plus a DEL char: the historical
    // divergence cases (float exponent format, big-int precision, 0x7f).
    write_file(
        &payload,
        "{\"github_token\": \"secret-value\", \"note\": \"Beisli\\u00f0 ok\", \"n\": 3, \"e30\": 1e+30, \"small\": 1e-07, \"big\": 12345678901234567890123, \"del\": \"a\\u007fb\"}",
    );
    let envelope_fail = repo.join("fail.json");
    write_file(
        &envelope_fail,
        r#"{"status": "fail", "environment_failure": true, "gate": {"name": "fmt", "scope": "Repo Wide", "timestamp": "2026-07-04T22:00:00+00:00"}}"#,
    );
    let envelope_pass = repo.join("pass.json");
    write_file(
        &envelope_pass,
        r#"{"status": "pass", "gate": {"name": "fmt", "scope": "Repo Wide", "timestamp": "2026-07-04T22:10:00+00:00"}}"#,
    );
    let report_md = repo.join("report.md");
    write_file(&report_md, "# final\n\ndone\n");

    let driver = root.join("driver.py");
    write_file(&driver, PY_DRIVER);

    let steps: Vec<Vec<String>> = vec![
        vec![
            "init",
            "--skill",
            "kickoff",
            "--flow",
            "kickoff",
            "--ticket-id",
            "TASK-19",
            "--ticket-title",
            "Equivalence Check",
            "--branch",
            "feature/eq",
            "--run-id",
            "eq-run",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        vec![
            "event",
            "--run-id",
            "eq-run",
            "--type",
            "step",
            "--json-file",
            payload.to_str().unwrap(),
            "--summary",
            "did TOKEN=abc123 things",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        vec![
            "checkpoint",
            "--run-id",
            "eq-run",
            "--name",
            "ctx ready",
            "--json-file",
            payload.to_str().unwrap(),
            "--resume-hint",
            "resume at step 2",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        vec![
            "gate",
            "--run-id",
            "eq-run",
            "--name",
            "fmt",
            "--envelope-file",
            envelope_fail.to_str().unwrap(),
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        vec![
            "gate",
            "--run-id",
            "eq-run",
            "--name",
            "fmt",
            "--envelope-file",
            envelope_pass.to_str().unwrap(),
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        vec![
            "interrupt",
            "--run-id",
            "eq-run",
            "--reason",
            "pausing PASSWORD=hunter2",
            "--resume-hint",
            "continue at gate",
        ]
        .into_iter()
        .map(String::from)
        .collect(),
        vec![
            "finalize",
            "--run-id",
            "eq-run",
            "--status",
            "failed",
            "--report-file",
            report_md.to_str().unwrap(),
        ]
        .into_iter()
        .map(String::from)
        .collect(),
    ];

    for step in &steps {
        let py = Command::new(&python)
            .arg(&driver)
            .arg(reference_dir())
            .args(step)
            .current_dir(&repo)
            .env("BEISLID_STATE_DIR", &py_state)
            .output()
            .expect("spawn python reference");
        assert!(
            py.status.success(),
            "python step {step:?} failed: {}",
            String::from_utf8_lossy(&py.stderr)
        );

        let mut rust_args: Vec<&str> = vec!["ledger"];
        rust_args.extend(step.iter().map(String::as_str));
        let rs = Command::new(env!("CARGO_BIN_EXE_nopal"))
            .arg("--dir")
            .arg(&repo)
            .args(&rust_args)
            .env("BEISLID_STATE_DIR", &rs_state)
            .env("NOPAL_LEDGER_TEST_EPOCH", "1783204993")
            .env("NOPAL_LEDGER_TEST_TOKEN", "a7f3c9")
            .output()
            .expect("spawn nopal");
        assert_eq!(
            rs.status.code(),
            Some(0),
            "nopal step {step:?} failed: {}",
            String::from_utf8_lossy(&rs.stderr)
        );
    }

    let py_run = py_state.join("runs/kickoff/unknown-repo/eq-run");
    let rs_run = rs_state.join("runs/kickoff/unknown-repo/eq-run");
    let py_files = tree_files(&py_run);
    let rs_files: Vec<String> = tree_files(&rs_run)
        .into_iter()
        .filter(|path| !path.starts_with("transactions/"))
        .collect();
    assert_eq!(py_files, rs_files, "legacy projection tree shapes differ");
    assert!(
        rs_run
            .join("transactions/00000000000000000001.json")
            .is_file(),
        "native writes must add an authoritative journal"
    );

    // Both tools embed the RESOLVED state roots, so normalize those out.
    let py_state_resolved = fs::canonicalize(&py_state).expect("py state resolves");
    let rs_state_resolved = fs::canonicalize(&rs_state).expect("rs state resolves");
    for rel in &py_files {
        let py_text = fs::read_to_string(py_run.join(rel)).unwrap();
        let rs_text = fs::read_to_string(rs_run.join(rel)).unwrap();
        let py_norm = py_text.replace(py_state_resolved.to_str().unwrap(), "<STATE>");
        let rs_norm = rs_text.replace(rs_state_resolved.to_str().unwrap(), "<STATE>");
        assert_eq!(py_norm, rs_norm, "content differs for {rel}");
    }
}

/// Relative paths of all regular files in a run tree, lock file excluded
/// (Python only creates `.lock` once a locked command runs; presence differs
/// by construction order, and it is empty on both sides anyway).
fn tree_files(root: &Path) -> Vec<String> {
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else if path.file_name().and_then(|n| n.to_str()) != Some(".lock") {
                files.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                );
            }
        }
    }
    files.sort();
    files
}

// ---------------------------------------------------------------------------
// pointer (WS-CORE)
// ---------------------------------------------------------------------------

#[test]
fn pointer_reports_ok_empty_when_neither_file_exists() {
    let (_tmp, repo, state) = setup();
    let out = nopal(&repo, &state, &["--json", "ledger", "pointer"]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.run_ledger.pointer/v1");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["source"], serde_json::Value::Null);
    assert_eq!(doc["entries"], serde_json::json!([]));
}

#[test]
fn pointer_prefers_nopal_location_over_beislid_fallback() {
    let (_tmp, repo, _state) = setup();
    fs::create_dir_all(repo.join(".nopal/checkpoints")).unwrap();
    write_file(
        &repo.join(".nopal/checkpoints/latest.json"),
        r#"{ "latest": { "kickoff_start": {
            "event": "kickoff_start", "path": "plans/from-nopal.md",
            "source_skill": "kickoff", "written_at": "2026-07-06T00:00:00Z"
        } } }"#,
    );
    fs::create_dir_all(repo.join(".beislid/checkpoints")).unwrap();
    write_file(
        &repo.join(".beislid/checkpoints/latest.json"),
        r#"{ "latest": { "kickoff_start": {
            "event": "kickoff_start", "path": "plans/from-beislid.md"
        } } }"#,
    );

    // Even a bogus state dir must not affect a repo-local read.
    let out = nopal(
        &repo,
        &PathBuf::from("/nonexistent-state-dir"),
        &["--json", "ledger", "pointer"],
    );
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["source"], ".nopal/checkpoints/latest.json");
    assert_eq!(doc["entries"][0]["path"], "plans/from-nopal.md");
}

#[test]
fn pointer_falls_back_to_beislid_location_when_nopal_pointer_absent() {
    let (_tmp, repo, state) = setup();
    fs::create_dir_all(repo.join(".beislid/checkpoints")).unwrap();
    write_file(
        &repo.join(".beislid/checkpoints/latest.json"),
        r#"{ "latest": { "spec_approved": {
            "event": "spec_approved", "path": "plans/x-spec.md",
            "ticket": { "id": "TASK-1", "title": "T" },
            "branch": "nopal/x", "source_skill": "spec",
            "written_at": "2026-07-06T00:00:00Z"
        } } }"#,
    );

    let out = nopal(&repo, &state, &["--json", "ledger", "pointer"]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["source"], ".beislid/checkpoints/latest.json");
    let entry = &doc["entries"][0];
    assert_eq!(entry["event"], "spec_approved");
    assert_eq!(entry["path"], "plans/x-spec.md");
    assert_eq!(entry["branch"], "nopal/x");
    assert_eq!(entry["source_skill"], "spec");
    assert_eq!(entry["ticket"]["id"], "TASK-1");
}

#[test]
fn pointer_drops_unsafe_paths_with_a_warning_diagnostic() {
    let (_tmp, repo, state) = setup();
    fs::create_dir_all(repo.join(".nopal/checkpoints")).unwrap();
    write_file(
        &repo.join(".nopal/checkpoints/latest.json"),
        r#"{ "latest": {
            "absolute": { "event": "absolute", "path": "/etc/passwd" },
            "traversal": { "event": "traversal", "path": "../../secret.md" },
            "empty": { "event": "empty", "path": "" },
            "safe": { "event": "safe", "path": "plans/ok.md" }
        } }"#,
    );

    let out = nopal(&repo, &state, &["--json", "ledger", "pointer"]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    let entries = doc["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["event"], "safe");
    let diagnostics = doc["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 3);
    assert!(diagnostics.iter().all(|d| d["severity"] == "warning"));
}

#[test]
fn pointer_malformed_json_exits_nonzero_with_a_diagnostic() {
    let (_tmp, repo, state) = setup();
    fs::create_dir_all(repo.join(".nopal/checkpoints")).unwrap();
    write_file(&repo.join(".nopal/checkpoints/latest.json"), "{ not json");

    let out = nopal(&repo, &state, &["--json", "ledger", "pointer"]);
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["diagnostics"][0]["code"], "ledger_entry_invalid");
}
