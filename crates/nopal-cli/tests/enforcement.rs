#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

const RECEIPT_KEY: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn fixture(root: &Path) {
    write(
        &root.join(".nopal/policy.jsonc"),
        r#"{
          "version": "nopal.policy/v1",
          "modes": { "supervised_auto": { "rules": [
            { "id": "allow-push", "actions": ["git.push"], "decision": "allow" },
            { "id": "deny-force", "actions": ["git.push_force"], "decision": "deny" }
          ] } }
        }"#,
    );
    write(
        &root.join(".nopal/gates.jsonc"),
        r#"{
          "version": "nopal.gates/v1",
          "gates": [{ "id": "proof", "stage": "pre_pr", "argv": ["true"] }]
        }"#,
    );
    write(&root.join("source.txt"), "first\n");
}

fn run(root: &Path, state: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", root.to_str().unwrap(), "--json"])
        .args(args)
        .env("BEISLID_STATE_DIR", state)
        .env("NOPAL_CONFIG_DIR", state.join("config"))
        .output()
        .unwrap()
}

fn json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "invalid JSON: {error}; stdout={}; stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

#[test]
fn cli_records_decisions_and_reuses_only_current_gate_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fixture(temp.path());

    let initialized = run(
        temp.path(),
        state.path(),
        &[
            "ledger",
            "init",
            "--skill",
            "nopal",
            "--flow",
            "enforcement",
            "--run-id",
            "cli-proof",
        ],
    );
    assert!(initialized.status.success(), "{initialized:?}");
    let run_root = state.path().join("runs/enforcement/unknown-repo/cli-proof");
    write(
        &run_root.join("artifacts/enforcement/receipt-capability"),
        RECEIPT_KEY,
    );

    let plan_args = [
        "enforcement",
        "plan",
        "--mode",
        "supervised_auto",
        "--action",
        "git.push",
        "--class",
        "git_remote",
        "--run-id",
        "cli-proof",
    ];
    let first = run(temp.path(), state.path(), &plan_args);
    assert!(first.status.success(), "{first:?}");
    let first_json = json(&first);
    assert_eq!(first_json["required_gates"][0]["id"], "proof");
    let contract_digest = first_json["contract_digest"].as_str().unwrap().to_owned();
    let workspace_fingerprint = first_json["workspace_fingerprint"]
        .as_str()
        .unwrap()
        .to_owned();
    let gate_definition_digest = first_json["receipts"][0]["gate_definition_digest"]
        .as_str()
        .unwrap()
        .to_owned();

    let recorded = run(
        temp.path(),
        state.path(),
        &[
            "enforcement",
            "record-gate",
            "--mode",
            "supervised_auto",
            "--action",
            "git.push",
            "--class",
            "git_remote",
            "--run-id",
            "cli-proof",
            "--gate-id",
            "proof",
            "--exit-code",
            "0",
            "--contract-digest",
            &contract_digest,
            "--workspace-fingerprint",
            &workspace_fingerprint,
            "--gate-definition-digest",
            &gate_definition_digest,
        ],
    );
    assert!(recorded.status.success(), "{recorded:?}");

    let current = run(temp.path(), state.path(), &plan_args);
    assert!(current.status.success(), "{current:?}");
    assert_eq!(json(&current)["required_gates"], serde_json::json!([]));

    write(&temp.path().join("source.txt"), "changed\n");
    let stale = run(temp.path(), state.path(), &plan_args);
    assert_eq!(json(&stale)["required_gates"][0]["id"], "proof");

    let force = run(
        temp.path(),
        state.path(),
        &[
            "enforcement",
            "plan",
            "--mode",
            "supervised_auto",
            "--action",
            "git.push_force",
            "--class",
            "git_remote",
            "--run-id",
            "cli-proof",
        ],
    );
    assert!(force.status.success(), "{force:?}");
    assert_eq!(json(&force)["decision"], "deny");

    let events = fs::read_to_string(run_root.join("events.jsonl")).unwrap();
    assert!(events.contains("action_decision"));
    assert!(events.contains("gate_attempt"));
    assert!(events.contains("gate_receipt"));
}
