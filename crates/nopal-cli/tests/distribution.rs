#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

use base64::Engine as _;
use flate2::Compression;
use flate2::write::GzEncoder;
use sha2::{Digest as _, Sha512};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

fn git(dir: &Path, args: &[&str]) {
    assert!(
        Command::new("git")
            .args(args)
            .current_dir(dir)
            .status()
            .unwrap()
            .success()
    );
}

#[test]
#[cfg(unix)]
fn fresh_bare_launch_writes_complete_baseline_and_executes_pi_offline() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let args_file = temp.path().join("pi-args");
    let env_file = temp.path().join("pi-offline");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();

    let stub = temp.path().join("pi-stub.sh");
    fs::write(
        &stub,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" > {}\nprintf '%s\\n' \"$PI_OFFLINE\" > {}\nexit 17\n",
            args_file.display(),
            env_file.display()
        ),
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap(), "--", "--no-session"])
        .env("NOPAL_PI_BIN", &stub)
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .env("BEISLID_STATE_DIR", temp.path().join("state"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(17), "{output:?}");
    let expected = [
        ".nopal/nopal.jsonc",
        ".nopal/policy.jsonc",
        ".nopal/gates.jsonc",
        ".nopal/bundle.jsonc",
        ".nopal/nopal.lock",
        ".beislid/workflow.md",
    ];
    for path in expected {
        assert!(repo.join(path).is_file(), "missing generated {path}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains(path),
            "launch did not report {path}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let args = fs::read_to_string(args_file).unwrap();
    assert!(args.lines().any(|arg| arg == "--offline"), "{args}");
    assert!(args.lines().any(|arg| arg == "--no-extensions"), "{args}");
    assert!(args.lines().any(|arg| arg == "--no-skills"), "{args}");
    assert!(
        args.lines().any(|arg| arg == "--no-prompt-templates"),
        "{args}"
    );
    assert!(args.lines().any(|arg| arg == "--no-themes"), "{args}");
    assert!(args.lines().any(|arg| arg == "-e"), "{args}");
    assert!(args.lines().any(|arg| arg == "--no-session"), "{args}");
    assert_eq!(fs::read_to_string(env_file).unwrap().trim(), "1");
}

#[test]
#[cfg(unix)]
fn unknown_first_run_writes_complete_baseline_but_does_not_start_pi() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let marker = temp.path().join("pi-started");
    let stub = temp.path().join("pi-stub.sh");
    fs::write(
        &stub,
        format!("#!/bin/sh\ntouch {}\nexit 0\n", marker.display()),
    )
    .unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap(), "--json"])
        .env("NOPAL_PI_BIN", &stub)
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!marker.exists());
    assert!(repo.join(".nopal/nopal.jsonc").is_file());
    let gates = fs::read_to_string(repo.join(".nopal/gates.jsonc")).unwrap();
    assert!(gates.contains("nopal.gates/v2"));
    assert!(gates.contains("needs_configuration"));
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        document["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| { diagnostic["code"] == "gate_configuration_required" })
    );
}

#[test]
fn preflight_only_configuration_does_not_unblock_unknown_generated_gates() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("extensions/policy-gate");
    nopal_core::scaffold::write_baseline(
        &repo,
        nopal_core::distribution::BuiltinDistribution {
            version: env!("CARGO_PKG_VERSION"),
            root: &adapter,
        },
    )
    .unwrap();
    let gates_path = repo.join(".nopal/gates.jsonc");
    let mut gates: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&gates_path).unwrap()).unwrap();
    gates["preflights"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "readiness",
            "stage": "run_start",
            "argv": ["true"]
        }));
    fs::write(&gates_path, serde_json::to_string_pretty(&gates).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap(), "--json", "--dry-run"])
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        document["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| { diagnostic["code"] == "gate_configuration_required" })
    );
}

