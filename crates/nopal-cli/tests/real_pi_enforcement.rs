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
    let enforcement = source_root
        .join("extensions/policy-gate/index.ts")
        .canonicalize()
        .unwrap();
    let provider = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/deterministic-enforcement-provider.mjs")
        .canonicalize()
        .unwrap();
    assert!(
        source_root
            .join("node_modules/@earendil-works/pi-ai")
            .is_dir(),
        "run npm ci first"
    );

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let remote = temp.path().join("remote.git");
    let state = temp.path().join("state");
    let home = temp.path().join("home");
    let agent = temp.path().join("agent");
    let bin = temp.path().join("bin");
    for directory in [&repo, &state, &home, &agent, &bin] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::copy(&nopal, bin.join("nopal")).unwrap();
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
            { "id": "allow-commit", "actions": ["git.commit"], "decision": "allow" }
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
    write(
        &repo.join(".nopal/bundle.jsonc"),
        &format!(
            "{{ \"version\": \"nopal.bundle/v1\", \"extensions\": [{{ \"source\": \"enforcement\", \"path\": {:?} }}, {{ \"source\": \"proof-provider\", \"path\": {:?} }}] }}",
            enforcement.display().to_string(),
            provider.display().to_string()
        ),
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
            "bash",
            "--offline",
            "--approve",
            "--provider",
            "nopal-enforcement-proof",
            "--model",
            "deterministic",
            "enforcement walking skeleton proof",
        ])
        .env("NOPAL_PI_BIN", &pi)
        .env("BEISLID_STATE_DIR", &state)
        .env("NOPAL_CONFIG_DIR", temp.path().join("config"))
        .env("PI_CODING_AGENT_DIR", &agent)
        .env("HOME", &home)
        .env("GATE_COUNT_FILE", &gate_count)
        .env("PATH", path)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

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
    assert_eq!(
        local_head.stdout, remote_head.stdout,
        "normal stale push must land while force push remains blocked"
    );

    let events = find_file(&state, "events.jsonl").expect("enforcement ledger events");
    let events = fs::read_to_string(events).unwrap();
    assert!(events.contains("action_decision"));
    assert!(events.contains("gate_attempt"));
    assert!(events.contains("gate_receipt"));
    assert!(events.contains("git.push_force"));
    assert!(events.contains("\"decision\": \"deny\""));
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
