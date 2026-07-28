#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

const RUN_ENV: &str = "NOPAL_RUN_REAL_PI_ENFORCEMENT_E2E";

fn command(dir: &Path, program: &Path, args: &[&str]) {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{} {:?} failed: stdout={} stderr={}",
        program.display(),
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn executable(name: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(format!("NOPAL_{name}_BIN")) {
        return PathBuf::from(path);
    }
    let output = Command::new("sh")
        .args(["-c", &format!("command -v {}", name.to_ascii_lowercase())])
        .output()
        .unwrap();
    assert!(output.status.success(), "{name} is required");
    PathBuf::from(String::from_utf8(output.stdout).unwrap().trim())
}

#[test]
#[ignore = "requires an installed Pi and NOPAL_RUN_REAL_PI_ENFORCEMENT_E2E=1"]
fn real_bare_nopal_enforces_allowed_denied_and_stale_pushes() {
    assert_eq!(std::env::var(RUN_ENV).as_deref(), Ok("1"));
    let git = executable("GIT");
    let pi = executable("PI");
    let nopal = PathBuf::from(env!("CARGO_BIN_EXE_nopal"));
    let source_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let enforcement_source = source_root.join("extensions/policy-gate");
    let provider_source = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deterministic-enforcement-provider.mjs")
        .canonicalize()
        .unwrap();
    let pi_package = pi
        .canonicalize()
        .unwrap()
        .parent()
        .and_then(Path::parent)
        .unwrap()
        .to_path_buf();
    let pi_ai = pi_package.join("node_modules/@earendil-works/pi-ai");
    assert!(
        pi_ai.is_dir(),
        "installed Pi must include its pi-ai dependency"
    );

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let remote = temp.path().join("remote.git");
    let state = temp.path().join("state");
    let home = temp.path().join("home");
    let agent = temp.path().join("agent");
    let bin = temp.path().join("bin");
    let adapter = temp.path().join("distribution/extensions/policy-gate");
    for directory in [&repo, &state, &home, &agent, &bin, &adapter] {
        fs::create_dir_all(directory).unwrap();
    }
    for file in ["index.ts", "classifier.ts", "nopal-cli.ts"] {
        fs::copy(enforcement_source.join(file), adapter.join(file)).unwrap();
    }
    fs::copy(
        &provider_source,
        adapter.join("deterministic-enforcement-provider.mjs"),
    )
    .unwrap();
    let enforcement = adapter.join("index.ts").canonicalize().unwrap();
    let dependency_parent = temp
        .path()
        .join("distribution/node_modules/@earendil-works");
    fs::create_dir_all(&dependency_parent).unwrap();
    std::os::unix::fs::symlink(&pi_ai, dependency_parent.join("pi-ai")).unwrap();
    let substituted_cli_marker = temp.path().join("substituted-cli-ran");
    write(
        &bin.join("nopal"),
        &format!("#!/bin/sh\ntouch {:?}\nexit 99\n", substituted_cli_marker),
    );
    fs::set_permissions(bin.join("nopal"), fs::Permissions::from_mode(0o755)).unwrap();

    command(
        temp.path(),
        &git,
        &["init", "--bare", remote.to_str().unwrap()],
    );
    command(&repo, &git, &["init"]);
    command(
        &repo,
        &git,
        &["config", "user.email", "proof@nopal.invalid"],
    );
    command(&repo, &git, &["config", "user.name", "Nopal Proof"]);
    write(&repo.join("source.txt"), "initial\n");
    command(&repo, &git, &["add", "source.txt"]);
    command(&repo, &git, &["commit", "-m", "initial"]);
    command(
        &repo,
        &git,
        &["remote", "add", "origin", remote.to_str().unwrap()],
    );

    write(
        &repo.join(".nopal/nopal.jsonc"),
        r#"{ "version": "nopal.project/v1", "profile": "portable" }"#,
    );
    write(
        &repo.join(".nopal/policy.jsonc"),
        r#"{
          "version": "nopal.policy/v1",
          "modes": { "supervised_auto": { "rules": [
            { "id": "allow-push", "actions": ["git.push"], "decision": "allow" },
            { "id": "deny-force", "actions": ["git.push_force"], "decision": "deny" },
            { "id": "allow-workspace-change", "actions": ["fs.write", "git.add", "git.commit"], "decision": "allow" }
          ] } }
        }"#,
    );
    write(
        &repo.join(".nopal/gates.jsonc"),
        r#"{
          "version": "nopal.gates/v1",
          "gates": [{
            "id": "proof",
            "stage": "pre_pr",
            "command": "count=$(cat \"$GATE_COUNT_FILE\" 2>/dev/null || echo 0); echo $((count + 1)) > \"$GATE_COUNT_FILE\""
          }]
        }"#,
    );
    std::os::unix::fs::symlink(".nopal/policy.jsonc", repo.join("policy-link")).unwrap();
    std::os::unix::fs::symlink(".nopal", repo.join("authority-dir-link")).unwrap();
    let bundle = format!(
        r#"{{
          "version": "nopal.bundle/v2",
          "inherit_ambient": [],
          "packages": [{{
            "id": "nopal",
            "source": {{ "type": "builtin", "package": "nopal" }},
            "requirement": "={}",
            "resources": [
              {{ "kind": "extension", "path": "index.ts" }},
              {{ "kind": "extension", "path": "deterministic-enforcement-provider.mjs" }}
            ]
          }}]
        }}"#,
        env!("CARGO_PKG_VERSION")
    );
    write(&repo.join(".nopal/bundle.jsonc"), &bundle);
    let lock = nopal_core::distribution::build_lock_from_local_sources(
        &repo,
        &bundle,
        &nopal_core::distribution::BuiltinDistribution {
            version: env!("CARGO_PKG_VERSION"),
            root: &adapter,
        },
    )
    .unwrap();
    write(
        &repo.join(".nopal/nopal.lock"),
        &nopal_core::distribution::lock_json(&lock).unwrap(),
    );
    write(
        &repo.join(".beislid/workflow.md"),
        "<!-- beislid-workflow: v1 -->\n\n# Enforcement proof\n",
    );
    let gate_count = temp.path().join("gate-count");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let output = Command::new(&nopal)
        .current_dir(&repo)
        .args([
            "--dir",
            repo.to_str().unwrap(),
            "--",
            "--mode",
            "json",
            "-p",
            "--no-session",
            "--tools",
            "bash,write",
            "--offline",
            "--approve",
            "--provider",
            "nopal-enforcement-proof",
            "--model",
            "deterministic",
            "enforcement walking skeleton proof",
        ])
        .env("NOPAL_PI_BIN", &pi)
        .env("NOPAL_DISTRIBUTION_ROOT", temp.path().join("distribution"))
        .env("BEISLID_STATE_DIR", &state)
        .env("NOPAL_CONFIG_DIR", temp.path().join("config"))
        .env("PI_CODING_AGENT_DIR", &agent)
        .env("HOME", &home)
        .env("GATE_COUNT_FILE", &gate_count)
        .env("AUTHORITY_FILE", ".nopal/policy.jsonc")
        .env("PROOF_ADAPTER_INDEX", &enforcement)
        .env("PROOF_GIT_BIN", &git)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let pi_events = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "not supported",
        "reserved for the trusted Pi adapter",
        "Nopal enforcement authority is not accessible to agent tools",
        "contract and evidence store are reserved",
    ] {
        assert!(
            pi_events.contains(expected),
            "missing blocked-tool evidence {expected:?}: {pi_events}"
        );
    }

    assert_eq!(fs::read_to_string(&gate_count).unwrap().trim(), "2");
    let local_head = Command::new(&git)
        .args(["rev-parse", "HEAD"])
        .current_dir(&repo)
        .output()
        .unwrap();
    let remote_head = Command::new(&git)
        .args(["rev-parse", "refs/heads/main"])
        .current_dir(&remote)
        .output()
        .unwrap();
    let authorized_head = Command::new(&git)
        .args(["rev-parse", "HEAD~1"])
        .current_dir(&repo)
        .output()
        .unwrap();
    assert_eq!(
        authorized_head.stdout, remote_head.stdout,
        "the stale-receipt push must land before every adversarial force-push form is blocked"
    );
    assert_ne!(
        local_head.stdout, remote_head.stdout,
        "compound authorization must block the whole shell envelope before its normal-push prefix executes"
    );
    assert!(
        !fs::read_to_string(repo.join("source.txt"))
            .unwrap()
            .contains("hidden")
    );
    let policy = fs::read_to_string(repo.join(".nopal/policy.jsonc")).unwrap();
    assert!(policy.contains("deny-force"));
    assert!(!policy.contains("forged"));
    assert!(!repo.join(".nopal/new/deep/forged.jsonc").exists());
    assert!(
        fs::read_to_string(&enforcement)
            .unwrap()
            .contains("policyGate")
    );

    let events = find_file(&state, "events.jsonl").expect("enforcement ledger events");
    let events = fs::read_to_string(events).unwrap();
    assert!(events.contains("action_decision"));
    assert!(events.contains("gate_attempt"));
    assert!(events.contains("gate_receipt"));
    assert!(events.contains("git.push_force"));
    assert!(events.contains("\"decision\": \"deny\""));
    assert!(
        !substituted_cli_marker.exists(),
        "the adapter must invoke the resolved launch binary instead of a PATH substitute"
    );
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    for entry in fs::read_dir(root).ok()? {
        let path = entry.ok()?.path();
        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
            return Some(path);
        }
    }
    None
}