#[test]
fn ambiguous_first_run_reports_all_evidence_without_writing_authority() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(
        repo.join("package.json"),
        r#"{"scripts":{"test":"node test.js"}}"#,
    )
    .unwrap();
    fs::write(repo.join("package-lock.json"), "{}\n").unwrap();
    fs::write(repo.join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap(), "--json"])
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(!repo.join(".nopal").exists());
    assert!(!repo.join(".beislid").exists());
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(document["scaffold"], "none");
    assert!(
        document["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| {
                diagnostic["code"] == "gate_scaffold_ambiguous"
                    && diagnostic["message"].as_str().is_some_and(|message| {
                        message.contains("package-lock.json") && message.contains("pnpm-lock.yaml")
                    })
            })
    );
}

#[test]
fn generated_gate_evidence_drift_blocks_launch_until_explicit_authority_exists() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers=[]\n").unwrap();
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("extensions/policy-gate");
    nopal_core::scaffold::write_baseline(
        &repo,
        nopal_core::distribution::BuiltinDistribution {
            version: env!("CARGO_PKG_VERSION"),
            root: &adapter,
        },
    )
    .unwrap();

    fs::remove_file(repo.join("Cargo.toml")).unwrap();
    fs::write(repo.join("go.mod"), "module example.test/demo\n").unwrap();
    let blocked = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap(), "--json", "--dry-run"])
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(blocked.status.code(), Some(1), "{blocked:?}");
    let document: serde_json::Value = serde_json::from_slice(&blocked.stdout).unwrap();
    assert!(
        document["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| { diagnostic["code"] == "gate_scaffold_drift" })
    );

    let gates_path = repo.join(".nopal/gates.jsonc");
    let mut gates: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&gates_path).unwrap()).unwrap();
    gates["gates"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "explicit-go",
            "stage": "pre_pr",
            "argv": ["go", "test", "./..."],
            "parallel_safe": false,
            "mutates": false
        }));
    fs::write(&gates_path, serde_json::to_string_pretty(&gates).unwrap()).unwrap();

    let explicit = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap(), "--json", "--dry-run"])
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(explicit.status.code(), Some(0), "{explicit:?}");

    let selection = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args([
            "--dir",
            repo.to_str().unwrap(),
            "--json",
            "gates",
            "select",
            "--stage",
            "pre_pr",
        ])
        .output()
        .unwrap();
    assert_eq!(selection.status.code(), Some(0), "{selection:?}");
    let selected: serde_json::Value = serde_json::from_slice(&selection.stdout).unwrap();
    assert_eq!(
        selected["selected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|gate| gate["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["explicit-go"]
    );
    assert!(
        selected["skipped"]
            .as_array()
            .unwrap()
            .iter()
            .any(|gate| { gate["reason"] == "superseded_by_explicit_authority" })
    );
}

#[test]
fn partial_beislid_nopal_and_legacy_states_are_preserved_and_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let cases = ["beislid", "nopal", "legacy"];
    for case in cases {
        let repo = temp.path().join(case);
        fs::create_dir_all(&repo).unwrap();
        git(&repo, &["init", "-q"]);
        let marker = match case {
            "beislid" => repo.join(".beislid/workflow.md"),
            "nopal" => repo.join(".nopal/nopal.jsonc"),
            "legacy" => repo
                .join(nopal_core::discover::LEGACY_DIR)
                .join("state.json"),
            _ => unreachable!(),
        };
        fs::create_dir_all(marker.parent().unwrap()).unwrap();
        fs::write(&marker, format!("preserve-{case}\n")).unwrap();
        if case == "legacy" {
            fs::create_dir_all(repo.join(".nopal")).unwrap();
        }
        let output = Command::new(env!("CARGO_BIN_EXE_nopal"))
            .args(["--dir", repo.to_str().unwrap(), "--json", "--dry-run"])
            .env("NOPAL_DATA_DIR", temp.path().join("data"))
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(1), "{case}: {output:?}");
        assert_eq!(
            fs::read_to_string(&marker).unwrap(),
            format!("preserve-{case}\n")
        );
        let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(document["scaffold"], "none");
        assert!(
            document["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|diagnostic| {
                    let code = diagnostic["code"].as_str().unwrap_or_default();
                    code == "scaffold_incomplete" || code == "scaffold_legacy_detected"
                })
        );
        if case != "nopal" {
            assert!(!repo.join(".nopal/nopal.lock").exists());
        }
    }

    let invalid = temp.path().join("invalid");
    fs::create_dir_all(&invalid).unwrap();
    git(&invalid, &["init", "-q"]);
    let adapter = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("extensions/policy-gate");
    nopal_core::scaffold::write_baseline(
        &invalid,
        nopal_core::distribution::BuiltinDistribution {
            version: env!("CARGO_PKG_VERSION"),
            root: &adapter,
        },
    )
    .unwrap();
    fs::write(invalid.join(".nopal/bundle.jsonc"), "invalid-sentinel\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", invalid.to_str().unwrap(), "--json", "--dry-run"])
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert_eq!(
        fs::read_to_string(invalid.join(".nopal/bundle.jsonc")).unwrap(),
        "invalid-sentinel\n"
    );
    assert!(
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "bundle_parse_error")
    );
}

