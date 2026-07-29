#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::{Seek, Write as IoWrite};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use nopal_core::enforcement::{
    self, EnforcementIntent, EnforcementRequest, EvidenceEffect, GateExecutionContext,
};
use nopal_core::policy::{ActionClass, Mode};
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

fn git_executable() -> std::path::PathBuf {
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|directory| directory.join("git"))
                .find(|candidate| candidate.is_file())
        })
        .unwrap()
        .canonicalize()
        .unwrap()
}

fn run_with_capability(root: &Path, state: &Path, args: &[&str], capability: &str) -> Output {
    const CAPABILITY_FD: i32 = 198;
    let mut capability_file = tempfile::tempfile().unwrap();
    capability_file.write_all(RECEIPT_KEY.as_bytes()).unwrap();
    capability_file.seek(std::io::SeekFrom::Start(0)).unwrap();
    let source_fd = capability_file.as_raw_fd();
    let home = state.join("test-home");
    fs::create_dir_all(&home).unwrap();
    let mut command = Command::new(env!("CARGO_BIN_EXE_nopal"));
    command
        .args(["--dir", root.to_str().unwrap(), "--json"])
        .args(args)
        .env("BEISLID_STATE_DIR", state)
        .env("NOPAL_CONFIG_DIR", state.join("config"))
        .env("NOPAL_ENFORCEMENT_CAPABILITY_FD", CAPABILITY_FD.to_string())
        .env("PROOF_GIT_BIN", git_executable())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("NOPAL_TEST_CLEAN_GIT_CONFIG", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    // SAFETY: source_fd stays open through spawn and CAPABILITY_FD is reserved
    // for this child before exec.
    unsafe {
        command.pre_exec(move || {
            if libc::dup2(source_fd, CAPABILITY_FD) < 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let mut child = command.spawn().unwrap();
    write!(
        child.stdin.take().unwrap(),
        "{}",
        serde_json::json!({
            "kind": "nopal.enforcement.adapter_proof/v1",
            "capability": capability
        })
    )
    .unwrap();
    child.wait_with_output().unwrap()
}

fn run(root: &Path, state: &Path, args: &[&str]) -> Output {
    run_with_capability(root, state, args, RECEIPT_KEY)
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

fn exact_enforcement_args<'a>(
    operation: &'a str,
    digest: &'a str,
    runtime_digest: &'a str,
) -> Vec<&'a str> {
    vec![
        "enforcement",
        operation,
        "--mode",
        "supervised_auto",
        "--action",
        "git.push",
        "--class",
        "git_remote",
        "--run-id",
        "parity-run",
        "--launch-id",
        "launch-parity",
        "--session-id",
        "session-parity",
        "--tool-call-id",
        "call-parity",
        "--tool-name",
        "bash",
        "--input-digest",
        "input-parity",
        "--target-digest",
        "target-parity",
        "--executor-digest",
        digest,
        "--runtime-digest",
        runtime_digest,
        "--mutates",
    ]
}

fn events(run_root: &Path) -> Vec<Value> {
    fs::read_to_string(run_root.join("events.jsonl"))
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[test]
fn public_verify_runs_the_local_pre_pr_boundary_without_pi_or_network() {
    let temp = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fixture(temp.path());
    let initialized_git = Command::new("git")
        .arg("init")
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(initialized_git.status.success(), "{initialized_git:?}");
    let poisoned_pi = temp.path().join("pi-must-not-run");
    write(&poisoned_pi, "#!/bin/sh\nexit 97\n");

    let output = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args([
            "--dir",
            temp.path().to_str().unwrap(),
            "--json",
            "verify",
            "--state-dir",
            state.path().to_str().unwrap(),
        ])
        .env("PI_CODING_AGENT_BIN", &poisoned_pi)
        .env("PROOF_GIT_BIN", git_executable())
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .env("HOME", state.path().join("home"))
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("NOPAL_TEST_CLEAN_GIT_CONFIG", "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let report = json(&output);
    assert_eq!(report["kind"], "nopal.verification/v1");
    assert_eq!(report["ok"], true);
    assert_eq!(report["outcome"]["state"], "verified");

    let run_id = report["run_id"].as_str().unwrap();
    let run_dir = state
        .path()
        .join("runs/verification/unknown-repo")
        .join(run_id);
    assert!(run_dir.join("events.jsonl").is_file());
    assert!(
        run_dir
            .join("artifacts/enforcement/receipts/proof")
            .read_dir()
            .unwrap()
            .next()
            .is_some()
    );
    let run: Value = serde_json::from_slice(&fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(run["status"], "completed");
}

#[test]
fn public_verify_cannot_approve_an_ask_or_create_a_release() {
    let temp = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fixture(temp.path());
    write(
        &temp.path().join(".nopal/policy.jsonc"),
        r#"{
          "version": "nopal.policy/v1",
          "modes": { "supervised_auto": { "rules": [
            { "id": "ask-push", "actions": ["git.push"], "decision": "ask" }
          ] } }
        }"#,
    );
    assert!(
        Command::new("git")
            .arg("init")
            .current_dir(temp.path())
            .status()
            .unwrap()
            .success()
    );

    let output = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args([
            "--dir",
            temp.path().to_str().unwrap(),
            "--json",
            "verify",
            "--state-dir",
            state.path().to_str().unwrap(),
        ])
        .env("HOME", state.path().join("home"))
        .env("PROOF_GIT_BIN", git_executable())
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("NOPAL_TEST_CLEAN_GIT_CONFIG", "1")
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    let report = json(&output);
    assert_eq!(report["outcome"]["state"], "approval_required");
    let run_id = report["run_id"].as_str().unwrap();
    let run_dir = state
        .path()
        .join("runs/verification/unknown-repo")
        .join(run_id);
    assert!(!run_dir.join("artifacts/enforcement/releases").exists());
    let run: Value = serde_json::from_slice(&fs::read(run_dir.join("run.json")).unwrap()).unwrap();
    assert_eq!(run["status"], "interrupted");
}

fn receipt_files(root: &Path) -> Vec<(String, Vec<u8>)> {
    let receipts = root.join("artifacts/enforcement/receipts");
    let mut files = Vec::new();
    let mut stack = vec![receipts.clone()];
    while let Some(directory) = stack.pop() {
        if !directory.is_dir() {
            continue;
        }
        for entry in fs::read_dir(directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                stack.push(path);
            } else {
                files.push((
                    path.strip_prefix(&receipts)
                        .unwrap()
                        .to_string_lossy()
                        .into_owned(),
                    fs::read(path).unwrap(),
                ));
            }
        }
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    files
}

#[test]
fn evidence_only_and_interactive_transactions_emit_identical_plans_and_receipts() {
    let root = tempfile::tempdir().unwrap();
    let headless_state = tempfile::tempdir().unwrap();
    let interactive_state = tempfile::tempdir().unwrap();
    fixture(root.path());
    assert!(
        Command::new("git")
            .arg("init")
            .current_dir(root.path())
            .status()
            .unwrap()
            .success()
    );

    let initialize = |state: &Path| {
        let initialized = run(
            root.path(),
            state,
            &[
                "ledger",
                "init",
                "--skill",
                "nopal",
                "--flow",
                "enforcement",
                "--run-id",
                "parity-run",
            ],
        );
        assert!(initialized.status.success(), "{initialized:?}");
        let run_root = Path::new(json(&initialized)["run_dir"].as_str().unwrap()).to_path_buf();
        let prepared = run(
            root.path(),
            state,
            &[
                "enforcement",
                "prepare-runtime",
                "--mode",
                "supervised_auto",
                "--action",
                "git.push",
                "--class",
                "git_remote",
                "--run-id",
                "parity-run",
            ],
        );
        assert!(prepared.status.success(), "{prepared:?}");
        let prepared = json(&prepared);
        let digest = prepared["executor_digest"].as_str().unwrap().to_owned();
        let runtime_digest = prepared["runtime_digest"].as_str().unwrap().to_owned();
        (run_root, digest, runtime_digest)
    };
    let (headless_run, headless_digest, headless_runtime_digest) =
        initialize(headless_state.path());
    let (interactive_run, interactive_digest, interactive_runtime_digest) =
        initialize(interactive_state.path());
    assert_eq!(headless_digest, interactive_digest);

    let evidence = run(
        root.path(),
        headless_state.path(),
        &exact_enforcement_args(
            "verify-evidence",
            &headless_digest,
            &headless_runtime_digest,
        ),
    );
    let interactive = run(
        root.path(),
        interactive_state.path(),
        &exact_enforcement_args("advance", &interactive_digest, &interactive_runtime_digest),
    );
    assert!(evidence.status.success(), "{evidence:?}");
    assert!(interactive.status.success(), "{interactive:?}");
    let evidence = json(&evidence);
    let interactive = json(&interactive);
    assert_eq!(evidence["state"], "verified");
    assert_eq!(interactive["state"], "released");
    assert_eq!(evidence["plan"], interactive["plan"]);
    assert_eq!(
        receipt_files(&headless_run),
        receipt_files(&interactive_run)
    );
    assert!(!receipt_files(&headless_run).is_empty());

    for (state, digest, runtime_digest) in [
        (
            headless_state.path(),
            headless_digest.as_str(),
            headless_runtime_digest.as_str(),
        ),
        (
            interactive_state.path(),
            interactive_digest.as_str(),
            interactive_runtime_digest.as_str(),
        ),
    ] {
        let cleaned = run(
            root.path(),
            state,
            &exact_enforcement_args("cleanup-runtime", digest, runtime_digest),
        );
        assert!(cleaned.status.success(), "{cleaned:?}");
    }
}

#[test]
fn cli_publication_preserves_core_plan_event_path_and_receipt_bytes() {
    let temp = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    write(
        &temp.path().join(".nopal/policy.jsonc"),
        include_str!("../../nopal-core/tests/fixtures/enforcement/project/.nopal/policy.jsonc"),
    );
    write(
        &temp.path().join(".nopal/gates.jsonc"),
        include_str!("../../nopal-core/tests/fixtures/enforcement/project/.nopal/gates.jsonc"),
    );
    write(&temp.path().join("source.txt"), "adapter fixture\n");
    let initialized_git = Command::new("git")
        .arg("init")
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(initialized_git.status.success(), "{initialized_git:?}");

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
            "adapter-parity",
        ],
    );
    assert!(initialized.status.success(), "{initialized:?}");
    let run_root = Path::new(json(&initialized)["run_dir"].as_str().unwrap()).to_path_buf();

    let mut exact_intent: EnforcementIntent = serde_json::from_str(include_str!(
        "../../nopal-core/tests/fixtures/enforcement/canonical-intent.json"
    ))
    .unwrap();
    // Debug integration tests use the compatibility executor identity because
    // only the private launch path can mint a run-private executor manifest.
    exact_intent.executor_digest = "legacy-executor".to_owned();
    exact_intent.workspace_fingerprint = None;
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
        "adapter-parity",
        "--launch-id",
        &exact_intent.launch_id,
        "--session-id",
        &exact_intent.session_id,
        "--tool-call-id",
        &exact_intent.tool_call_id,
        "--tool-name",
        &exact_intent.tool_name,
        "--input-digest",
        &exact_intent.input_digest,
        "--target-digest",
        &exact_intent.target_digest,
        "--executor-digest",
        &exact_intent.executor_digest,
        "--changed-file",
        &exact_intent.changed_files[0],
        "--changed-file",
        &exact_intent.changed_files[1],
        "--mutates",
    ];
    let published_plan = run(temp.path(), state.path(), &plan_args);
    assert!(published_plan.status.success(), "{published_plan:?}");
    let published_plan_json = json(&published_plan);
    exact_intent.workspace_fingerprint = Some(
        published_plan_json["workspace_fingerprint"]
            .as_str()
            .unwrap()
            .to_owned(),
    );
    let config_dir = state.path().join("config");
    let canonical_root = std::path::PathBuf::from(published_plan_json["root"].as_str().unwrap());
    let core_request = || EnforcementRequest {
        root: &canonical_root,
        config_dir: Some(&config_dir),
        mode: Mode::SupervisedAuto,
        action: "git.push",
        classes: &[ActionClass::GitRemote],
        run_dir: Some(&run_root),
        receipt_key: Some(RECEIPT_KEY.as_bytes()),
    };
    let core_plan = enforcement::plan_for_intent(core_request(), exact_intent.clone()).unwrap();
    let mut core_plan_bytes = serde_json::to_vec_pretty(&core_plan).unwrap();
    core_plan_bytes.push(b'\n');
    assert_eq!(published_plan.stdout, core_plan_bytes);
    let decision_directive = enforcement::decision_evidence(&core_plan).unwrap();
    let decision_payload = decision_directive
        .effects
        .iter()
        .find_map(|effect| match effect {
            EvidenceEffect::AppendEvent { event, payload } if event == "action_decision" => {
                Some(nopal_core::run_ledger::redact_json(payload))
            }
            _ => None,
        })
        .unwrap();
    let decision_event = events(&run_root)
        .into_iter()
        .find(|event| event["type"] == "action_decision")
        .unwrap();
    assert_eq!(
        decision_event["payload"],
        serde_json::to_value(decision_payload).unwrap()
    );

    let receipt_status = core_plan
        .receipts
        .iter()
        .find(|receipt| receipt.gate_id == "proof")
        .unwrap();
    let context = GateExecutionContext {
        contract_digest: core_plan.contract_digest.clone(),
        workspace_fingerprint: core_plan.workspace_fingerprint.clone(),
        gate_definition_digest: receipt_status.gate_definition_digest.clone(),
        authorization_binding: core_plan.authorization_binding.clone(),
    };
    let record_intent = exact_intent.clone();
    let core_evidence =
        enforcement::gate_evidence_for_intent(core_request(), exact_intent, "proof", 0, &context)
            .unwrap();
    let (receipt_path, receipt_payload) = core_evidence
        .effects
        .iter()
        .find_map(|effect| match effect {
            EvidenceEffect::CreateJson {
                relative_path,
                payload,
            } => Some((relative_path.clone(), payload.clone())),
            _ => None,
        })
        .unwrap();

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
            "adapter-parity",
            "--launch-id",
            &record_intent.launch_id,
            "--session-id",
            &record_intent.session_id,
            "--tool-call-id",
            &record_intent.tool_call_id,
            "--tool-name",
            &record_intent.tool_name,
            "--input-digest",
            &record_intent.input_digest,
            "--target-digest",
            &record_intent.target_digest,
            "--executor-digest",
            &record_intent.executor_digest,
            "--changed-file",
            &record_intent.changed_files[0],
            "--changed-file",
            &record_intent.changed_files[1],
            "--mutates",
            "--gate-id",
            "proof",
            "--exit-code",
            "0",
            "--contract-digest",
            &context.contract_digest,
            "--workspace-fingerprint",
            &context.workspace_fingerprint,
            "--gate-definition-digest",
            &context.gate_definition_digest,
            "--authorization-binding",
            &context.authorization_binding,
        ],
    );
    assert!(recorded.status.success(), "{recorded:?}");

    let mut receipt_bytes = serde_json::to_vec_pretty(&receipt_payload).unwrap();
    receipt_bytes.push(b'\n');
    assert_eq!(
        fs::read(run_root.join(&receipt_path)).unwrap(),
        receipt_bytes
    );
    let expected_gate_receipt = core_evidence
        .effects
        .iter()
        .find_map(|effect| match effect {
            EvidenceEffect::AppendEvent { event, payload } if event == "gate_receipt" => {
                Some(nopal_core::run_ledger::redact_json(payload))
            }
            _ => None,
        })
        .unwrap();
    let gate_receipt_event = events(&run_root)
        .into_iter()
        .find(|event| event["type"] == "gate_receipt")
        .unwrap();
    assert_eq!(
        gate_receipt_event["payload"],
        serde_json::to_value(expected_gate_receipt).unwrap()
    );
}

