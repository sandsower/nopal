#![cfg(unix)]
#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
    let distribution = temp.path().join("distribution");
    let adapter = distribution.join("extensions/policy-gate");
    let beislid = distribution.join("resources/beislid");
    for directory in [&repo, &state, &home, &agent, &bin, &adapter] {
        fs::create_dir_all(directory).unwrap();
    }
    fs::create_dir_all(beislid.join("skills")).unwrap();
    fs::copy(
        source_root.join("resources/beislid/LICENSE"),
        beislid.join("LICENSE"),
    )
    .unwrap();
    fs::copy(
        source_root.join("resources/beislid/provenance.json"),
        beislid.join("provenance.json"),
    )
    .unwrap();
    for file in ["index.ts", "classifier.ts", "guard.ts", "nopal-cli.ts"] {
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
    let network_read_marker = temp.path().join("network-read-ran");
    let network_write_marker = temp.path().join("network-write-ran");
    let ambient_curl_marker = temp.path().join("ambient-curl-config-observed");
    let weakened_effect_marker = temp.path().join("weakened-effect-ran");
    let bypass_marker = temp.path().join("classifier-bypass-ran");
    let ambient_carrier_marker = temp.path().join("ambient-carrier-ran");
    let ripgrep_config = temp.path().join("ripgrep.conf");
    let ripgrep_pre = temp.path().join("rg-pre.sh");
    write(
        &ripgrep_pre,
        &format!(
            "#!/bin/sh\ntouch {:?}\ncat \"$1\"\n",
            ambient_carrier_marker
        ),
    );
    fs::set_permissions(&ripgrep_pre, fs::Permissions::from_mode(0o755)).unwrap();
    write(
        &ripgrep_config,
        &format!("--pre={}\n", ripgrep_pre.display()),
    );
    write(
        &bin.join("nopal"),
        &format!("#!/bin/sh\ntouch {:?}\nexit 99\n", substituted_cli_marker),
    );
    write(
        &bin.join("gh"),
        "#!/bin/sh\ntouch \"$PROOF_NETWORK_READ_MARKER\"\n",
    );
    write(
        &bin.join("curl"),
        "#!/bin/sh\n[ ! -f \"$HOME/.curlrc\" ] || touch \"$PROOF_AMBIENT_CURL_MARKER\"\ntouch \"$PROOF_NETWORK_WRITE_MARKER\"\n",
    );
    write(&home.join(".curlrc"), "url = https://attacker.invalid\n");
    write(
        &bin.join("vercel"),
        "#!/bin/sh\ntouch \"$PROOF_WEAKENED_EFFECT_MARKER\"\n",
    );
    for executable in ["nopal", "gh", "curl", "vercel"] {
        fs::set_permissions(bin.join(executable), fs::Permissions::from_mode(0o755)).unwrap();
    }

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
    command(
        &repo,
        &git,
        &["push", "origin", "HEAD:refs/heads/protected-delete-proof"],
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
            { "id": "allow-network-read", "actions": ["gh.read"], "decision": "allow" },
            { "id": "attempt-weaken-user-deny", "actions": ["deploy.mutate"], "decision": "allow" },
            { "id": "ask-network-write", "actions": ["network.transfer"], "decision": "ask" },
            { "id": "deny-force", "actions": ["git.push_force"], "decision": "deny" },
            { "id": "allow-workspace-change", "actions": ["fs.write", "git.add", "git.commit"], "decision": "allow" }
          ] } }
        }"#,
    );
    write(
        &temp.path().join("config/policy.jsonc"),
        r#"{
          "version": "nopal.policy/v1",
          "modes": { "supervised_auto": {
            "default_decision": "allow",
            "default_placement": "shared_user_runtime",
            "rules": [
              { "id": "user-deny-deploy", "actions": ["deploy.mutate"], "decision": "deny" }
            ]
          } }
        }"#,
    );
    let gate_count = temp.path().join("gate-count");
    let gate_script = repo.join("gate-proof.sh");
    write(
        &gate_script,
        &format!(
            "#!/bin/sh\nfor fd in /dev/fd/[3-9]* /proc/$PPID/fd/*; do [ -r \"$fd\" ] || continue; value=$(dd if=\"$fd\" bs=64 count=1 2>/dev/null); case \"$value\" in ????????????????????????????????????????????????????????????????) exit 91;; esac; done\ncount=$(cat {:?} 2>/dev/null || echo 0)\necho $((count + 1)) > {:?}\n",
            gate_count, gate_count
        ),
    );
    fs::set_permissions(&gate_script, fs::Permissions::from_mode(0o755)).unwrap();
    write(
        &repo.join(".nopal/gates.jsonc"),
        &serde_json::to_string_pretty(&serde_json::json!({
          "version": "nopal.gates/v1",
          "gates": [
            {
              "id": "parallel-read-proof",
              "stage": "continuous",
              "argv": ["true"],
              "parallel_safe": true,
              "mutates": false
            },
            {
              "id": "proof",
              "stage": "pre_pr",
              "command": "./gate-proof.sh"
            }
          ]
        }))
        .unwrap(),
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
              {{ "kind": "extension", "path": "extensions/policy-gate/index.ts" }},
              {{ "kind": "extension", "path": "extensions/policy-gate/deterministic-enforcement-provider.mjs" }}
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
            root: &distribution,
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
            "--offline",
            "--approve",
            "--provider",
            "nopal-enforcement-proof",
            "--model",
            "deterministic",
            "enforcement walking skeleton proof",
        ])
        .env("NOPAL_TEST_PI_BIN", &pi)
        .env("NOPAL_DISTRIBUTION_ROOT", temp.path().join("distribution"))
        .env("BEISLID_STATE_DIR", &state)
        .env("NOPAL_CONFIG_DIR", temp.path().join("config"))
        .env("PI_CODING_AGENT_DIR", &agent)
        .env("HOME", &home)
        .env("AUTHORITY_FILE", ".nopal/policy.jsonc")
        .env("PROOF_ADAPTER_INDEX", &enforcement)
        .env("PROOF_GIT_BIN", &git)
        .env("PROOF_NETWORK_READ_MARKER", &network_read_marker)
        .env("PROOF_NETWORK_WRITE_MARKER", &network_write_marker)
        .env("PROOF_AMBIENT_CURL_MARKER", &ambient_curl_marker)
        .env("PROOF_WEAKENED_EFFECT_MARKER", &weakened_effect_marker)
        .env("PROOF_BYPASS_MARKER", &bypass_marker)
        .env("PATH", &path)
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
        "delete-refspec-denied",
        "read-parallel-a",
        "read-parallel-b",
        "read-direct",
        "grep-direct",
        "find-direct",
        "ls-direct",
        "write-change",
        "edit-change",
        "rg-config-read",
        "network-read-allowed",
        "repository-weakening-denied",
        "dependency-install-denied",
        "bundled-force-denied",
        "helper-remote-denied",
        "tmux-format-exec-denied",
        "tmux-list-format-exec-denied",
        "find-output-denied",
        "ancestor-read-denied",
        "pi-settings-write-denied",
        "secret-bearing-denied",
        "unknown-denied",
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

    assert_eq!(
        fs::read_to_string(&gate_count)
            .unwrap_or_else(|error| {
                panic!(
                    "{error}: stderr={} stdout={pi_events}",
                    String::from_utf8_lossy(&output.stderr)
                )
            })
            .trim(),
        "2"
    );
    assert!(
        network_read_marker.is_file(),
        "an allowed network-read action must reach the exact fake target"
    );
    assert!(
        !network_write_marker.exists(),
        "an ask action must not execute without an explicit UI response"
    );
    assert!(
        !weakened_effect_marker.exists(),
        "repository allow policy must not weaken the user deny"
    );
    assert!(
        !bypass_marker.exists(),
        "read-classified shell carriers must not execute mutation effects"
    );
    command(
        temp.path(),
        &git,
        &[
            "--git-dir",
            remote.to_str().unwrap(),
            "show-ref",
            "--verify",
            "refs/heads/protected-delete-proof",
        ],
    );
    assert!(
        !ambient_carrier_marker.exists(),
        "ambient executable configuration must not reach audited commands"
    );
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
        authorized_head.stdout,
        remote_head.stdout,
        "the stale-receipt push must land before every adversarial force-push form is blocked; remote stderr={}; pi events={}",
        String::from_utf8_lossy(&remote_head.stderr),
        pi_events
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

    let foreign = Command::new(&nopal)
        .current_dir(&repo)
        .args([
            "--dir",
            repo.to_str().unwrap(),
            "--",
            "--mode",
            "json",
            "--print",
            "--no-session",
            "--offline",
            "--approve",
            "--provider",
            "nopal-enforcement-proof",
            "--model",
            "deterministic",
            "foreign receipt proof",
        ])
        .env("NOPAL_TEST_PI_BIN", &pi)
        .env("NOPAL_DISTRIBUTION_ROOT", temp.path().join("distribution"))
        .env("BEISLID_STATE_DIR", &state)
        .env("NOPAL_CONFIG_DIR", temp.path().join("config"))
        .env("PI_CODING_AGENT_DIR", &agent)
        .env("HOME", &home)
        .env("PROOF_GIT_BIN", &git)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(foreign.status.success(), "{foreign:?}");
    assert_eq!(
        fs::read_to_string(&gate_count).unwrap().trim(),
        "3",
        "a receipt from another Nopal launch must not authorize the new run"
    );

    let failing_gate = Command::new(&nopal)
        .current_dir(&repo)
        .args([
            "--dir",
            repo.to_str().unwrap(),
            "--",
            "--mode",
            "json",
            "--print",
            "--no-session",
            "--offline",
            "--approve",
            "--provider",
            "nopal-enforcement-proof",
            "--model",
            "deterministic",
            "failing gate proof",
        ])
        .env("NOPAL_TEST_PI_BIN", &pi)
        .env("NOPAL_DISTRIBUTION_ROOT", temp.path().join("distribution"))
        .env("BEISLID_STATE_DIR", &state)
        .env("NOPAL_CONFIG_DIR", temp.path().join("config"))
        .env("PI_CODING_AGENT_DIR", &agent)
        .env("HOME", &home)
        .env("PROOF_GIT_BIN", &git)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert!(failing_gate.status.success(), "{failing_gate:?}");
    let failing_gate_events = String::from_utf8_lossy(&failing_gate.stdout);
    assert!(failing_gate_events.contains("write-failing-gate"));
    assert!(failing_gate_events.contains("failing-gate-push-denied"));
    assert_eq!(
        fs::read_to_string(&gate_count).unwrap().trim(),
        "3",
        "a failed gate must block the protected push effect"
    );

    let mut ask = Command::new(&nopal)
        .current_dir(&repo)
        .args([
            "--dir",
            repo.to_str().unwrap(),
            "--",
            "--mode",
            "rpc",
            "--no-session",
            "--offline",
            "--approve",
            "--provider",
            "nopal-enforcement-proof",
            "--model",
            "deterministic",
        ])
        .env("NOPAL_TEST_PI_BIN", &pi)
        .env("NOPAL_DISTRIBUTION_ROOT", temp.path().join("distribution"))
        .env("BEISLID_STATE_DIR", &state)
        .env("NOPAL_CONFIG_DIR", temp.path().join("config"))
        .env("PI_CODING_AGENT_DIR", &agent)
        .env("HOME", &home)
        .env("PROOF_NETWORK_WRITE_MARKER", &network_write_marker)
        .env("PROOF_AMBIENT_CURL_MARKER", &ambient_curl_marker)
        .env("PROOF_GIT_BIN", &git)
        .env("PATH", &path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut ask_stdin = ask.stdin.take().unwrap();
    let mut ask_stdout = BufReader::new(ask.stdout.take().unwrap());
    writeln!(
        ask_stdin,
        "{}",
        serde_json::json!({
            "id": "ask-proof",
            "type": "prompt",
            "message": "explicit ask approval proof"
        })
    )
    .unwrap();
    ask_stdin.flush().unwrap();
    let mut saw_explicit_prompt = false;
    let mut line = String::new();
    loop {
        line.clear();
        assert!(
            ask_stdout.read_line(&mut line).unwrap() > 0,
            "RPC Pi exited before settling"
        );
        let event: serde_json::Value = serde_json::from_str(line.trim_end()).unwrap();
        if event["type"] == "extension_ui_request" && event["method"] == "select" {
            saw_explicit_prompt = true;
            writeln!(
                ask_stdin,
                "{}",
                serde_json::json!({
                    "type": "extension_ui_response",
                    "id": event["id"],
                    "value": "Yes, run it"
                })
            )
            .unwrap();
            ask_stdin.flush().unwrap();
        }
        if event["type"] == "agent_settled" {
            break;
        }
    }
    assert!(
        saw_explicit_prompt,
        "the real Pi RPC hook must request exact human approval"
    );
    assert!(
        network_write_marker.is_file(),
        "the approved exact ask action must be released once"
    );
    assert!(
        !ambient_curl_marker.exists(),
        "the released external action must not observe the caller's ambient curl configuration"
    );
    drop(ask_stdin);
    let _ = ask.kill();
    let _ = ask.wait();

    let ambient_carrier = Command::new(&nopal)
        .current_dir(&repo)
        .args([
            "--dir",
            repo.to_str().unwrap(),
            "--",
            "--mode",
            "json",
            "-p",
            "--no-session",
            "--offline",
            "ambient carrier proof",
        ])
        .env("NOPAL_TEST_PI_BIN", &pi)
        .env("NOPAL_DISTRIBUTION_ROOT", temp.path().join("distribution"))
        .env("BEISLID_STATE_DIR", &state)
        .env("NOPAL_CONFIG_DIR", temp.path().join("config"))
        .env("PI_CODING_AGENT_DIR", &agent)
        .env("HOME", &home)
        .env("RIPGREP_CONFIG_PATH", &ripgrep_config)
        .env("GIT_EDITOR", &ripgrep_pre)
        .env("GIT_EXEC_PATH", temp.path().join("untrusted-git-exec"))
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(
        ambient_carrier.status.code(),
        Some(2),
        "{ambient_carrier:?}"
    );
    assert!(String::from_utf8_lossy(&ambient_carrier.stderr).contains("ambient GIT_EDITOR"));
    assert!(!ambient_carrier_marker.exists());

    let actual_pi_started = temp.path().join("actual-pi-missing-guard-probe");
    let missing_guard_wrapper = temp.path().join("missing-guard-pi.sh");
    write(
        &missing_guard_wrapper,
        &format!(
            "#!/bin/sh\ntouch {:?}\nrm -f {:?}\nexec {:?} \"$@\"\n",
            actual_pi_started,
            adapter.join("guard.ts"),
            pi
        ),
    );
    fs::set_permissions(&missing_guard_wrapper, fs::Permissions::from_mode(0o755)).unwrap();
    let missing_guard = Command::new(&nopal)
        .current_dir(&repo)
        .args([
            "--dir",
            repo.to_str().unwrap(),
            "--",
            "--mode",
            "json",
            "-p",
            "--no-session",
            "--offline",
            "missing enforcement proof",
        ])
        .env("NOPAL_TEST_PI_BIN", &missing_guard_wrapper)
        .env("NOPAL_DISTRIBUTION_ROOT", temp.path().join("distribution"))
        .env("BEISLID_STATE_DIR", &state)
        .env("NOPAL_CONFIG_DIR", temp.path().join("config"))
        .env("PI_CODING_AGENT_DIR", &agent)
        .env("HOME", &home)
        .env("PROOF_GIT_BIN", &git)
        .env("PATH", &path)
        .output()
        .unwrap();
    assert_eq!(missing_guard.status.code(), Some(2), "{missing_guard:?}");
    assert!(actual_pi_started.exists(), "{missing_guard:?}");
    assert!(
        String::from_utf8_lossy(&missing_guard.stderr)
            .contains("Pi enforcement capability probe exited"),
        "{}",
        String::from_utf8_lossy(&missing_guard.stderr)
    );

    let events = collect_named_files(&state, "events.jsonl");
    assert!(events.contains("action_decision"));
    assert!(events.contains("gate_attempt"));
    assert!(events.contains("gate_receipt"));
    assert!(events.contains("\"exit_code\": 9"));
    assert!(events.contains("git.push_force"));
    assert!(events.contains("\"decision\": \"deny\""));
    assert!(events.contains("action_approval"));
    assert!(events.contains("authorization_release"));
    assert!(events.contains("tool_outcome"));
    assert!(events.contains("\"outcome\": \"success\""));
    assert!(events.contains("read-parallel-a"));
    assert!(events.contains("read-parallel-b"));
    assert!(events.contains("parallel-read-proof"));
    assert!(count_files_in_named_directories(&state, "parallel-read-proof") >= 2);
    assert!(
        !substituted_cli_marker.exists(),
        "the adapter must invoke the resolved launch binary instead of a PATH substitute"
    );
}

fn count_files_in_named_directories(root: &Path, directory_name: &str) -> usize {
    let Ok(entries) = fs::read_dir(root) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| {
            let path = entry.path();
            if !path.is_dir() {
                0
            } else if path.file_name().and_then(|value| value.to_str()) == Some(directory_name) {
                fs::read_dir(path)
                    .map(|children| {
                        children
                            .flatten()
                            .filter(|child| child.path().is_file())
                            .count()
                    })
                    .unwrap_or(0)
            } else {
                count_files_in_named_directories(&path, directory_name)
            }
        })
        .sum()
}

fn collect_named_files(root: &Path, name: &str) -> String {
    let mut combined = String::new();
    let Ok(entries) = fs::read_dir(root) else {
        return combined;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            combined.push_str(&collect_named_files(&path, name));
        } else if path.file_name().and_then(|value| value.to_str()) == Some(name) {
            combined.push_str(&fs::read_to_string(path).unwrap());
        }
    }
    combined
}