#[test]
#[cfg(unix)]
fn ambient_resources_are_disabled_by_default_and_only_checked_in_non_executable_opt_in_is_honored()
{
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(repo.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
    let stub = temp.path().join("pi-stub.sh");
    fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
    let scaffold = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap()])
        .env("NOPAL_PI_BIN", &stub)
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(scaffold.status.code(), Some(0), "{scaffold:?}");

    let default = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap(), "--json", "--dry-run"])
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();
    let default_doc: serde_json::Value = serde_json::from_slice(&default.stdout).unwrap();
    assert!(
        default_doc["pi_argv"]
            .as_array()
            .unwrap()
            .iter()
            .any(|argument| argument == "--no-skills")
    );

    let bundle_path = repo.join(".nopal/bundle.jsonc");
    let bundle = fs::read_to_string(&bundle_path).unwrap();
    fs::write(
        &bundle_path,
        bundle.replace(
            "\"inherit_ambient\": []",
            "\"inherit_ambient\": [\"skills\"]",
        ),
    )
    .unwrap();
    let update = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap(), "update", "--write"])
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(update.status.code(), Some(0), "{update:?}");

    let opted_in = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap(), "--json", "--dry-run"])
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(opted_in.status.code(), Some(0), "{opted_in:?}");
    let opted_in_doc: serde_json::Value = serde_json::from_slice(&opted_in.stdout).unwrap();
    let arguments = opted_in_doc["pi_argv"].as_array().unwrap();
    assert!(!arguments.iter().any(|argument| argument == "--no-skills"));
    assert!(
        arguments
            .iter()
            .any(|argument| argument == "--no-extensions")
    );
    assert_eq!(opted_in_doc["ambient_kinds"], serde_json::json!(["skills"]));

    let ambient_flag = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", repo.to_str().unwrap(), "--with-ambient"])
        .env("NOPAL_DATA_DIR", temp.path().join("data"))
        .output()
        .unwrap();
    assert_eq!(ambient_flag.status.code(), Some(2), "{ambient_flag:?}");
}

