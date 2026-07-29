#![allow(clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use nopal_core::enforcement::{
    self, ENFORCEMENT_INTENT_KIND, EnforcementIntent, EnforcementRequest,
};
use nopal_core::policy::{ActionClass, Mode};
use nopal_core::run_ledger_store::{self, InitArgs, LedgerEnv};

const RECEIPT_KEY: &[u8] = b"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const CANONICAL_PROJECT_ROOT: &str = "tests/fixtures/enforcement/project";

fn canonical_intent() -> EnforcementIntent {
    serde_json::from_str(include_str!("fixtures/enforcement/canonical-intent.json")).unwrap()
}

fn pretty_json(value: &impl serde::Serialize) -> Vec<u8> {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    bytes
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn apply_evidence(run_dir: &Path, directive: enforcement::EvidenceDirective) {
    for effect in directive.effects {
        match effect {
            enforcement::EvidenceEffect::AppendEvent { event, payload } => {
                run_ledger_store::append_event(run_dir, &event, &payload, None).unwrap();
            }
            enforcement::EvidenceEffect::WriteJson {
                relative_path,
                payload,
            } => {
                run_ledger_store::write_json_durable(&run_dir.join(relative_path), &payload)
                    .unwrap();
            }
            enforcement::EvidenceEffect::CreateJson {
                relative_path,
                payload,
            } => {
                let path = run_dir.join(relative_path);
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                let file = fs::OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(path)
                    .unwrap();
                serde_json::to_writer(file, &payload).unwrap();
            }
            enforcement::EvidenceEffect::RemoveFile {
                relative_path,
                ignore_missing,
            } => {
                if let Err(error) = fs::remove_file(run_dir.join(relative_path)) {
                    assert!(ignore_missing && error.kind() == std::io::ErrorKind::NotFound);
                }
            }
        }
    }
}

fn intent(tool_name: &str, changed_files: &[&str], mutates: bool) -> EnforcementIntent {
    EnforcementIntent {
        kind: ENFORCEMENT_INTENT_KIND.to_owned(),
        launch_id: "launch-1".to_owned(),
        session_id: "session-1".to_owned(),
        tool_call_id: format!("call-{tool_name}"),
        tool_name: tool_name.to_owned(),
        input_digest: format!("input-{tool_name}"),
        target_digest: format!("target-{tool_name}"),
        executor_digest: "executor-test".to_owned(),
        changed_files: changed_files
            .iter()
            .map(|path| (*path).to_owned())
            .collect(),
        workspace_fingerprint: None,
        mutates,
    }
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
fn canonical_enforcement_artifacts_remain_byte_stable() {
    let root = Path::new(CANONICAL_PROJECT_ROOT);
    assert!(
        root.is_dir(),
        "missing canonical enforcement project fixture"
    );
    let exact_intent = canonical_intent();
    assert_eq!(
        pretty_json(&exact_intent),
        include_bytes!("fixtures/enforcement/canonical-intent.json")
    );

    let request = || EnforcementRequest {
        root,
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
        receipt_key: Some(RECEIPT_KEY),
    };
    let plan = enforcement::plan_for_intent(request(), exact_intent.clone()).unwrap();
    assert_eq!(
        pretty_json(&plan),
        include_bytes!("fixtures/enforcement/canonical-plan.json")
    );

    let decision = enforcement::decision_evidence(&plan).unwrap();
    assert_eq!(
        pretty_json(&decision),
        include_bytes!("fixtures/enforcement/canonical-decision-event.json")
    );

    let receipt = plan
        .receipts
        .iter()
        .find(|receipt| receipt.gate_id == "proof")
        .unwrap();
    let evidence = enforcement::gate_evidence_for_intent(
        request(),
        exact_intent,
        "proof",
        0,
        &enforcement::GateExecutionContext {
            contract_digest: plan.contract_digest,
            workspace_fingerprint: plan.workspace_fingerprint,
            gate_definition_digest: receipt.gate_definition_digest.clone(),
            authorization_binding: plan.authorization_binding,
        },
    )
    .unwrap();
    let (receipt_path, receipt_payload) = evidence
        .effects
        .iter()
        .find_map(|effect| match effect {
            enforcement::EvidenceEffect::CreateJson {
                relative_path,
                payload,
            } => Some((relative_path, payload)),
            _ => None,
        })
        .unwrap();
    let receipt_path = format!("{}\n", receipt_path.display());
    assert_eq!(
        receipt_path.as_bytes(),
        include_bytes!("fixtures/enforcement/canonical-receipt-path.txt")
    );
    assert_eq!(
        pretty_json(receipt_payload),
        include_bytes!("fixtures/enforcement/canonical-receipt.json")
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

    for authority in ["gates", "gates-internal", "workflow", "directory"] {
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
        } else if authority == "gates-internal" {
            write(
                &temp.path().join(".nopal/real-gates.jsonc"),
                r#"{"version":"nopal.gates/v1","gates":[{"id":"inside","stage":"pre_pr","argv":["true"]}]}"#,
            );
            fs::remove_file(temp.path().join(".nopal/gates.jsonc")).unwrap();
            symlink("real-gates.jsonc", temp.path().join(".nopal/gates.jsonc")).unwrap();
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
fn launch_executor_requirements_cover_every_active_stage_despite_push_denial() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    write(
        &temp.path().join(".nopal/policy.jsonc"),
        r#"{
          "version": "nopal.policy/v1",
          "modes": { "supervised_auto": { "rules": [
            { "id": "deny-push", "actions": ["git.push"], "decision": "deny" }
          ] } }
        }"#,
    );
    write(
        &temp.path().join(".nopal/gates.jsonc"),
        r#"{
          "version": "nopal.gates/v1",
          "gates": [
            { "id": "continuous", "stage": "continuous", "argv": ["continuous-proof"] },
            { "id": "edit", "stage": "per_edit", "argv": ["edit-proof"] },
            { "id": "commit", "stage": "pre_commit", "argv": ["commit-proof"] },
            { "id": "push", "stage": "pre_pr", "argv": ["push-proof"] },
            { "id": "after", "stage": "post_pr", "argv": ["post-proof"] }
          ]
        }"#,
    );

    let requirements = enforcement::gate_executor_requirements(temp.path(), None).unwrap();
    assert_eq!(
        requirements
            .iter()
            .map(|gate| gate.id.as_str())
            .collect::<Vec<_>>(),
        ["continuous", "edit", "commit", "push"]
    );
}

#[test]
fn selector_scoped_explicit_gate_cannot_suppress_all_generated_push_proof() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    write(&temp.path().join("Cargo.toml"), "[workspace]\nmembers=[]\n");
    let generated = nopal_core::gate_scaffold::inspect(temp.path()).unwrap();
    let mut gates: serde_json::Value =
        serde_json::from_str(&generated.gates_json().unwrap()).unwrap();
    gates["gates"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "explicit-rust",
            "stage": "pre_pr",
            "argv": ["cargo", "test"]
        }));
    gates["gate_sets"] = serde_json::json!({
        "rust": {"gates": ["explicit-rust"]}
    });
    gates["selectors"] = serde_json::json!([{
        "name": "rust",
        "paths": ["**/*.rs"],
        "gate_sets": ["rust"]
    }]);
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

    assert!(report.ok, "{:?}", report.diagnostics);
    assert!(!report.required_gates.is_empty());
    assert!(
        report
            .required_gates
            .iter()
            .all(|gate| gate.id.starts_with("detected."))
    );
}

