#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use nopal_core::enforcement::{self, EnforcementRequest};
use nopal_core::policy::{ActionClass, Mode};
use nopal_core::run_ledger_store::{self, InitArgs, LedgerEnv};

const RECEIPT_KEY: &[u8] = b"0123456789abcdef0123456789abcdef";

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
fn explicit_nopal_or_beislid_gates_supersede_generated_templates() {
    for authority in ["nopal", "beislid"] {
        let temp = tempfile::tempdir().unwrap();
        project(temp.path());
        write(&temp.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n");
        let plan = nopal_core::gate_scaffold::inspect(temp.path()).unwrap();
        let mut gates: serde_json::Value =
            serde_json::from_str(&plan.gates_json().unwrap()).unwrap();
        if authority == "nopal" {
            gates["gates"]
                .as_array_mut()
                .unwrap()
                .push(serde_json::json!({
                    "id": "explicit-nopal",
                    "stage": "pre_pr",
                    "argv": ["true"]
                }));
        } else {
            write(
                &temp.path().join(".beislid/workflow.md"),
                "```beislid:gates\n- name: explicit-beislid\n  command: 'cargo test'\n```\n",
            );
        }
        write(
            &temp.path().join(".nopal/gates.jsonc"),
            &serde_json::to_string_pretty(&gates).unwrap(),
        );

        let report = enforcement::plan(EnforcementRequest {
            root: temp.path(),
            config_dir: None,
            mode: Mode::SupervisedAuto,
            action: "git.push",
            classes: &[ActionClass::GitRemote],
            run_dir: None,
            receipt_key: None,
        })
        .unwrap();
        assert!(report.ok, "{authority}: {:?}", report.diagnostics);
        assert_eq!(report.required_gates.len(), 1, "{authority}");
        assert_eq!(report.required_gates[0].id, format!("explicit-{authority}"));
    }
}

#[test]
#[cfg(unix)]
fn symlinked_authority_files_and_directories_fail_closed_without_following_targets() {
    use std::os::unix::fs::symlink;

    for authority in ["gates", "workflow", "directory"] {
        let temp = tempfile::tempdir().unwrap();
        project(temp.path());
        let outside = temp.path().join(format!("outside-{authority}"));
        if authority == "gates" {
            write(
                &outside,
                r#"{"version":"nopal.gates/v1","gates":[{"id":"outside","stage":"pre_pr","argv":["true"]}]}"#,
            );
            fs::remove_file(temp.path().join(".nopal/gates.jsonc")).unwrap();
            symlink(&outside, temp.path().join(".nopal/gates.jsonc")).unwrap();
        } else if authority == "workflow" {
            write(
                &outside,
                "```beislid:gates\n- name: outside\n  command: 'cargo test'\n```\n",
            );
            fs::create_dir_all(temp.path().join(".beislid")).unwrap();
            symlink(&outside, temp.path().join(".beislid/workflow.md")).unwrap();
        } else {
            write(
                &outside.join("policy.jsonc"),
                &fs::read_to_string(temp.path().join(".nopal/policy.jsonc")).unwrap(),
            );
            write(
                &outside.join("gates.jsonc"),
                r#"{"version":"nopal.gates/v1","gates":[{"id":"outside","stage":"pre_pr","argv":["true"]}]}"#,
            );
            fs::remove_dir_all(temp.path().join(".nopal")).unwrap();
            symlink(&outside, temp.path().join(".nopal")).unwrap();
        }
        let report = enforcement::plan(EnforcementRequest {
            root: temp.path(),
            config_dir: None,
            mode: Mode::SupervisedAuto,
            action: "git.push",
            classes: &[ActionClass::GitRemote],
            run_dir: None,
            receipt_key: None,
        })
        .unwrap();
        assert!(!report.ok, "{authority}: {:?}", report.diagnostics);
        assert!(report.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == nopal_core::diagnostics::Code::ModuleParseError
        }));
    }
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
        receipt_key: None,
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
        receipt_key: None,
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
            receipt_key: None,
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
        receipt_key: Some(RECEIPT_KEY),
    };
    let initial = enforcement::plan(request()).unwrap();
    assert_eq!(initial.required_gates.len(), 1);
    let receipt = initial
        .receipts
        .iter()
        .find(|receipt| receipt.gate_id == "proof")
        .unwrap();
    enforcement::record_gate(
        request(),
        &run.run_dir,
        "proof",
        0,
        &enforcement::GateExecutionContext {
            contract_digest: initial.contract_digest.clone(),
            workspace_fingerprint: initial.workspace_fingerprint.clone(),
            gate_definition_digest: receipt.gate_definition_digest.clone(),
        },
    )
    .unwrap();
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
fn unsigned_receipt_cannot_forge_passing_evidence() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
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
            run_id: Some("forged-receipt"),
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
        receipt_key: Some(RECEIPT_KEY),
    };
    let initial = enforcement::plan(request()).unwrap();
    let receipt = initial
        .receipts
        .iter()
        .find(|value| value.gate_id == "proof")
        .unwrap();
    write(
        &run.run_dir
            .join("artifacts/enforcement/receipts/proof.json"),
        &serde_json::json!({
            "action": "git.push",
            "contract_digest": initial.contract_digest,
            "exit_code": 0,
            "gate_id": "proof",
            "gate_definition_digest": receipt.gate_definition_digest,
            "workspace_fingerprint": initial.workspace_fingerprint,
        })
        .to_string(),
    );

    assert_eq!(
        enforcement::plan(request()).unwrap().required_gates.len(),
        1
    );
}