#[test]
fn workspace_update_previews_then_writes_exact_lock_and_sync_never_rewrites_it() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let package = repo.join("packages/guidance");
    fs::create_dir_all(repo.join(".nopal")).unwrap();
    fs::create_dir_all(package.join("skills/review")).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(
        repo.join(".nopal/nopal.jsonc"),
        r#"{ "version": "nopal.project/v1", "profile": "minimal" }"#,
    )
    .unwrap();
    fs::write(
        package.join("package.json"),
        r#"{ "name": "@team/guidance", "version": "2.3.4" }"#,
    )
    .unwrap();
    fs::write(package.join("skills/review/SKILL.md"), "# Review\n").unwrap();
    fs::write(
        repo.join(".nopal/bundle.jsonc"),
        format!(
            r#"{{
  "version": "nopal.bundle/v2",
  "packages": [
    {{
      "id": "nopal",
      "source": {{ "type": "builtin", "package": "nopal" }},
      "requirement": "={}",
      "resources": [{{ "kind": "extension", "path": "index.ts" }}]
    }},
    {{
      "id": "guidance",
      "source": {{ "type": "workspace", "package": "@team/guidance", "root": "packages/guidance" }},
      "requirement": "=2.3.4",
      "resources": [{{ "kind": "skill", "path": "skills/review" }}]
    }}
  ]
}}"#,
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();

    let command = |extra: &[&str]| {
        let mut args = vec!["--dir", repo.to_str().unwrap(), "--json"];
        args.extend_from_slice(extra);
        Command::new(env!("CARGO_BIN_EXE_nopal"))
            .args(args)
            .env("NOPAL_DATA_DIR", temp.path().join("data"))
            .output()
            .unwrap()
    };

    let preview = command(&["update"]);
    assert_eq!(preview.status.code(), Some(0), "{preview:?}");
    let preview_doc: serde_json::Value = serde_json::from_slice(&preview.stdout).unwrap();
    assert_eq!(preview_doc["wrote"], false);
    assert!(!repo.join(".nopal/nopal.lock").exists());

    let update = command(&["update", "--write"]);
    assert_eq!(update.status.code(), Some(0), "{update:?}");
    let update_doc: serde_json::Value = serde_json::from_slice(&update.stdout).unwrap();
    assert_eq!(update_doc["wrote"], true);
    assert_eq!(update_doc["packages"][0]["id"], "guidance");
    assert_eq!(update_doc["packages"][0]["resolved"], "2.3.4");
    let original_lock = fs::read(repo.join(".nopal/nopal.lock")).unwrap();

    let sync = command(&["sync"]);
    assert_eq!(sync.status.code(), Some(0), "{sync:?}");
    assert_eq!(
        fs::read(repo.join(".nopal/nopal.lock")).unwrap(),
        original_lock
    );

    fs::write(package.join("skills/review/SKILL.md"), "tampered\n").unwrap();
    let invalid = command(&["sync"]);
    assert_eq!(invalid.status.code(), Some(1), "{invalid:?}");
    let invalid_doc: serde_json::Value = serde_json::from_slice(&invalid.stdout).unwrap();
    assert!(
        invalid_doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "distribution_integrity_mismatch")
    );
    assert_eq!(
        fs::read(repo.join(".nopal/nopal.lock")).unwrap(),
        original_lock
    );
}