#[test]
fn exact_file_write_intent_selects_continuous_and_matching_per_edit_gates() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    write(
        &temp.path().join(".nopal/gates.jsonc"),
        r#"{
          "version": "nopal.gates/v1",
          "gates": [
            { "id": "always", "stage": "continuous", "argv": ["true"] },
            { "id": "rust-edit", "stage": "per_edit", "argv": ["true"] },
            { "id": "docs-edit", "stage": "per_edit", "argv": ["true"] }
          ],
          "gate_sets": {
            "rust": { "gates": ["always", "rust-edit"] },
            "docs": { "gates": ["always", "docs-edit"] }
          },
          "selectors": [
            { "name": "rust", "paths": ["**/*.rs"], "gate_sets": ["rust"] },
            { "name": "docs", "paths": ["docs/**"], "gate_sets": ["docs"] }
          ]
        }"#,
    );
    let request = EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "fs.write",
        classes: &[ActionClass::WorkspaceWrite],
        run_dir: None,
        receipt_key: None,
    };

    let report =
        enforcement::plan_for_intent(request, intent("write", &["src/lib.rs"], true)).unwrap();

    assert!(report.ok, "{:?}", report.diagnostics);
    assert_eq!(report.required_stages, ["continuous", "per_edit"]);
    assert_eq!(
        report
            .required_gates
            .iter()
            .map(|gate| gate.id.as_str())
            .collect::<Vec<_>>(),
        ["always", "rust-edit"]
    );
    assert!(!report.authorization_binding.is_empty());
    assert_eq!(report.intent.tool_call_id, "call-write");
}