#[test]
fn gate_result_is_rejected_when_definition_changes_during_execution() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
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
            run_id: Some("changed-gate"),
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
        receipt_key: Some(RECEIPT_KEY),
    };
    let initial = enforcement::plan(request()).unwrap();
    let receipt = initial
        .receipts
        .iter()
        .find(|value| value.gate_id == "proof")
        .unwrap();
    let context = enforcement::GateExecutionContext {
        contract_digest: initial.contract_digest,
        workspace_fingerprint: initial.workspace_fingerprint,
        gate_definition_digest: receipt.gate_definition_digest.clone(),
    };
    write(
        &temp.path().join(".nopal/gates.jsonc"),
        r#"{
          "version": "nopal.gates/v1",
          "gates": [{ "id": "proof", "stage": "pre_pr", "command": "false" }]
        }"#,
    );

    let error =
        enforcement::record_gate(request(), &run.run_dir, "proof", 0, &context).unwrap_err();
    assert!(error.to_string().contains("changed during execution"));
    assert_eq!(
        enforcement::plan(request()).unwrap().required_gates.len(),
        1
    );
}

#[test]
#[cfg(unix)]
fn receipt_capability_is_private_run_state_and_cannot_be_reinitialized() {
    use std::os::unix::fs::PermissionsExt;

    let run = tempfile::tempdir().unwrap();
    enforcement::initialize_receipt_capability(run.path()).unwrap();
    let capability = enforcement::load_receipt_capability(run.path()).unwrap();
    assert_eq!(capability.len(), 64);
    let path = run.path().join("artifacts/enforcement/receipt-capability");
    assert_eq!(
        fs::metadata(path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert!(enforcement::initialize_receipt_capability(run.path()).is_err());
}

#[test]
#[cfg(unix)]
fn external_workspace_symlink_fails_closed_instead_of_reusing_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    project(temp.path());
    write(&external.path().join("input.txt"), "outside\n");
    std::os::unix::fs::symlink(
        external.path().join("input.txt"),
        temp.path().join("external-input"),
    )
    .unwrap();

    let error = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
        receipt_key: None,
    })
    .unwrap_err();
    assert!(
        error
            .to_string()
            .contains("escapes the repository proof surface")
    );
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
        receipt_key: None,
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
        receipt_key: None,
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
        receipt_key: None,
    })
    .unwrap();
    assert!(!invalid.ok);
    assert!(invalid.required_gates.is_empty());
}