#[test]
#[cfg(unix)]
fn npm_update_and_sync_verify_sri_extract_safely_and_repair_the_exact_store() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let data = temp.path().join("data");
    let bin = temp.path().join("bin");
    fs::create_dir_all(repo.join(".nopal")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(
        repo.join(".nopal/nopal.jsonc"),
        r#"{ "version": "nopal.project/v1", "profile": "minimal" }"#,
    )
    .unwrap();
    fs::write(
        repo.join(".nopal/bundle.jsonc"),
        format!(
            r#"{{
  "version": "nopal.bundle/v2",
  "packages": [
    {{
      "id": "nopal",
      "source": {{ "type": "builtin", "package": "nopal" }},
      "requirement": "={}",
      "resources": [{{ "kind": "extension", "path": "index.ts" }}]
    }},
    {{
      "id": "review-guidance",
      "source": {{ "type": "npm", "package": "@test/guidance", "registry": "https://registry.example.test" }},
      "requirement": "=1.2.3",
      "resources": [{{ "kind": "skill", "path": "skills/review" }}]
    }}
  ]
}}"#,
            env!("CARGO_PKG_VERSION")
        ),
    )
    .unwrap();
    let archive = temp.path().join("guidance.tgz");
    write_npm_archive(&archive, false);
    let integrity = sha512_sri(&archive);
    let npm = bin.join("npm");
    let npm_calls = temp.path().join("npm-calls");
    write_fake_npm(&npm);
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let command = |extra: &[&str]| {
        let mut args = vec!["--dir", repo.to_str().unwrap(), "--json"];
        args.extend_from_slice(extra);
        Command::new(env!("CARGO_BIN_EXE_nopal"))
            .args(args)
            .env("PATH", &path)
            .env("NOPAL_DATA_DIR", &data)
            .env("FAKE_NPM_ARCHIVE", &archive)
            .env("FAKE_NPM_INTEGRITY", &integrity)
            .env("FAKE_NPM_CALLS", &npm_calls)
            .output()
            .unwrap()
    };

    let update = command(&["update", "--write"]);
    assert_eq!(update.status.code(), Some(0), "{update:?}");
    let lock: nopal_core::distribution::LockDocument =
        serde_json::from_slice(&fs::read(repo.join(".nopal/nopal.lock")).unwrap()).unwrap();
    let locked = lock
        .packages
        .iter()
        .find(|package| package.id == "review-guidance")
        .unwrap();
    assert_eq!(locked.resolved, "1.2.3");
    assert_eq!(locked.artifact_integrity, integrity);
    assert_eq!(fs::read_to_string(&npm_calls).unwrap(), "pack\n");

    let missing_launch = command(&["--dry-run"]);
    assert!(!missing_launch.status.success(), "{missing_launch:?}");
    assert_eq!(
        fs::read_to_string(&npm_calls).unwrap(),
        "pack\n",
        "bare launch must not invoke the package adapter"
    );

    let unavailable = temp.path().join("guidance-away.tgz");
    fs::rename(&archive, &unavailable).unwrap();
    let missing = command(&["sync"]);
    assert_eq!(missing.status.code(), Some(1), "{missing:?}");
    let missing_doc: serde_json::Value = serde_json::from_slice(&missing.stdout).unwrap();
    assert!(
        missing_doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |diagnostic| diagnostic["message"].as_str().is_some_and(|message| {
                    message.contains("package \"review-guidance\"")
                        && message.contains("npm source \"@test/guidance\"")
                        && message.contains("control boundary npm_pack")
                })
            )
    );
    fs::rename(&unavailable, &archive).unwrap();

    let store_root = data.join("packages");
    let escaped_store = temp.path().join("escaped-store");
    fs::create_dir_all(&store_root).unwrap();
    fs::create_dir_all(&escaped_store).unwrap();
    fs::remove_dir(store_root.join("npm")).unwrap();
    std::os::unix::fs::symlink(&escaped_store, store_root.join("npm")).unwrap();
    let escaped = command(&["sync"]);
    assert_eq!(escaped.status.code(), Some(1), "{escaped:?}");
    let escaped_doc: serde_json::Value = serde_json::from_slice(&escaped.stdout).unwrap();
    assert!(
        escaped_doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |diagnostic| diagnostic["message"].as_str().is_some_and(|message| message
                    .contains("control boundary installed_store")
                    && message.contains("not a real directory"))
            )
    );
    assert_eq!(fs::read_dir(&escaped_store).unwrap().count(), 0);
    fs::remove_file(store_root.join("npm")).unwrap();

    let lock_path = repo.join(".nopal/nopal.lock");
    let commented_lock = fs::read_to_string(&lock_path).unwrap().replacen(
        '{',
        "{\n  // Exact lock comments are accepted consistently by launch and sync.",
        1,
    );
    fs::write(&lock_path, &commented_lock).unwrap();
    let sync = command(&["sync"]);
    assert_eq!(sync.status.code(), Some(0), "{sync:?}");
    assert_eq!(fs::read_to_string(&lock_path).unwrap(), commented_lock);
    let sync_doc: serde_json::Value = serde_json::from_slice(&sync.stdout).unwrap();
    assert_eq!(sync_doc["changed"], true);
    let store = nopal_core::distribution::npm_store_path(
        &data.join("packages"),
        "@test/guidance",
        "1.2.3",
        &integrity,
    );
    let skill = store.join("skills/review/SKILL.md");
    assert_eq!(fs::read_to_string(&skill).unwrap(), "# Review\n");

    fs::write(&skill, "tampered\n").unwrap();
    let launch = command(&["--dry-run"]);
    assert!(!launch.status.success(), "{launch:?}");

    let repaired = command(&["sync"]);
    assert_eq!(repaired.status.code(), Some(0), "{repaired:?}");
    assert_eq!(fs::read_to_string(&skill).unwrap(), "# Review\n");

    let manifest = store.join("package.json");
    fs::write(
        &manifest,
        r#"{ "name": "@test/guidance", "version": "9.9.9" }"#,
    )
    .unwrap();
    let identity_invalid = command(&["--dry-run"]);
    assert!(!identity_invalid.status.success(), "{identity_invalid:?}");
    let identity_repaired = command(&["sync"]);
    assert_eq!(
        identity_repaired.status.code(),
        Some(0),
        "{identity_repaired:?}"
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(manifest).unwrap()).unwrap()["version"],
        "1.2.3"
    );
}

