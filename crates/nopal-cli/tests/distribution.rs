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