#[test]
fn plan_reports_effective_policy_and_placement_winners() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    write(
        &temp.path().join(".beislid/workflow.md"),
        r#"```beislid:action_policy
modes:
  supervised-auto:
    actions:
      fs.write: deny
```"#,
    );
    let report = enforcement::plan_for_intent(
        EnforcementRequest {
            root: temp.path(),
            config_dir: None,
            mode: Mode::SupervisedAuto,
            action: "fs.write",
            classes: &[ActionClass::WorkspaceWrite],
            run_dir: None,
            receipt_key: None,
        },
        intent("write", &["src/lib.rs"], true),
    )
    .unwrap();

    assert_eq!(report.decision.as_str(), "deny");
    assert_eq!(report.decision_winners, ["workflow policy"]);
    assert!(!report.placement_winners.is_empty());
    assert!(
        report
            .decisions
            .iter()
            .all(|source| !source.source.is_empty())
    );
}

#[test]
fn known_action_with_forged_weaker_class_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    let report = enforcement::plan_for_intent(
        EnforcementRequest {
            root: temp.path(),
            config_dir: None,
            mode: Mode::SupervisedAuto,
            action: "fs.write",
            classes: &[ActionClass::Read],
            run_dir: None,
            receipt_key: None,
        },
        intent("write", &["source.txt"], true),
    )
    .unwrap();
    assert!(!report.ok);
    assert_eq!(report.decision.as_str(), "deny");
}

#[test]
fn unknown_action_with_forged_weaker_class_is_rejected() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    let report = enforcement::plan_for_intent(
        EnforcementRequest {
            root: temp.path(),
            config_dir: None,
            mode: Mode::SupervisedAuto,
            action: "future.mutate",
            classes: &[ActionClass::Read],
            run_dir: None,
            receipt_key: None,
        },
        intent("bash", &[], true),
    )
    .unwrap();
    assert!(!report.ok);
    assert_eq!(report.decision.as_str(), "deny");
}

#[test]
fn malformed_exact_intent_is_an_unapprovable_contract_error() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    let mut malformed = intent("write", &["../escape"], true);
    malformed.input_digest.clear();
    let report = enforcement::plan_for_intent(
        EnforcementRequest {
            root: temp.path(),
            config_dir: None,
            mode: Mode::SupervisedAuto,
            action: "fs.write",
            classes: &[ActionClass::WorkspaceWrite],
            run_dir: None,
            receipt_key: None,
        },
        malformed,
    )
    .unwrap();

    assert!(!report.ok);
    assert_eq!(report.decision.as_str(), "deny");
    assert!(report.required_gates.is_empty());
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
    assert_eq!(force.decision_winners, ["built-in safety floor"]);
    assert_eq!(force.placement_winners, ["built-in safety floor"]);
    assert!(force.required_gates.is_empty());

    let manual_force = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::Manual,
        action: "git.push_force",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
        receipt_key: None,
    })
    .unwrap();
    assert_eq!(manual_force.decision.as_str(), "deny");
    assert_eq!(manual_force.decision_winners, ["built-in safety floor"]);
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
    let evidence = enforcement::gate_evidence(
        request(),
        "proof",
        0,
        &enforcement::GateExecutionContext {
            contract_digest: initial.contract_digest.clone(),
            workspace_fingerprint: initial.workspace_fingerprint.clone(),
            gate_definition_digest: receipt.gate_definition_digest.clone(),
            authorization_binding: initial.authorization_binding.clone(),
        },
    )
    .unwrap();
    apply_evidence(&run.run_dir, evidence);
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
fn concurrent_gate_receipts_are_scoped_to_exact_authorization_bindings() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    write(
        &temp.path().join(".nopal/gates.jsonc"),
        r#"{
          "version": "nopal.gates/v1",
          "gates": [{
            "id": "proof",
            "stage": "continuous",
            "argv": ["true"],
            "parallel_safe": true,
            "mutates": false
          }]
        }"#,
    );
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
            run_id: Some("concurrent-receipts"),
        },
    )
    .unwrap();
    let request = || EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "fs.read",
        classes: &[ActionClass::Read],
        run_dir: Some(&run.run_dir),
        receipt_key: Some(RECEIPT_KEY),
    };

    let mut paths = Vec::new();
    for tool_name in ["read-a", "read-b"] {
        let exact_intent = intent(tool_name, &[], false);
        let initial = enforcement::plan_for_intent(request(), exact_intent.clone()).unwrap();
        let receipt = initial
            .receipts
            .iter()
            .find(|receipt| receipt.gate_id == "proof")
            .unwrap();
        let evidence = enforcement::gate_evidence_for_intent(
            request(),
            exact_intent,
            "proof",
            0,
            &enforcement::GateExecutionContext {
                contract_digest: initial.contract_digest,
                workspace_fingerprint: initial.workspace_fingerprint,
                gate_definition_digest: receipt.gate_definition_digest.clone(),
                authorization_binding: initial.authorization_binding,
            },
        )
        .unwrap();
        let receipt_path = evidence
            .effects
            .iter()
            .find_map(|effect| match effect {
                enforcement::EvidenceEffect::CreateJson { relative_path, .. } => {
                    Some(relative_path.clone())
                }
                _ => None,
            })
            .unwrap();
        paths.push(receipt_path);
        apply_evidence(&run.run_dir, evidence);
        assert!(
            enforcement::plan_for_intent(request(), intent(tool_name, &[], false))
                .unwrap()
                .required_gates
                .is_empty()
        );
    }

    assert_ne!(paths[0], paths[1]);
    assert!(paths.iter().all(|path| run.run_dir.join(path).is_file()));
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
    let context = enforcement::GateExecutionContext {
        contract_digest: initial.contract_digest.clone(),
        workspace_fingerprint: initial.workspace_fingerprint.clone(),
        gate_definition_digest: receipt.gate_definition_digest.clone(),
        authorization_binding: initial.authorization_binding.clone(),
    };
    let exact_path = enforcement::gate_evidence(request(), "proof", 0, &context)
        .unwrap()
        .effects
        .into_iter()
        .find_map(|effect| match effect {
            enforcement::EvidenceEffect::CreateJson { relative_path, .. } => Some(relative_path),
            _ => None,
        })
        .unwrap();
    write(
        &run.run_dir.join(exact_path),
        &serde_json::json!({
            "action": "git.push",
            "contract_digest": initial.contract_digest,
            "exit_code": 0,
            "gate_id": "proof",
            "gate_definition_digest": receipt.gate_definition_digest,
            "workspace_fingerprint": initial.workspace_fingerprint,
            "authorization_binding": initial.authorization_binding,
            "signature": "forged"
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
        authorization_binding: initial.authorization_binding.clone(),
    };
    write(
        &temp.path().join(".nopal/gates.jsonc"),
        r#"{
          "version": "nopal.gates/v1",
          "gates": [{ "id": "proof", "stage": "pre_pr", "command": "false" }]
        }"#,
    );

    let error = enforcement::gate_evidence(request(), "proof", 0, &context).unwrap_err();
    assert!(error.to_string().contains("changed during execution"));
    assert_eq!(
        enforcement::plan(request()).unwrap().required_gates.len(),
        1
    );
}

#[test]
fn exact_human_approval_is_durable_authenticated_and_single_use() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    write(
        &temp.path().join(".nopal/policy.jsonc"),
        r#"{
          "version": "nopal.policy/v1",
          "modes": { "supervised_auto": { "rules": [
            { "id": "ask-write", "actions": ["fs.write"], "decision": "ask" }
          ] } }
        }"#,
    );
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
            run_id: Some("approval-test"),
        },
    )
    .unwrap();
    let request = || EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "fs.write",
        classes: &[ActionClass::WorkspaceWrite],
        run_dir: Some(&run.run_dir),
        receipt_key: Some(RECEIPT_KEY),
    };
    let exact_intent = intent("write", &["source.txt"], true);
    let initial = enforcement::plan_for_intent(request(), exact_intent.clone()).unwrap();
    assert_eq!(initial.decision.as_str(), "ask");
    assert!(!initial.approval_current);
    assert!(!initial.authorized);

    let approval = enforcement::approval_evidence(&initial, true, "vic", RECEIPT_KEY).unwrap();
    apply_evidence(&run.run_dir, approval);
    let approved = enforcement::plan_for_intent(request(), exact_intent.clone()).unwrap();
    assert!(approved.approval_current);
    assert!(approved.authorized);

    let foreign =
        enforcement::plan_for_intent(request(), intent("write-foreign", &["source.txt"], true))
            .unwrap();
    assert!(!foreign.approval_current);
    assert!(!foreign.authorized);

    let mut changed_target = exact_intent.clone();
    changed_target.target_digest = "target-changed-only".to_owned();
    let changed_target = enforcement::plan_for_intent(request(), changed_target).unwrap();
    assert!(!changed_target.approval_current);
    assert!(!changed_target.authorized);

    let release = enforcement::authorization_release_evidence(&approved, RECEIPT_KEY).unwrap();
    apply_evidence(&run.run_dir, release);
    let consumed = enforcement::plan_for_intent(request(), exact_intent).unwrap();
    assert!(!consumed.approval_current);
    assert!(!consumed.authorized);
    assert!(enforcement::authorization_release_evidence(&consumed, RECEIPT_KEY).is_err());
    let events = fs::read_to_string(run.run_dir.join("events.jsonl")).unwrap();
    assert!(events.contains("action_approval"));
    assert!(events.contains("authorization_release"));
}

#[test]
fn tool_outcome_is_bound_to_the_exact_authorization_release() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    write(
        &temp.path().join(".nopal/policy.jsonc"),
        r#"{
          "version": "nopal.policy/v1",
          "modes": { "supervised_auto": { "rules": [
            { "id": "allow-read", "actions": ["fs.read"], "decision": "allow" }
          ] } }
        }"#,
    );
    let plan = enforcement::plan_for_intent(
        EnforcementRequest {
            root: temp.path(),
            config_dir: None,
            mode: Mode::SupervisedAuto,
            action: "fs.read",
            classes: &[ActionClass::Read],
            run_dir: None,
            receipt_key: None,
        },
        intent("read", &["source.txt"], false),
    )
    .unwrap();
    assert!(plan.authorized);
    let release_id = enforcement::authorization_release_id(&plan, RECEIPT_KEY).unwrap();
    let evidence = enforcement::tool_outcome_evidence(
        &plan.action,
        &plan.authorization_binding,
        &plan.intent.tool_call_id,
        &release_id,
        enforcement::ToolOutcome::Success,
        RECEIPT_KEY,
    )
    .unwrap();
    assert!(
        evidence
            .effects
            .iter()
            .any(|effect| matches!(effect, enforcement::EvidenceEffect::CreateJson { .. }))
    );
    assert!(
        enforcement::tool_outcome_evidence(
            &plan.action,
            "foreign-binding",
            &plan.intent.tool_call_id,
            &release_id,
            enforcement::ToolOutcome::Error,
            RECEIPT_KEY,
        )
        .is_err()
    );
}

#[test]
fn receipt_capability_generation_is_random_and_well_formed() {
    let first = enforcement::generate_receipt_key().unwrap();
    let second = enforcement::generate_receipt_key().unwrap();
    assert_eq!(first.len(), 64);
    assert!(first.bytes().all(|byte| byte.is_ascii_hexdigit()));
    assert_ne!(first, second);
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
fn typed_agent_isolation_is_recognized_and_unavailable_placement_blocks() {
    let temp = tempfile::tempdir().unwrap();
    project(temp.path());
    write(
        &temp.path().join(".beislid/workflow.md"),
        r#"```beislid:agent_isolation
orchestrator: current
delegate: sequential
manual_root: repo-sibling
fallback:
  orchestrator: manual-transition-required
  delegate: sequential
```"#,
    );
    let current = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
        receipt_key: None,
    })
    .unwrap();
    assert!(current.ok, "{:?}", current.diagnostics);
    assert!(!current.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::BeislidImportUnsupported
            && diagnostic.message.contains("agent_isolation")
    }));

    write(
        &temp.path().join(".beislid/workflow.md"),
        r#"```beislid:agent_isolation
orchestrator: native
delegate: sequential
manual_root: repo-sibling
```"#,
    );
    let unavailable = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
        receipt_key: None,
    })
    .unwrap();
    assert!(!unavailable.ok);
    assert!(
        unavailable
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("cannot prove") })
    );

    write(
        &temp.path().join(".beislid/workflow.md"),
        r#"```beislid:agent_isolation
orchestrator: current
delegate: native
manual_root: repo-sibling
```"#,
    );
    let unavailable_delegate = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
        receipt_key: None,
    })
    .unwrap();
    assert!(!unavailable_delegate.ok);
    assert!(
        unavailable_delegate
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.message.contains("delegated mutation placement"))
    );

    write(
        &temp.path().join(".beislid/workflow.md"),
        r#"```beislid:agent_isolation
orchestrator: current
delegate: sequential
manual_root: repo-sibling
runtime_profiles:
  integration:
    required_bindings:
      - PRIMARY_DATABASE_URL
    provider:
      allocate: 'runtime allocate'
      verify: 'runtime verify'
      release: 'runtime release'
      reconcile: 'runtime reconcile'
```"#,
    );
    let unavailable_profile = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
        receipt_key: None,
    })
    .unwrap();
    assert!(!unavailable_profile.ok);
    assert!(
        unavailable_profile
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("runtime profile capability") }),
        "{:?}",
        unavailable_profile.diagnostics
    );

    write(
        &temp.path().join(".beislid/workflow.md"),
        "```beislid:agent_isolation\norchestrator: 7\ndelegate: sequential\n```\n",
    );
    let malformed_type = enforcement::plan(EnforcementRequest {
        root: temp.path(),
        config_dir: None,
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: None,
        receipt_key: None,
    })
    .unwrap();
    assert!(!malformed_type.ok);
    assert!(
        malformed_type
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.message.contains("orchestrator must be a string") })
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