#[test]
#[cfg(unix)]
fn update_refuses_to_write_a_lock_for_bundle_bytes_changed_during_resolution() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let data = temp.path().join("data");
    let bin = temp.path().join("bin");
    fs::create_dir_all(repo.join(".nopal")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    git(&repo, &["init", "-q"]);
    let bundle_path = repo.join(".nopal/bundle.jsonc");
    let bundle = r#"{
  "version": "nopal.bundle/v2",
  "packages": [{
    "id": "guidance",
    "source": { "type": "npm", "package": "@test/guidance", "registry": "https://registry.example.test" },
    "requirement": "=1.2.3",
    "resources": [{ "kind": "skill", "path": "skills/review" }]
  }]
}"#;
    fs::write(&bundle_path, bundle).unwrap();
    let archive = temp.path().join("guidance.tgz");
    write_npm_archive(&archive, false);
    let integrity = sha512_sri(&archive);
    let npm = bin.join("npm");
    write_fake_npm(&npm);
    let ready = temp.path().join("npm-ready");
    let resume = temp.path().join("npm-resume");
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let child = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args([
            "--dir",
            repo.to_str().unwrap(),
            "--json",
            "update",
            "--write",
        ])
        .env("PATH", path)
        .env("NOPAL_DATA_DIR", &data)
        .env("FAKE_NPM_ARCHIVE", &archive)
        .env("FAKE_NPM_INTEGRITY", &integrity)
        .env("FAKE_NPM_READY", &ready)
        .env("FAKE_NPM_RESUME", &resume)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    for _ in 0..500 {
        if ready.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(ready.exists(), "fake npm never reached the resolution seam");
    fs::write(
        &bundle_path,
        format!("{bundle}\n// concurrent checked-in contract edit\n"),
    )
    .unwrap();
    fs::write(&resume, "resume\n").unwrap();

    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        document["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "distribution_lock_drift"
                && diagnostic["message"]
                    .as_str()
                    .is_some_and(|message| message.contains("update_transaction")))
    );
    assert!(!repo.join(".nopal/nopal.lock").exists());
}

