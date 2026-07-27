#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use nopal_core::enforcement::{self, EnforcementRequest};
use nopal_core::policy::{ActionClass, Mode};
use nopal_core::run_ledger_store::{self, InitArgs, LedgerEnv};

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn project(root: &Path) {
    write(
        &root.join(".nopal/policy.jsonc"),
        r#"{
          "version": "nopal.policy/v1",
          "modes": { "supervised_auto": { "rules": [
            { "id": "allow-remote", "classes": ["git_remote"], "decision": "allow" },
            { "id": "deny-force", "actions": ["git.push_force"], "decision": "deny" }
          ] } }
        }"#,
    );
    write(
        &root.join(".nopal/gates.jsonc"),
        r#"{
          "version": "nopal.gates/v1",
          "gates": [{ "id": "proof", "stage": "pre_pr", "command": "true" }]
        }"#,
    );
}

#[test]
fn normal_push_requires_pre_pr_gate_while_force_push_is_denied() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());

    let normal = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
    })
    .unwrap();
    assert!(normal.ok, "{:?}", normal.diagnostics);
    assert_eq!(normal.decision.as_str(), "allow");
    assert_eq!(normal.required_gates.len(), 1);
    assert_eq!(normal.required_gates[0].id, "proof");

    let force = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push_force",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
    })
    .unwrap();
    assert!(force.ok, "{:?}", force.diagnostics);
    assert_eq!(force.decision.as_str(), "deny");
    assert!(force.required_gates.is_empty());
}

#[test]
fn workflow_policy_can_tighten_but_not_weaken_user_policy() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    let config = temp.path().join("config");
    write(
        &config.join("policy.jsonc"),
        r#"{
          "version": "nopal.policy/v1",
          "modes": { "supervised_auto": { "rules": [
            { "id": "user-deny-force", "actions": ["git.push_force"], "decision": "deny" },
            { "id": "user-allow-push", "actions": ["git.push"], "decision": "allow" }
          ] } }
        }"#,
    );
    write(
        &temp.path().join(".beislid/workflow.md"),
        r#"Prose cannot grant authority.

```beislid:action_policy
modes:
  supervised-auto:
    actions:
      git.push_force: allow
      git.push: deny
```
"#,
    );

    for action in ["git.push", "git.push_force"] {
        let report = enforcement::plan(EnforcementRequest {
            root: temp.path(),
            config_dir: Some(&config),
            mode: Mode::SupervisedAuto,
            action,
            classes: &[ActionClass::GitRemote],
            run_dir: None,
        })
        .unwrap();
        assert!(report.ok, "{:?}", report.diagnostics);
        assert_eq!(report.decision.as_str(), "deny", "{action}");
    }
}

#[test]
fn passing_receipt_is_reused_until_workspace_content_changes() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    write(&temp.path().join("source.txt"), "first\n");
    let state = tempfile::tempdir().unwrap();
    let ledger = LedgerEnv::discover(temp.path(), Some(state.path()));
    let run = run_ledger_store::init_run(
        &ledger,
        &InitArgs {
            skill: "nopal",
            flow: Some("enforcement"),
            ticket_id: "none",
            ticket_title: "Nopal session",
            ticket_url: "",
            branch: Some("test"),
            run_id: Some("receipt-test"),
        },
    )
    .unwrap();

    let request = || EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: Some(&run.run_dir),
    };
    assert_eq!(
        enforcement::plan(request()).unwrap().required_gates.len(),
        1
    );
    enforcement::record_gate(request(), &run.run_dir, "proof", 0).unwrap();
    assert!(
        enforcement::plan(request())
            .unwrap()
            .required_gates
            .is_empty()
    );

    write(&temp.path().join("source.txt"), "changed\n");
    assert_eq!(
        enforcement::plan(request()).unwrap().required_gates.len(),
        1
    );
    let events = fs::read_to_string(run.run_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("gate_attempt"));
    assert!(events.contains("gate_receipt"));
}

#[test]
fn malformed_recognized_workflow_block_fails_closed_but_unknown_block_warns() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    write(
        &temp.path().join(".beislid/workflow.md"),
        "```beislid:future_enforcement\nanything: true\n```\n",
    );
    let warning = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
    })
    .unwrap();
    assert!(warning.ok, "{:?}", warning.diagnostics);
    assert!(
        warning
            .diagnostics
            .iter()
            .any(|d| d.severity.as_str() == "warning")
    );

    write(
        &temp.path().join(".beislid/workflow.md"),
        "```beislid:future_enforcement\nanything: true\n",
    );
    let malformed_unknown = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
    })
    .unwrap();
    assert!(malformed_unknown.ok, "{:?}", malformed_unknown.diagnostics);

    write(
        &temp.path().join(".beislid/workflow.md"),
        "```beislid:action_policy\nmodes:\n  supervised-auto:\n    actions:\n      git.push: allow\n",
    );
    let invalid = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
    })
    .unwrap();
    assert!(!invalid.ok);
    assert!(invalid.required_gates.is_empty());
}
