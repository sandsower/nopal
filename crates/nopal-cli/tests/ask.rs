// Integration tests may panic freely; clippy's in-tests allowance only covers
// #[test] fns, not shared helpers in the tests/ tree.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `nopal ask` integration tests: cross-process visibility of a
//! pending ask, poll-based unblocking through `await`, expiry failing closed
//! across processes, double-resolve rejection, and redaction of ask context.

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

fn nopal(repo: &Path, state: &Path, args: &[&str]) -> Output {
    nopal_env(repo, state, args, &[])
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

fn raise(repo: &Path, state: &Path, extra: &[&str]) -> String {
    let mut args = vec![
        "ask",
        "raise",
        "--session",
        "sess-1",
        "--mode",
        "unattended_auto",
        "--action",
        "git.push",
        "--rule",
        "ask-push",
        "--reason",
        "needs a push",
        "--json",
    ];
    args.extend_from_slice(extra);
    let out = nopal(repo, state, &args);
    assert_eq!(
        out.status.code(),
        Some(0),
        "raise failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v = json(&out);
    assert_eq!(v["kind"], "nopal.ask.raise/v1");
    assert_eq!(v["state"], "pending");
    assert_eq!(v["effective_decision"], "deny");
    v["ask_id"].as_str().expect("ask_id").to_owned()
}

// ---------------------------------------------------------------------------
// Cross-process visibility and poll-based unblocking
// ---------------------------------------------------------------------------

#[test]
fn ask_raised_in_one_process_is_visible_and_resolvable_in_another() {
    let (_tmp, repo, state) = setup();
    let ask_id = raise(&repo, &state, &["--ttl-seconds", "0"]);

    // A separate process lists it as pending.
    let listed = nopal(&repo, &state, &["ask", "list", "--json"]);
    assert_eq!(listed.status.code(), Some(0));
    let lv = json(&listed);
    assert_eq!(lv["total"], 1);
    assert_eq!(lv["asks"][0]["ask_id"], ask_id.as_str());
    assert_eq!(lv["asks"][0]["state"], "pending");

    // The blocked caller polls: still pending -> exit 4, fail closed.
    let waiting = nopal(
        &repo,
        &state,
        &[
            "ask",
            "await",
            "--id",
            &ask_id,
            "--timeout-seconds",
            "0",
            "--json",
        ],
    );
    assert_eq!(waiting.status.code(), Some(4));
    assert_eq!(json(&waiting)["timed_out"], true);

    // A second process approves.
    let resolved = nopal(
        &repo,
        &state,
        &[
            "ask",
            "resolve",
            "--id",
            &ask_id,
            "--decision",
            "approve",
            "--by",
            "vic",
            "--json",
        ],
    );
    assert_eq!(resolved.status.code(), Some(0));
    assert_eq!(json(&resolved)["effective_decision"], "allow");

    // The original caller's next poll unblocks: exit 0.
    let unblocked = nopal(
        &repo,
        &state,
        &[
            "ask",
            "await",
            "--id",
            &ask_id,
            "--timeout-seconds",
            "0",
            "--json",
        ],
    );
    assert_eq!(unblocked.status.code(), Some(0));
    let uv = json(&unblocked);
    assert_eq!(uv["state"], "approved");
    assert_eq!(uv["effective_decision"], "allow");
}

#[test]
fn denied_ask_unblocks_await_fail_closed() {
    let (_tmp, repo, state) = setup();
    let ask_id = raise(&repo, &state, &["--ttl-seconds", "0"]);
    nopal(
        &repo,
        &state,
        &[
            "ask",
            "resolve",
            "--id",
            &ask_id,
            "--decision",
            "deny",
            "--by",
            "vic",
        ],
    );
    let waited = nopal(
        &repo,
        &state,
        &[
            "ask",
            "await",
            "--id",
            &ask_id,
            "--timeout-seconds",
            "0",
            "--json",
        ],
    );
    assert_eq!(waited.status.code(), Some(3));
    assert_eq!(json(&waited)["effective_decision"], "deny");
}

#[test]
fn double_resolve_is_rejected() {
    let (_tmp, repo, state) = setup();
    let ask_id = raise(&repo, &state, &["--ttl-seconds", "0"]);
    let first = nopal(
        &repo,
        &state,
        &[
            "ask",
            "resolve",
            "--id",
            &ask_id,
            "--decision",
            "approve",
            "--by",
            "vic",
            "--json",
        ],
    );
    assert_eq!(first.status.code(), Some(0));
    let second = nopal(
        &repo,
        &state,
        &[
            "ask",
            "resolve",
            "--id",
            &ask_id,
            "--decision",
            "deny",
            "--by",
            "vic",
            "--json",
        ],
    );
    assert_eq!(second.status.code(), Some(1));
    assert_eq!(
        json(&second)["diagnostics"][0]["code"],
        "ask_already_resolved"
    );
}

// ---------------------------------------------------------------------------
// Expiry fails closed across processes
// ---------------------------------------------------------------------------

#[test]
fn expiry_fails_closed_across_processes() {
    let (_tmp, repo, state) = setup();
    // Raise at a pinned epoch with a short ttl.
    let raised = nopal_env(
        &repo,
        &state,
        &[
            "ask",
            "raise",
            "--session",
            "afk",
            "--mode",
            "unattended_auto",
            "--action",
            "git.push",
            "--reason",
            "afk push",
            "--ttl-seconds",
            "60",
            "--json",
        ],
        &[("NOPAL_LEDGER_TEST_EPOCH", "1783204993")],
    );
    assert_eq!(raised.status.code(), Some(0));
    let ask_id = json(&raised)["ask_id"].as_str().unwrap().to_owned();

    // A later process (clock advanced well past the deadline) observes expiry.
    let later = "1783208593"; // +3600s
    let awaited = nopal_env(
        &repo,
        &state,
        &[
            "ask",
            "await",
            "--id",
            &ask_id,
            "--timeout-seconds",
            "0",
            "--json",
        ],
        &[("NOPAL_LEDGER_TEST_EPOCH", later)],
    );
    assert_eq!(awaited.status.code(), Some(3), "expired must fail closed");
    let av = json(&awaited);
    assert_eq!(av["state"], "expired");
    assert_eq!(av["effective_decision"], "deny");

    // And an expired ask can no longer be approved.
    let resolve = nopal_env(
        &repo,
        &state,
        &[
            "ask",
            "resolve",
            "--id",
            &ask_id,
            "--decision",
            "approve",
            "--by",
            "vic",
            "--json",
        ],
        &[("NOPAL_LEDGER_TEST_EPOCH", later)],
    );
    assert_eq!(resolve.status.code(), Some(1));
    assert_eq!(json(&resolve)["diagnostics"][0]["code"], "ask_expired");
}

// ---------------------------------------------------------------------------
// Redaction and run-ledger audit trail
// ---------------------------------------------------------------------------

#[test]
fn ask_context_is_redacted_and_run_events_are_written() {
    let (_tmp, repo, state) = setup();

    // A backing run so ask lifecycle events land in its ledger.
    let init = nopal(
        &repo,
        &state,
        &[
            "ledger",
            "init",
            "--skill",
            "kickoff",
            "--run-id",
            "r1",
            "--branch",
            "feature/x",
            "--json",
        ],
    );
    assert_eq!(init.status.code(), Some(0));
    let run_dir: PathBuf = json(&init)["run_dir"].as_str().unwrap().into();

    let raised = nopal(
        &repo,
        &state,
        &[
            "ask",
            "raise",
            "--session",
            "sess-1",
            "--run-id",
            "r1",
            "--flow",
            "kickoff",
            "--mode",
            "unattended_auto",
            "--action",
            "git.push",
            "--reason",
            "push needs TOKEN=leakme now",
            "--ttl-seconds",
            "0",
            "--json",
        ],
    );
    assert_eq!(raised.status.code(), Some(0));
    let ask_id = json(&raised)["ask_id"].as_str().unwrap().to_owned();

    // show: the free-text reason is redacted.
    let shown = nopal(&repo, &state, &["ask", "show", "--id", &ask_id, "--json"]);
    assert_eq!(
        json(&shown)["ask"]["reason"],
        "push needs TOKEN=[REDACTED] now"
    );

    nopal(
        &repo,
        &state,
        &[
            "ask",
            "resolve",
            "--id",
            &ask_id,
            "--decision",
            "approve",
            "--by",
            "vic",
            "--note",
            "ok secret=hunter2",
        ],
    );

    // The run ledger carries the ask lifecycle events, redacted.
    let events = std::fs::read_to_string(run_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("\"type\": \"ask_raised\""), "{events}");
    assert!(events.contains("\"type\": \"ask_resolved\""), "{events}");
    assert!(
        !events.contains("hunter2"),
        "resolution note leaked: {events}"
    );
    assert!(!events.contains("leakme"), "ask reason leaked: {events}");
}