#[test]
#[cfg(unix)]
fn npm_update_rejects_link_entries_before_writing_a_lock() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    let bin = temp.path().join("bin");
    fs::create_dir_all(repo.join(".nopal")).unwrap();
    fs::create_dir_all(&bin).unwrap();
    git(&repo, &["init", "-q"]);
    fs::write(
        repo.join(".nopal/bundle.jsonc"),
        r#"{
  "version": "nopal.bundle/v2",
  "packages": [{
    "id": "guidance",
    "source": { "type": "npm", "package": "@test/guidance", "registry": "https://registry.example.test" },
    "requirement": "1.2.3",
    "resources": [{ "kind": "skill", "path": "skills/review" }]
  }]
}"#,
    )
    .unwrap();
    let archive = temp.path().join("malicious.tgz");
    write_npm_archive(&archive, true);
    let integrity = sha512_sri(&archive);
    write_fake_npm(&bin.join("npm"));
    let path = format!(
        "{}:{}",
        bin.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let command = |oversized: bool| {
        let mut command = Command::new(env!("CARGO_BIN_EXE_nopal"));
        command
            .args([
                "--dir",
                repo.to_str().unwrap(),
                "--json",
                "update",
                "--write",
            ])
            .env("PATH", &path)
            .env("NOPAL_DATA_DIR", temp.path().join("data"))
            .env("FAKE_NPM_ARCHIVE", &archive)
            .env("FAKE_NPM_INTEGRITY", &integrity);
        if oversized {
            command.env("FAKE_NPM_OVERSIZED", "1");
        }
        command.output().unwrap()
    };

    let oversized = command(true);
    assert_eq!(oversized.status.code(), Some(1), "{oversized:?}");
    let oversized_document: serde_json::Value = serde_json::from_slice(&oversized.stdout).unwrap();
    assert!(
        oversized_document["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["message"]
                .as_str()
                .is_some_and(|message| message.contains("control boundary npm_pack")
                    && message.contains("output exceeded bounded capture limits")))
    );
    assert!(!repo.join(".nopal/nopal.lock").exists());

    let output = command(false);
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    let document: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(
        document["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(
                |diagnostic| diagnostic["message"].as_str().is_some_and(|message| {
                    message.contains("package \"guidance\"")
                        && message.contains("npm source \"@test/guidance\"")
                        && message.contains("control boundary archive_extraction")
                        && message.contains("links")
                })
            )
    );
    assert!(!repo.join(".nopal/nopal.lock").exists());
}

#[cfg(unix)]
fn write_fake_npm(path: &Path) {
    fs::write(
        path,
        r#"#!/bin/sh
set -eu
destination=''
while [ "$#" -gt 0 ]; do
  if [ "$1" = '--pack-destination' ]; then
    shift
    destination="$1"
  fi
  shift
done
if [ "${FAKE_NPM_OVERSIZED:-}" = '1' ]; then
  dd if=/dev/zero bs=1048576 count=3 2>/dev/null | tr '\000' x
  exit 0
fi
if [ -n "${FAKE_NPM_READY:-}" ]; then
  : > "$FAKE_NPM_READY"
  while [ ! -e "$FAKE_NPM_RESUME" ]; do sleep 0.01; done
fi
cp "$FAKE_NPM_ARCHIVE" "$destination/package.tgz"
if [ -n "${FAKE_NPM_CALLS:-}" ]; then printf 'pack\n' >> "$FAKE_NPM_CALLS"; fi
printf '[{"name":"@test/guidance","version":"1.2.3","filename":"package.tgz","integrity":"%s"}]\n' "$FAKE_NPM_INTEGRITY"
"#,
    )
    .unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn write_npm_archive(path: &Path, malicious_link: bool) {
    let file = fs::File::create(path).unwrap();
    let encoder = GzEncoder::new(file, Compression::default());
    let mut archive = tar::Builder::new(encoder);
    append_tar_file(
        &mut archive,
        "package/package.json",
        br#"{ "name": "@test/guidance", "version": "1.2.3" }"#,
    );
    append_tar_file(
        &mut archive,
        "package/skills/review/SKILL.md",
        b"# Review\n",
    );
    if malicious_link {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_mode(0o777);
        header.set_cksum();
        archive
            .append_link(
                &mut header,
                "package/skills/review/escape",
                "../../../../outside",
            )
            .unwrap();
    }
    archive.into_inner().unwrap().finish().unwrap();
}

fn append_tar_file<W: std::io::Write>(archive: &mut tar::Builder<W>, path: &str, bytes: &[u8]) {
    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    archive.append_data(&mut header, path, bytes).unwrap();
}

fn sha512_sri(path: &Path) -> String {
    let bytes = fs::read(path).unwrap();
    let digest = Sha512::digest(bytes);
    format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode(digest)
    )
}
