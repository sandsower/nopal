#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;
use std::process::Command;

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