#[test]
fn cli_records_decisions_and_reuses_only_current_gate_receipts() {
    let temp = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    fixture(temp.path());
    for args in [
        vec!["init"],
        vec!["config", "user.email", "proof@nopal.invalid"],
        vec!["config", "user.name", "Nopal Proof"],
        vec!["add", "."],
        vec!["commit", "-m", "initial"],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(temp.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "git fixture failed: {output:?}");
    }

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
    let run_root = Path::new(json(&initialized)["run_dir"].as_str().unwrap()).to_path_buf();
    assert!(
        !run_root
            .join("artifacts/enforcement/receipt-capability")
            .exists()
    );

    let unauthenticated = run_with_capability(
        temp.path(),
        state.path(),
        &[
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
        ],
        "wrong-capability",
    );
    assert!(!unauthenticated.status.success());
    assert!(
        String::from_utf8_lossy(&unauthenticated.stderr)
            .contains("active launch-scoped adapter capability"),
        "{}",
        String::from_utf8_lossy(&unauthenticated.stderr)
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
    let authorization_binding = first_json["authorization_binding"]
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
            "--authorization-binding",
            &authorization_binding,
        ],
    );
    assert!(recorded.status.success(), "{recorded:?}");

    let current = run(temp.path(), state.path(), &plan_args);
    assert!(current.status.success(), "{current:?}");
    let current_json = json(&current);
    assert_eq!(current_json["required_gates"], serde_json::json!([]));
    let authorization_binding = current_json["authorization_binding"].as_str().unwrap();
    let authorized = run(
        temp.path(),
        state.path(),
        &[
            "enforcement",
            "authorize",
            "--mode",
            "supervised_auto",
            "--action",
            "git.push",
            "--class",
            "git_remote",
            "--run-id",
            "cli-proof",
            "--authorization-binding",
            authorization_binding,
        ],
    );
    assert!(authorized.status.success(), "{authorized:?}");
    let release_id = json(&authorized)["release_id"].as_str().unwrap().to_owned();
    let outcome_args = [
        "enforcement",
        "record-outcome",
        "--mode",
        "supervised_auto",
        "--action",
        "git.push",
        "--class",
        "git_remote",
        "--run-id",
        "cli-proof",
        "--authorization-binding",
        authorization_binding,
        "--release-id",
        &release_id,
        "--outcome",
        "success",
    ];
    let outcome = run(temp.path(), state.path(), &outcome_args);
    assert!(outcome.status.success(), "{outcome:?}");
    let duplicate = run(temp.path(), state.path(), &outcome_args);
    assert!(!duplicate.status.success());

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
    assert!(events.contains("authorization_release"));
    assert!(events.contains("tool_outcome"));

    let configured = Command::new("git")
        .args(["config", "core.fsmonitor", "/tmp/untrusted-fsmonitor"])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(configured.status.success());
    let dangerous_config = run(temp.path(), state.path(), &plan_args);
    assert!(!dangerous_config.status.success());
    assert!(String::from_utf8_lossy(&dangerous_config.stderr).contains("executable helper"));
    Command::new("git")
        .args(["config", "--unset", "core.fsmonitor"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    let configured = Command::new("git")
        .args([
            "config",
            "credential.helper",
            "!touch /tmp/credential-helper-ran",
        ])
        .current_dir(temp.path())
        .output()
        .unwrap();
    assert!(configured.status.success());
    let dangerous_helper = run(temp.path(), state.path(), &plan_args);
    assert!(!dangerous_helper.status.success());
    assert!(
        String::from_utf8_lossy(&dangerous_helper.stderr)
            .contains("not an exact trusted helper name")
    );
    Command::new("git")
        .args(["config", "--unset", "credential.helper"])
        .current_dir(temp.path())
        .output()
        .unwrap();

    write(
        &temp.path().join(".git/hooks/pre-commit"),
        "#!/bin/sh\nexit 0\n",
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            temp.path().join(".git/hooks/pre-commit"),
            fs::Permissions::from_mode(0o700),
        )
        .unwrap();
    }
    let dangerous_hook = run(temp.path(), state.path(), &plan_args);
    assert!(!dangerous_hook.status.success());
    assert!(String::from_utf8_lossy(&dangerous_hook.stderr).contains("unconfined code carrier"));

    write(
        &temp.path().join(".pi/settings.json"),
        r#"{"shellCommandPrefix":"touch bypass"}"#,
    );
    let changed_settings = run(temp.path(), state.path(), &plan_args);
    assert!(!changed_settings.status.success());
    assert!(String::from_utf8_lossy(&changed_settings.stderr).contains("executable authority"));

    write(&temp.path().join(".pi/settings.json"), "{}");
    fs::hard_link(
        temp.path().join(".pi/settings.json"),
        temp.path().join("settings-alias.json"),
    )
    .unwrap();
    let hardlinked_settings = run(temp.path(), state.path(), &plan_args);
    assert!(!hardlinked_settings.status.success());
    assert!(String::from_utf8_lossy(&hardlinked_settings.stderr).contains("hardlink aliases"));

    fs::remove_file(temp.path().join("settings-alias.json")).unwrap();
    fs::remove_file(temp.path().join(".pi/settings.json")).unwrap();
    fs::remove_dir(temp.path().join(".pi")).unwrap();
    let internal = temp.path().join("internal-config");
    fs::create_dir(&internal).unwrap();
    write(&internal.join("settings.json"), "{}");
    std::os::unix::fs::symlink(&internal, temp.path().join(".pi")).unwrap();
    let symlinked_parent = run(temp.path(), state.path(), &plan_args);
    assert!(!symlinked_parent.status.success());
    assert!(String::from_utf8_lossy(&symlinked_parent.stderr).contains("real directory"));
}
