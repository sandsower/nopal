#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::{Seek, Write as IoWrite};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{Command, Output, Stdio};

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
    let mut command = Command::new(env!("CARGO_BIN_EXE_nopal"));
    command
        .args(["--dir", root.to_str().unwrap(), "--json"])
        .args(args)
        .env("BEISLID_STATE_DIR", state)
        .env("NOPAL_CONFIG_DIR", state.join("config"))
        .env("NOPAL_ENFORCEMENT_CAPABILITY_FD", CAPABILITY_FD.to_string())
        .env("PROOF_GIT_BIN", git_executable())
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
