// Integration tests may panic freely; clippy's in-tests allowance only covers
// #[test] fns, not shared helpers in the tests/ tree.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::process::{Child, Command, Output, Stdio};
use std::sync::{OnceLock, mpsc};
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

/// An empty directory guaranteed to have no `bundle-default.jsonc` in it,
/// used as the default `NOPAL_CONFIG_DIR` for every spawned `nopal`
/// subprocess in this file: the machine running these
/// tests may have a real `~/.config/nopal/bundle-default.jsonc`, and letting
/// that leak into a fixture would make scaffold behavior depend on whoever's
/// machine is running the suite. Tests that specifically exercise the
/// template mechanism pass their own `.env("NOPAL_CONFIG_DIR", ...)` on top
/// of a `Command` built directly (see the "User-level default-bundle
/// template" test section below), pointing at a fixture directory that does
/// have a template in it. One directory shared process-wide is safe here:
/// nothing ever writes into it.
fn isolated_config_dir() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    DIR.get_or_init(|| tempfile::tempdir().expect("failed to create isolated config dir"))
        .path()
}

const TEST_PLOT_ID: &str = "plot-test";

fn write_established_plot_state(state_dir: &Path) {
    let plots = state_dir.join("plots");
    fs::create_dir_all(&plots).unwrap();
    fs::write(
        plots.join(format!("{TEST_PLOT_ID}.json")),
        serde_json::to_vec(&serde_json::json!({
            "kind": "nopal.plot/v1",
            "plot_id": TEST_PLOT_ID,
            "title": "Test Plot",
            "provisional": false,
            "progress": "planned",
            "conditions": [],
            "seed": {"source": "test", "text": "test"},
            "intent": "Test the managed execution flow",
            "sessions": [],
            "selected_session_id": null,
            "establishment": {
                "event": "kickoff_context_ready",
                "primary_repository_id": "repo-test",
                "effective_workflow": {
                    "source_repository_id": "repo-test",
                    "source_hash": "a".repeat(64),
                    "value": {}
                },
                "applied_requests": [],
                "established_at": "2026-07-12T00:00:00Z"
            },
            "repositories": [],
            "workspaces": [],
            "created_at": "2026-07-12T00:00:00Z",
            "updated_at": "2026-07-12T00:00:00Z"
        }))
        .unwrap(),
    )
    .unwrap();
}

fn nopal_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_nopal"));
    command
        .env_remove("NOPAL_RONDO_CORE_URL")
        .env_remove("NOPAL_RONDO_RUNTIME")
        .env_remove("NOPAL_RONDO_STATE_DIR");
    command
}

fn nopal(args: &[&str]) -> Output {
    nopal_command()
        .args(args)
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .output()
        .expect("failed to spawn nopal binary")
}

fn nopal_with_cwd(cwd: &Path, args: &[&str]) -> Output {
    nopal_command()
        .args(args)
        .current_dir(cwd)
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .output()
        .expect("failed to spawn nopal binary")
}

#[cfg(unix)]
struct LifecycleFixture {
    temp: tempfile::TempDir,
    runtime: std::path::PathBuf,
}

#[cfg(unix)]
impl LifecycleFixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let runtime = temp.path().join("fake-rondo");
        let test_binary = std::env::current_exe().unwrap();
        let script = format!(
            "#!/bin/sh\nready=\"\"\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--ready-file\" ]; then\n    ready=\"$2\"\n    shift 2\n  else\n    shift\n  fi\ndone\nexec env NOPAL_FAKE_RONDO_READY_FILE=\"$ready\" \"{}\" --exact fake_rondo_runtime_process --ignored --nocapture\n",
            test_binary.display()
        );
        fs::write(&runtime, script).unwrap();
        fs::set_permissions(&runtime, fs::Permissions::from_mode(0o700)).unwrap();
        let fixture = Self { temp, runtime };
        write_established_plot_state(&fixture.plot_state());
        fixture
    }

    fn state(&self) -> std::path::PathBuf {
        self.temp.path().join("state")
    }

    fn active_run_count_file(&self) -> std::path::PathBuf {
        self.temp.path().join("active-run-count")
    }

    fn plot_state(&self) -> std::path::PathBuf {
        self.temp.path().join("nopal-state")
    }

    fn nopal(&self, args: &[&str]) -> Output {
        self.command(args)
            .env("NOPAL_CONFIG_DIR", isolated_config_dir())
            .output()
            .expect("failed to spawn lifecycle-aware nopal binary")
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = nopal_command();
        command
            .args(args)
            .env("NOPAL_RONDO_STATE_DIR", self.state())
            .env("NOPAL_RONDO_RUNTIME", &self.runtime)
            .env(
                "NOPAL_FAKE_RONDO_ACTIVE_RUN_COUNT_FILE",
                self.active_run_count_file(),
            );
        command
    }
}

#[cfg(unix)]
impl Drop for LifecycleFixture {
    fn drop(&mut self) {
        if self.state().join("runtime.json").exists() {
            let _ = fs::write(self.active_run_count_file(), "0");
            let _ = self.nopal(&["--json", "rondo", "stop"]);
        }
    }
}

#[cfg(unix)]
#[test]
#[ignore = "helper process launched through the fake Rondo wrapper"]
fn fake_rondo_runtime_process() {
    let ready_file = std::env::var("NOPAL_FAKE_RONDO_READY_FILE").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let identity = || {
        let active_run_count = std::env::var("NOPAL_FAKE_RONDO_ACTIVE_RUN_COUNT_FILE")
            .ok()
            .and_then(|path| fs::read_to_string(path).ok())
            .and_then(|value| value.trim().parse::<u64>().ok())
            .unwrap_or(0);

        serde_json::json!({
            "surface": "rondo.core/v1",
            "runtime_version": "0.1.0",
            "instance_id": "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
            "service_mode": "trackerless_core",
            "ready": true,
            "active_run_count": active_run_count
        })
    };
    let mut bootstrap = identity();
    bootstrap["base_url"] = serde_json::Value::String(base_url);
    fs::write(ready_file, serde_json::to_vec(&bootstrap).unwrap()).unwrap();

    let mut accepted_manifest = None;
    let mut accepted_repo = None;
    let mut accepted_plot = None;
    for stream in listener.incoming() {
        let mut stream = stream.unwrap();
        let request = read_http_request(&mut stream);
        let (status, body) = if request.starts_with("GET /api/v1/health HTTP/1.1") {
            (200, identity().to_string())
        } else if request.starts_with("POST /api/v1/execution-requests HTTP/1.1") {
            let request_body = request.split("\r\n\r\n").nth(1).unwrap();
            let submitted: serde_json::Value = serde_json::from_str(request_body).unwrap();
            let digest = submitted["manifest_sha256"].as_str().unwrap().to_owned();
            let deduplicated = accepted_manifest.as_ref() == Some(&digest);
            accepted_manifest = Some(digest);
            accepted_repo = submitted["repo_id"].as_str().map(str::to_owned);
            accepted_plot = submitted["plot_id"].as_str().map(str::to_owned);
            (
                if deduplicated { 200 } else { 202 },
                serde_json::json!({
                    "surface": "rondo.core/v1",
                    "service_id": "rondo-core",
                    "repo_id": submitted["repo_id"],
                    "plot_id": submitted["plot_id"],
                    "run_id": "run-lifecycle-owned",
                    "status": "running",
                    "event_cursor": "rondo.core/v1:0",
                    "deduplicated": deduplicated
                })
                .to_string(),
            )
        } else if request.starts_with("GET /api/v1/runs/run-lifecycle-owned/events?") {
            (
                200,
                serde_json::json!({
                    "surface": "rondo.core/v1",
                    "repo_id": accepted_repo.as_deref().unwrap(),
                    "plot_id": accepted_plot.as_deref().unwrap(),
                    "run_id": "run-lifecycle-owned",
                    "events": [{"type": "run.completed"}],
                    "next_event_cursor": "rondo.core/v1:1",
                    "has_more": false
                })
                .to_string(),
            )
        } else if request.starts_with("GET /api/v1/runs/run-lifecycle-owned?") {
            (
                200,
                serde_json::json!({
                    "surface": "rondo.core/v1",
                    "repo_id": accepted_repo.as_deref().unwrap(),
                    "plot_id": accepted_plot.as_deref().unwrap(),
                    "run_id": "run-lifecycle-owned",
                    "status": "completed",
                    "last_event": {"type": "run.completed"},
                    "evidence_pointers": [],
                    "event_cursor": "rondo.core/v1:1"
                })
                .to_string(),
            )
        } else {
            panic!("unexpected fake Rondo request: {request}");
        };
        let reason = if status == 202 { "Accepted" } else { "OK" };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    }
}

#[test]
fn shared_nopal_subprocess_helper_removes_ambient_rondo_endpoint_override() {
    let command = nopal_command();
    assert!(
        command
            .get_envs()
            .any(|(key, value)| { key == "NOPAL_RONDO_CORE_URL" && value.is_none() })
    );
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout is not utf-8")
}

fn stderr(out: &Output) -> String {
    String::from_utf8(out.stderr.clone()).expect("stderr is not utf-8")
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(out)).expect("stdout is not valid JSON")
}

struct ScriptedHttpServer {
    base_url: String,
    request_rx: mpsc::Receiver<String>,
    handle: thread::JoinHandle<()>,
}

impl ScriptedHttpServer {
    fn start(responses: Vec<(u16, serde_json::Value)>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
        let address = listener.local_addr().expect("read loopback server address");
        let (request_tx, request_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept loopback request");
                let request = read_http_request(&mut stream);
                request_tx.send(request).expect("record loopback request");
                let body = body.to_string();
                let reason = if status == 202 { "Accepted" } else { "OK" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write loopback response");
            }
        });

        Self {
            base_url: format!("http://{address}"),
            request_rx,
            handle,
        }
    }

    fn finish(self, expected_requests: usize) -> Vec<String> {
        let requests = (0..expected_requests)
            .map(|_| {
                self.request_rx
                    .recv_timeout(Duration::from_secs(2))
                    .expect("observe loopback request")
            })
            .collect();
        self.handle.join().expect("loopback server thread");
        requests
    }
}

fn read_http_request(stream: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    let mut expected_length = None;

    loop {
        let count = stream.read(&mut buffer).expect("read loopback request");
        if count == 0 {
            break;
        }
        bytes.extend_from_slice(&buffer[..count]);

        if expected_length.is_none()
            && let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n")
        {
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let body_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            expected_length = Some(header_end + 4 + body_length);
        }

        if expected_length.is_some_and(|length| bytes.len() >= length) {
            break;
        }
    }

    String::from_utf8(bytes).expect("loopback request is UTF-8")
}

fn write_project(root: &Path) {
    write_project_with_placement(root, "dedicated_repo_runtime");
}

fn write_project_with_placement(root: &Path, placement: &str) {
    fs::create_dir_all(root.join(".nopal")).unwrap();
    fs::write(
        root.join(".nopal/nopal.jsonc"),
        r#"{
  "version": "nopal.project/v1",
  "project": { "name": "nopal-fixture" },
  "profile": "nopal",
  "profiles": {
    "nopal": {
      "required_modules": ["gates", "policy", "workflow", "integrations", "guidance"]
    }
  }
}
"#,
    )
    .unwrap();
    fs::write(
        root.join(".nopal/gates.jsonc"),
        r#"{
  "version": "nopal.gates/v1",
  "gates": []
}
"#,
    )
    .unwrap();
    fs::write(
        root.join(".nopal/policy.jsonc"),
        format!(
            r#"{{
  "version": "nopal.policy/v1",
  "modes": {{
    "nopal_tui": {{
      "default_decision": "ask",
      "default_placement": "{placement}",
      "rules": []
    }}
  }}
}}
"#
        ),
    )
    .unwrap();
}

/// Minimal `portable`-profile project (gates + policy only) valid for
/// `validation_report.ok`, used as the base fixture for launch tests.
fn write_portable_project(root: &Path) {
    fs::create_dir_all(root.join(".nopal")).unwrap();
    fs::write(
        root.join(".nopal/nopal.jsonc"),
        r#"{
  "version": "nopal.project/v1",
  "project": { "name": "launch-fixture" },
  "profile": "portable"
}
"#,
    )
    .unwrap();
    fs::write(
        root.join(".nopal/gates.jsonc"),
        r#"{
  "version": "nopal.gates/v1",
  "gates": []
}
"#,
    )
    .unwrap();
    fs::write(
        root.join(".nopal/policy.jsonc"),
        r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "nopal_tui": {
      "default_decision": "ask",
      "default_placement": "dedicated_repo_runtime",
      "rules": []
    }
  }
}
"#,
    )
    .unwrap();
}

fn write_bundle(root: &Path, text: &str) {
    fs::create_dir_all(root.join(".nopal")).unwrap();
    fs::write(root.join(".nopal/bundle.jsonc"), text).unwrap();
}

fn write_nopal_config(root: &Path, mode: &str, action: &str, classes: &[&str]) {
    fs::create_dir_all(root.join(".nopal")).unwrap();
    let classes = classes
        .iter()
        .map(|class| format!("\"{class}\""))
        .collect::<Vec<_>>()
        .join(", ");
    fs::write(
        root.join(".nopal/config.jsonc"),
        format!(
            r#"{{
  "version": "nopal.config/v1",
  "run_start_policy": {{
    "mode": "{mode}",
    "action": "{action}",
    "classes": [{classes}]
  }}
}}
"#
        ),
    )
    .unwrap();
}

#[test]
fn status_reports_nopal_readiness_and_missing_modules() {
    let temp = tempfile::tempdir().unwrap();
    write_project(temp.path());

    let out = nopal(&["--dir", temp.path().to_str().unwrap(), "--json", "status"]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.status/v1");
    assert_eq!(doc["project"], "nopal-fixture");
    assert_eq!(doc["profile"], "nopal");
    assert_eq!(doc["ready"], false);
    assert_eq!(
        doc["missing_modules"],
        serde_json::json!(["workflow", "integrations", "guidance"])
    );
}

#[test]
fn cli_invocation_fail_closed_output_matches_explicit_dry_run_flag() {
    // No .nopal/bundle.jsonc, so the plan fails closed either way; `nopal
    // cli` without --dry-run fails closed identically to --dry-run when the
    // plan is already failing (dispatch_launch prints and exits before exec
    // in both cases).
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());

    let no_dry_run = nopal(&["cli", "--dir", temp.path().to_str().unwrap(), "--json"]);
    let explicit = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(no_dry_run.status.code(), Some(1));
    assert_eq!(explicit.status.code(), Some(1));
    assert_eq!(stdout(&no_dry_run), stdout(&explicit));
}

#[test]
fn launch_dry_run_with_configured_bundle_would_exec_with_hermetic_argv() {
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    let resource = temp.path().join("resource.txt");
    fs::write(&resource, "x").unwrap();
    let resource_path = resource.to_str().unwrap();
    write_bundle(
        temp.path(),
        &format!(
            r#"{{
  "version": "nopal.bundle/v1",
  "extensions": [ {{ "source": "ext-a", "path": "{resource_path}" }} ],
  "skills": [ {{ "source": "skill-a", "path": "{resource_path}" }} ]
}}
"#
        ),
    );

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.launch/v1");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["would_exec"], true);
    assert_eq!(doc["ambient"], false);
    assert_eq!(doc["ambient_kinds"], serde_json::json!([]));
    let argv: Vec<String> = doc["pi_argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        argv,
        vec![
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-themes",
            "-e",
            resource_path,
            "--skill",
            resource_path,
        ]
    );
}

#[test]
fn launch_dry_run_missing_required_module_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    // `write_project` declares profile "nopal" requiring workflow,
    // integrations, and guidance, none of which are written.
    write_project(temp.path());

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["validity_ok"], false);
    assert_eq!(doc["would_exec"], false);
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "module_missing")
    );
}

#[test]
fn launch_dry_run_missing_bundle_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["bundle"]["ok"], false);
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "bundle_missing")
    );
}

#[test]
fn launch_dry_run_empty_repo_reports_would_create_and_writes_nothing() {
    // No `.nopal/` directory at all: an unconfigured repo, not a
    // misconfigured one (a bare `.nopal/` dir with both files absent fails
    // closed instead - see the existing_empty_nopal_dir test). `--dry-run`
    // is a preview only: it must report what a real launch would
    // scaffold without ever writing it.
    let temp = tempfile::tempdir().unwrap();

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["would_exec"], true);
    assert_eq!(doc["scaffold"], "would_create");
    assert_eq!(
        doc["pi_argv"],
        serde_json::json!([
            "--no-extensions",
            "--no-skills",
            "--no-prompt-templates",
            "--no-themes"
        ]),
        "an unconfigured repo with no user-level template scaffolds hermetic \
         by default, so argv carries all four --no-* flags and no \
         resource flags"
    );
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "scaffold_defaults" && d["severity"] == "info"),
        "{:?}",
        doc["diagnostics"]
    );
    assert!(
        !temp.path().join(".nopal").exists(),
        "--dry-run must never write .nopal/"
    );
}

#[test]
fn launch_dry_run_manifest_only_repo_still_fails_closed_on_bundle_missing() {
    // Exactly one of the two files present is a partially-configured repo,
    // not an unconfigured one - the pre-scaffolding fail-closed behavior stands,
    // and scaffolding never applies to it.
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["scaffold"], "none");
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "bundle_missing")
    );
    assert!(!temp.path().join(".nopal/bundle.jsonc").exists());
}

#[test]
fn launch_dry_run_bundle_only_repo_still_fails_closed_on_manifest_missing() {
    let temp = tempfile::tempdir().unwrap();
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["scaffold"], "none");
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "manifest_missing")
    );
    assert!(!temp.path().join(".nopal/nopal.jsonc").exists());
}

#[test]
fn launch_dry_run_configured_repo_reports_scaffold_none() {
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["scaffold"], "none");
}

#[test]
#[cfg(unix)]
fn cli_real_launch_on_empty_repo_scaffolds_defaults_then_execs_stub() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let stub = temp.path().join("pi-stub.sh");
    fs::write(&stub, "#!/bin/sh\nexit 9\n").unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["cli", "--dir", temp.path().to_str().unwrap()])
        .env("NOPAL_PI_BIN", &stub)
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(9), "{out:?}");
    let manifest_text = fs::read_to_string(temp.path().join(".nopal/nopal.jsonc")).unwrap();
    assert!(
        manifest_text.contains("\"nopal.project/v1\""),
        "{manifest_text}"
    );
    assert!(manifest_text.contains("\"minimal\""), "{manifest_text}");
    let bundle_text = fs::read_to_string(temp.path().join(".nopal/bundle.jsonc")).unwrap();
    assert!(bundle_text.contains("\"nopal.bundle/v1\""), "{bundle_text}");
    assert!(
        bundle_text.contains("\"inherit_ambient\": false"),
        "expected hermetic fallback with no user-level template in the \
         isolated config dir: {bundle_text}"
    );

    // The two always-on stderr notices: scaffold provenance names
    // the built-in hermetic default (no template was present), and the
    // resource-surface line reflects the hermetic bundle just written.
    // Neither is gated by --verbose.
    let launch_stderr = stderr(&out);
    assert!(
        launch_stderr.contains(".nopal/nopal.jsonc")
            && launch_stderr.contains(".nopal/bundle.jsonc")
            && launch_stderr.contains("built-in hermetic defaults"),
        "{launch_stderr}"
    );
    assert!(
        launch_stderr.contains("nopal: hermetic launch - no ambient, no pinned resources"),
        "{launch_stderr}"
    );

    // A second dry-run against the now-scaffolded repo must not re-scaffold
    // or re-report the scaffold diagnostic.
    let second = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);
    assert_eq!(second.status.code(), Some(0));
    let doc = json(&second);
    assert_eq!(doc["scaffold"], "none");
    assert!(
        !doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "scaffold_defaults"),
        "{:?}",
        doc["diagnostics"]
    );
}

// ---------------------------------------------------------------------------
// User-level default-bundle template
// ---------------------------------------------------------------------------

fn write_template(config_dir: &Path, text: &str) {
    fs::create_dir_all(config_dir).unwrap();
    fs::write(config_dir.join("bundle-default.jsonc"), text).unwrap();
}

#[test]
fn launch_dry_run_valid_template_synthesizes_its_content_and_names_the_source() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let config_dir = temp.path().join("config");
    let template_path = config_dir.join("bundle-default.jsonc");
    write_template(
        &config_dir,
        "{ \"version\": \"nopal.bundle/v1\", \"inherit_ambient\": [\"skills\"] }",
    );

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args([
            "cli",
            "--dir",
            repo.to_str().unwrap(),
            "--json",
            "--dry-run",
        ])
        .env("NOPAL_CONFIG_DIR", &config_dir)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["scaffold"], "would_create");
    assert_eq!(doc["ambient_kinds"], serde_json::json!(["skills"]));
    let note = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["code"] == "scaffold_defaults")
        .expect("scaffold_defaults diagnostic present");
    assert!(
        note["message"]
            .as_str()
            .unwrap()
            .contains(template_path.to_str().unwrap()),
        "the would-create note names the template source: {note}"
    );
    assert!(!repo.join(".nopal").exists(), "--dry-run writes nothing");
}

#[test]
#[cfg(unix)]
fn cli_real_launch_copies_a_valid_template_verbatim_and_names_it_in_the_notice() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let config_dir = temp.path().join("config");
    let template_path = config_dir.join("bundle-default.jsonc");
    let template_text = "{\n  // team-wide nopal default - do not remove this comment\n  \"version\": \"nopal.bundle/v1\",\n  \"inherit_ambient\": [\"skills\"]\n}\n";
    write_template(&config_dir, template_text);

    let stub = temp.path().join("pi-stub.sh");
    fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["cli", "--dir", repo.to_str().unwrap()])
        .env("NOPAL_PI_BIN", &stub)
        .env("NOPAL_CONFIG_DIR", &config_dir)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let bundle_text = fs::read_to_string(repo.join(".nopal/bundle.jsonc")).unwrap();
    assert_eq!(
        bundle_text, template_text,
        "the template is copied byte-for-byte, comments included"
    );
    let manifest_text = fs::read_to_string(repo.join(".nopal/nopal.jsonc")).unwrap();
    assert!(
        manifest_text.contains("\"minimal\""),
        "the manifest half stays at the minimal constant regardless of \
         the bundle template: {manifest_text}"
    );

    let launch_stderr = stderr(&out);
    assert!(
        launch_stderr.contains(&format!("from {}", template_path.display())),
        "{launch_stderr}"
    );
}

#[test]
fn launch_dry_run_invalid_template_fails_closed_and_names_the_template_path() {
    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let config_dir = temp.path().join("config");
    let template_path = config_dir.join("bundle-default.jsonc");
    // Bad version - fails `bundle::validate_bundle_text`'s version gate.
    write_template(&config_dir, "{ \"version\": \"nopal.bundle/v2\" }");

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args([
            "cli",
            "--dir",
            repo.to_str().unwrap(),
            "--json",
            "--dry-run",
        ])
        .env("NOPAL_CONFIG_DIR", &config_dir)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(1), "{out:?}");
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["would_exec"], false);
    let diagnostics = doc["diagnostics"].as_array().unwrap();
    assert!(
        diagnostics
            .iter()
            .any(|d| d["code"] == "scaffold_template_invalid"
                && d["message"]
                    .as_str()
                    .unwrap()
                    .contains(template_path.to_str().unwrap())),
        "{diagnostics:?}"
    );
    assert!(!repo.join(".nopal").exists(), "--dry-run writes nothing");
}

#[test]
#[cfg(unix)]
fn cli_real_launch_invalid_template_writes_nothing_and_exits_nonzero() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    let repo = temp.path().join("repo");
    fs::create_dir_all(&repo).unwrap();
    let config_dir = temp.path().join("config");
    let template_path = config_dir.join("bundle-default.jsonc");
    // Malformed JSONC - fails to parse at all.
    write_template(&config_dir, "{ \"version\": ");

    let stub = temp.path().join("pi-stub.sh");
    fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["cli", "--dir", repo.to_str().unwrap(), "--json"])
        .env("NOPAL_PI_BIN", &stub)
        .env("NOPAL_CONFIG_DIR", &config_dir)
        .output()
        .unwrap();

    assert_ne!(
        out.status.code(),
        Some(0),
        "must not exec the stub on an invalid template: {out:?}"
    );
    assert!(
        !repo.join(".nopal").exists(),
        "nothing may be written when the template is invalid, not even the manifest"
    );
    let doc = json(&out);
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "scaffold_template_invalid"
                && d["message"]
                    .as_str()
                    .unwrap()
                    .contains(template_path.to_str().unwrap())),
        "{doc}"
    );
}

// ---------------------------------------------------------------------------
// Git-rooted discovery
// ---------------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(dir)
        .status()
        .expect("git available in test env");
    assert!(status.success(), "git {args:?} failed in {dir:?}");
}

fn init_repo(dir: &Path) {
    git(dir, &["init", "-q"]);
}

#[test]
fn launch_dry_run_from_subdir_finds_configured_root() {
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    write_portable_project(temp.path());
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");
    let sub = temp.path().join("sub/dir");
    fs::create_dir_all(&sub).unwrap();

    let out = nopal(&["cli", "--dir", sub.to_str().unwrap(), "--json", "--dry-run"]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["scaffold"], "none");
}

#[test]
#[cfg(unix)]
fn cli_real_launch_from_subdir_of_unconfigured_git_repo_scaffolds_at_toplevel() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    let sub = temp.path().join("sub/dir");
    fs::create_dir_all(&sub).unwrap();

    let stub = temp.path().join("pi-stub.sh");
    fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["cli", "--dir", sub.to_str().unwrap()])
        .env("NOPAL_PI_BIN", &stub)
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0), "{out:?}");
    assert!(
        temp.path().join(".nopal/nopal.jsonc").is_file(),
        "scaffold must land at the git toplevel, not the --dir subfolder"
    );
    assert!(temp.path().join(".nopal/bundle.jsonc").is_file());
    assert!(!sub.join(".nopal").exists());
}

#[test]
fn nested_nopal_dir_wins_over_root_nopal_dir() {
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    write_portable_project(temp.path());
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");

    // A different, deliberately-broken config one level down: if discovery
    // stepped past it to the (valid) root config instead of anchoring on
    // the nearest `.nopal/`, this would report `ok: true`.
    let sub = temp.path().join("sub");
    fs::create_dir_all(sub.join(".nopal")).unwrap();
    fs::write(
        sub.join(".nopal/nopal.jsonc"),
        "{ \"version\": \"nopal.project/v1\", \"profile\": \"minimal\" }\n",
    )
    .unwrap();
    // No bundle.jsonc at `sub` - exactly one of the two files present.

    let out = nopal(&["cli", "--dir", sub.to_str().unwrap(), "--json", "--dry-run"]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["scaffold"], "none");
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "bundle_missing"),
        "nearest .nopal/ (sub/) should anchor the search, not the root's",
    );
}

#[test]
fn launch_dry_run_existing_empty_nopal_dir_reports_scaffold_none() {
    // A bare `.nopal/` DIRECTORY with neither file inside is not an
    // unconfigured repo: scaffold only ever creates
    // a brand-new `.nopal/`, so this must take the normal fail-closed path.
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".nopal")).unwrap();

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["scaffold"], "none");
    let codes: Vec<&str> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert!(codes.contains(&"manifest_missing"), "{codes:?}");
    assert!(codes.contains(&"bundle_missing"), "{codes:?}");
}

#[test]
#[cfg(unix)]
fn cli_real_launch_never_scaffolds_into_an_existing_nopal_dir() {
    use std::os::unix::fs::PermissionsExt;

    // `sub/.nopal` exists but is empty: discovery anchors there, and a real
    // launch must fail closed on the missing manifest/bundle instead of
    // writing defaults into a directory the user may have part-populated -
    // and must not scaffold the toplevel either.
    let temp = tempfile::tempdir().unwrap();
    init_repo(temp.path());
    let sub = temp.path().join("sub");
    fs::create_dir_all(sub.join(".nopal")).unwrap();

    let stub = temp.path().join("pi-stub.sh");
    fs::write(&stub, "#!/bin/sh\nexit 7\n").unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["cli", "--dir", sub.to_str().unwrap(), "--json"])
        .env("NOPAL_PI_BIN", &stub)
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .output()
        .unwrap();

    assert_eq!(
        out.status.code(),
        Some(1),
        "must fail closed, not exec the stub (exit 7): {out:?}"
    );
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["scaffold"], "none");
    let codes: Vec<&str> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert!(codes.contains(&"manifest_missing"), "{codes:?}");
    assert!(codes.contains(&"bundle_missing"), "{codes:?}");
    assert!(
        !sub.join(".nopal/nopal.jsonc").exists() && !sub.join(".nopal/bundle.jsonc").exists(),
        "nothing may be written into the existing .nopal/"
    );
    assert!(
        !temp.path().join(".nopal").exists(),
        "the toplevel must not get scaffolded either"
    );
}

#[test]
fn non_git_dir_anchors_at_start_same_as_before_oli_45() {
    let temp = tempfile::tempdir().unwrap();
    // Deliberately no `git init`: outside any repo, discovery must not walk
    // anywhere - `--dir` itself is still the anchor, matching the behavior
    // before git-rooted discovery was introduced.
    let sub = temp.path().join("sub/dir");
    fs::create_dir_all(&sub).unwrap();

    let out = nopal(&["cli", "--dir", sub.to_str().unwrap(), "--json", "--dry-run"]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["scaffold"], "would_create");
    assert!(!sub.join(".nopal").exists());
    assert!(!temp.path().join(".nopal").exists());
}

#[test]
fn launch_dry_run_stale_process_artifact_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");
    fs::write(
        temp.path().join(".nopal/process-artifact.json"),
        "{ \"kind\": \"nopal.process_artifact/v1\", \"stale\": true }\n",
    )
    .unwrap();

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["would_exec"], false);
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "process_artifact_drift"
                || d["code"] == "process_artifact_parse_error")
    );
}

#[test]
fn launch_dry_run_missing_process_artifact_launches_with_note() {
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    assert!(
        doc["process_artifact_note"]
            .as_str()
            .unwrap()
            .contains("not found")
    );
}

#[test]
fn launch_dry_run_with_ambient_omits_no_flags() {
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
        "--with-ambient",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ambient"], true);
    assert_eq!(
        doc["ambient_kinds"],
        serde_json::json!(["extensions", "skills", "prompt_templates", "themes"])
    );
    assert_eq!(doc["pi_argv"].as_array().unwrap().len(), 0);
}

#[test]
fn launch_dry_run_per_kind_inherit_ambient_omits_only_named_no_flags() {
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    let resource = temp.path().join("resource.txt");
    fs::write(&resource, "x").unwrap();
    let resource_path = resource.to_str().unwrap();
    write_bundle(
        temp.path(),
        &format!(
            r#"{{
  "version": "nopal.bundle/v1",
  "inherit_ambient": ["skills"],
  "extensions": [ {{ "source": "ext-a", "path": "{resource_path}" }} ],
  "skills": [ {{ "source": "skill-a", "path": "{resource_path}" }} ]
}}
"#
        ),
    );

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["ambient"], false, "not all four kinds are inherited");
    assert_eq!(doc["ambient_kinds"], serde_json::json!(["skills"]));
    let argv: Vec<String> = doc["pi_argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        argv,
        vec![
            "--no-extensions",
            "--no-prompt-templates",
            "--no-themes",
            "-e",
            resource_path,
            "--skill",
            resource_path,
        ],
        "no-skills is dropped since skills is inherited from ambient, \
         but the pinned skill resource still loads additively"
    );
}

#[test]
fn launch_dry_run_with_ambient_widens_a_partial_bundle_inherit_to_all_four() {
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    write_bundle(
        temp.path(),
        "{ \"version\": \"nopal.bundle/v1\", \"inherit_ambient\": [\"skills\"] }",
    );

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
        "--with-ambient",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(
        doc["ambient"], true,
        "--with-ambient widens the bundle's partial declaration to all four, never narrows it"
    );
    assert_eq!(
        doc["ambient_kinds"],
        serde_json::json!(["extensions", "skills", "prompt_templates", "themes"])
    );
    assert_eq!(doc["pi_argv"].as_array().unwrap().len(), 0);
}

#[test]
fn launch_dry_run_toon_envelope_surfaces_ambient_kinds() {
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    write_bundle(
        temp.path(),
        "{ \"version\": \"nopal.bundle/v1\", \"inherit_ambient\": [\"themes\"] }",
    );

    let out = nopal(&["cli", "--dir", temp.path().to_str().unwrap(), "--dry-run"]);

    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("ambient_kinds"), "{text}");
    assert!(text.contains("themes"), "{text}");
}

#[test]
fn relative_dir_flag_resolves_bundle_paths_to_absolute_argv() {
    // Regression: a relative --dir combined with exec_pi's .current_dir(dir)
    // used to double the project dir into the resolved path once Pi's cwd
    // changed (bundle.rs resolved relative paths against the possibly
    // relative --dir string instead of an absolutized root).
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("proj");
    write_portable_project(&project);
    let resource = project.join("resource.txt");
    fs::write(&resource, "x").unwrap();
    write_bundle(
        &project,
        r#"{
  "version": "nopal.bundle/v1",
  "extensions": [ { "source": "ext-a", "path": "resource.txt" } ]
}
"#,
    );

    let out = nopal_with_cwd(
        temp.path(),
        &["cli", "--dir", "proj", "--json", "--dry-run"],
    );

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true, "{doc}");
    let resolved = doc["bundle"]["resources"][0]["resolved_path"]
        .as_str()
        .unwrap();
    assert!(
        Path::new(resolved).is_absolute(),
        "resolved_path should be absolute: {resolved}"
    );
    assert!(
        Path::new(resolved).exists(),
        "resolved_path should point at the real file: {resolved}"
    );
    assert!(
        !resolved.contains("proj/proj"),
        "resolved path doubled the project dir: {resolved}"
    );
}

#[test]
fn optional_module_schema_error_warns_but_does_not_block_launch() {
    // Regression: validation_report/status both derive from the same
    // Validation::ok(), which flags schema errors in ANY present module
    // regardless of whether the active profile requires it. That made an
    // optional module's schema bug hard-block launch, contradicting design
    // D4 ("ready == false from optional module gaps launches with a
    // warning").
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");
    // workflow is optional under the portable profile (only gates+policy are
    // required); an unknown lifecycle event makes it schema-invalid but
    // still present-and-parseable.
    fs::write(
        temp.path().join(".nopal/workflow.jsonc"),
        r#"{
  "version": "nopal.workflow/v1",
  "lifecycle": {
    "events": { "made_up_event": { "actions": [] } }
  }
}
"#,
    )
    .unwrap();

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true, "{doc}");
    assert_eq!(doc["validity_ok"], true, "{doc}");
    assert_eq!(doc["ready"], false, "{doc}");
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "workflow_event_unknown"),
        "{doc}"
    );
}

#[test]
fn required_module_schema_error_blocks_launch() {
    // Mirror of optional_module_schema_error_warns_but_does_not_block_launch:
    // policy is required under the portable profile, so a schema error in
    // it (unlike an optional module's) must still hard-block launch.
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");
    fs::write(
        temp.path().join(".nopal/policy.jsonc"),
        r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "nopal_tui": {
      "default_decision": "maybe",
      "default_placement": "dedicated_repo_runtime",
      "rules": []
    }
  }
}
"#,
    )
    .unwrap();

    let out = nopal(&[
        "cli",
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "--dry-run",
    ]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false, "{doc}");
    assert_eq!(doc["validity_ok"], false, "{doc}");
    assert_eq!(doc["would_exec"], false, "{doc}");
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "policy_decision_invalid"),
        "{doc}"
    );
}

#[test]
fn top_level_dry_run_flag_is_rejected() {
    // --dry-run/--with-ambient/--verbose no longer exist on the top-level
    // Cli struct: they moved to `nopal cli`. This pins the
    // deliberate breaking change as a clap usage error.
    let out = nopal(&["--dry-run"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
#[cfg(unix)]
fn cli_execs_the_stub_binary_with_expected_argv_and_cwd() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");

    let stub = temp.path().join("pi-stub.sh");
    fs::write(
        &stub,
        "#!/bin/sh\ntouch pi-was-here\necho \"argv=$*\"\necho \"skip_version_check=$PI_SKIP_VERSION_CHECK\"\nexit 7\n",
    )
    .unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["cli", "--dir", temp.path().to_str().unwrap()])
        .env("NOPAL_PI_BIN", &stub)
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(7), "{:?}", out);
    assert!(
        temp.path().join("pi-was-here").exists(),
        "exec_pi should have chdir'd into --dir before exec-ing the stub"
    );
    let stdout = stdout(&out);
    assert!(
        stdout.contains("argv=--no-extensions --no-skills --no-prompt-templates --no-themes"),
        "{stdout}"
    );
    assert!(
        stdout.contains("skip_version_check=1"),
        "exec_pi should skip Pi's own update-check banner: {stdout}"
    );
    let stderr = stderr(&out);
    assert!(
        !stderr.contains("nopal.launch/v1"),
        "the verbose summary line should be opt-in via --verbose, not printed by default: {stderr}"
    );
    assert!(
        stderr.contains("nopal: hermetic launch - no ambient, no pinned resources"),
        "the resource-surface notice is always-on, not gated by --verbose: {stderr}"
    );
}

#[test]
#[cfg(unix)]
fn verbose_flag_prints_the_stderr_summary_before_exec() {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    write_bundle(temp.path(), "{ \"version\": \"nopal.bundle/v1\" }");

    let stub = temp.path().join("pi-stub.sh");
    fs::write(&stub, "#!/bin/sh\nexit 0\n").unwrap();
    let mut perms = fs::metadata(&stub).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(&stub, perms).unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["cli", "--dir", temp.path().to_str().unwrap(), "--verbose"])
        .env("NOPAL_PI_BIN", &stub)
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(0), "{:?}", out);
    let stderr = stderr(&out);
    assert!(
        stderr.contains("nopal.launch/v1: would_exec=true"),
        "{stderr}"
    );
    assert!(
        stderr.contains("scaffold=none"),
        "the summary line reports scaffold status: {stderr}"
    );
}

#[test]
fn bare_nopal_without_tty_fails_closed_pointing_at_nopal_cli() {
    // No subcommand at all, piped stdio (the default for
    // std::process::Command in tests, i.e. non-tty): bare `nopal` must
    // refuse to start a TUI and point the operator at `nopal cli` instead.
    let temp = tempfile::tempdir().unwrap();
    write_project(temp.path());

    let state = temp.path().join("state");
    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", temp.path().to_str().unwrap()])
        .env("NOPAL_STATE_DIR", &state)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    assert!(stderr(&out).contains("nopal cli"), "{}", stderr(&out));
    assert!(
        !state.join("plots").exists(),
        "the terminal guard must run before Plot bootstrap"
    );
}

#[cfg(unix)]
struct NativeFieldE2eFixture {
    temp: tempfile::TempDir,
    native: std::path::PathBuf,
}

#[cfg(unix)]
struct NativeE2eChild {
    child: Option<Child>,
}

#[cfg(unix)]
impl NativeE2eChild {
    fn spawn(command: &mut Command) -> Result<Self, String> {
        command
            .spawn()
            .map(|child| Self { child: Some(child) })
            .map_err(|error| format!("spawn fake native Field process: {error}"))
    }

    fn wait_with_output_bounded(mut self, timeout: Duration) -> Result<Output, String> {
        let started = std::time::Instant::now();
        loop {
            let child = self.child.as_mut().expect("child remains owned");
            if child
                .try_wait()
                .map_err(|error| format!("poll fake native Field process: {error}"))?
                .is_some()
            {
                return self
                    .child
                    .take()
                    .expect("completed child remains owned")
                    .wait_with_output()
                    .map_err(|error| {
                        format!("collect completed fake native Field process: {error}")
                    });
            }
            if started.elapsed() >= timeout {
                let mut child = self.child.take().expect("timed-out child remains owned");
                let _ = child.kill();
                let output = child.wait_with_output().map_err(|error| {
                    format!("collect timed-out fake native Field process: {error}")
                })?;
                return Err(format!(
                    "fake native Field process exceeded {timeout:?}; stdout:\n{}\nstderr:\n{}",
                    stdout(&output),
                    stderr(&output)
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    fn has_exited(&mut self) -> Result<bool, String> {
        self.child
            .as_mut()
            .expect("child remains owned")
            .try_wait()
            .map(|status| status.is_some())
            .map_err(|error| format!("poll fake native Field process: {error}"))
    }
}

#[cfg(unix)]
impl Drop for NativeE2eChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

#[cfg(unix)]
impl NativeFieldE2eFixture {
    fn new() -> Self {
        use nopal_native_lifecycle::application::ScopedOwnedResourceRecovery;
        use nopal_native_lifecycle::preferences::{
            RestorePreferenceStore, RestorePreferenceUpdate,
        };
        use nopal_native_lifecycle::reconcile::ExactSessionSelection;
        use nopal_native_lifecycle::recovery::{
            DurableIdentity, DurableRecoveryEntry, DurableRecoveryRecipe, FilesystemRecoveryRecipe,
            RecoveryJournalStore,
        };
        use nopal_native_lifecycle::resources::ResourceOwnership;
        use nopal_native_lifecycle::state_root::{
            CanonicalStateRoot, NativeInstanceScope, ReleaseChannel,
        };

        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let native = temp.path().join("nopal-field-native");
        let test_binary = std::env::current_exe().unwrap();
        let script = format!(
            "#!/bin/sh\nstate_dir=\"\"\nwhile [ \"$#\" -gt 0 ]; do\n  if [ \"$1\" = \"--state-dir\" ]; then\n    state_dir=\"$2\"\n    shift 2\n  else\n    echo \"fake native host: unexpected argument $1\" >&2\n    exit 64\n  fi\ndone\nif [ -z \"$state_dir\" ]; then\n  echo \"fake native host: --state-dir is required\" >&2\n  exit 64\nfi\nexec env NOPAL_FAKE_NATIVE_STATE_DIR=\"$state_dir\" \"{}\" --exact fake_native_field_process --ignored --nocapture\n",
            test_binary.display()
        );
        fs::write(&native, script).unwrap();
        fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();

        let fixture = Self { temp, native };
        let scope = NativeInstanceScope::new(
            CanonicalStateRoot::create(fixture.state_dir()).unwrap(),
            ReleaseChannel::Stable,
        );
        let preference = RestorePreferenceStore::new(scope.state_paths().restore_preference());
        assert!(matches!(
            preference
                .write(&RestorePreferenceUpdate::select(
                    ExactSessionSelection::new("plot-b", "session-b2"),
                ))
                .unwrap(),
            nopal_native_lifecycle::preferences::RestorePreferenceWriteOutcome::Written
        ));

        fs::write(fixture.stale_resource(), "stale-owned-v1").unwrap();
        let recovery_entry = DurableRecoveryEntry::new(
            "stale-terminal-transport",
            "stale fake Terminal transport",
            ResourceOwnership::ApplicationOwned,
            DurableRecoveryRecipe::Filesystem(
                FilesystemRecoveryRecipe::new(
                    fixture.stale_resource(),
                    DurableIdentity::new("test.file.contents", "stale-owned-v1").unwrap(),
                )
                .unwrap(),
            ),
        )
        .unwrap();
        let recovery_store = RecoveryJournalStore::new(ScopedOwnedResourceRecovery::<
            NativeE2eRecoveryAdapter,
        >::journal_path(&scope));
        assert!(matches!(
            recovery_store.register(recovery_entry).unwrap(),
            nopal_native_lifecycle::recovery::RecoveryJournalUpdateOutcome::Written {
                entry_count: 1
            }
        ));

        fixture
    }

    fn state_dir(&self) -> std::path::PathBuf {
        self.temp.path().join("state")
    }

    fn events(&self) -> std::path::PathBuf {
        self.temp.path().join("events.log")
    }

    fn start_gate(&self) -> std::path::PathBuf {
        self.temp.path().join("start")
    }

    fn release_gate(&self) -> std::path::PathBuf {
        self.temp.path().join("release")
    }

    fn cleanup_gate(&self) -> std::path::PathBuf {
        self.temp.path().join("cleanup")
    }

    fn stage_gate(&self) -> std::path::PathBuf {
        self.temp.path().join("stage")
    }

    fn stale_resource(&self) -> std::path::PathBuf {
        self.temp.path().join("stale-terminal.sock")
    }

    fn live_feed_resource(&self) -> std::path::PathBuf {
        self.temp.path().join("live-feed.binding")
    }

    fn live_terminal_resource(&self) -> std::path::PathBuf {
        self.temp.path().join("live-terminal.binding")
    }

    fn borrowed_session_marker(&self) -> std::path::PathBuf {
        self.temp.path().join("core-session-b2.borrowed")
    }

    fn command(&self) -> Command {
        let mut command = nopal_command();
        command
            .args([
                "field",
                "native",
                "--state-dir",
                self.state_dir().to_str().unwrap(),
            ])
            .env("NOPAL_FIELD_NATIVE_BIN", &self.native)
            .env("NOPAL_FAKE_NATIVE_EVENTS", self.events())
            .env("NOPAL_FAKE_NATIVE_START_GATE", self.start_gate())
            .env("NOPAL_FAKE_NATIVE_RELEASE_GATE", self.release_gate())
            .env("NOPAL_FAKE_NATIVE_CLEANUP_GATE", self.cleanup_gate())
            .env("NOPAL_FAKE_NATIVE_STAGE_GATE", self.stage_gate())
            .env("NOPAL_FAKE_NATIVE_MODE", "race")
            .env("NOPAL_FAKE_NATIVE_REQUEST_TERMINAL", "1")
            .env(
                "NOPAL_FAKE_SESSION_HOST_PID",
                std::process::id().to_string(),
            )
            .env("NOPAL_FAKE_NATIVE_EXPECTED_RECOVERY_COUNT", "1")
            .env(
                "NOPAL_FAKE_NATIVE_EXPECTED_RESTORE",
                "exact:plot-b/session-b2",
            )
            .env("NOPAL_FAKE_NATIVE_FEED_RESOURCE", self.live_feed_resource())
            .env(
                "NOPAL_FAKE_NATIVE_TERMINAL_RESOURCE",
                self.live_terminal_resource(),
            )
            .env("NOPAL_CONFIG_DIR", isolated_config_dir())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        command
    }

    fn single_command(&self, expected_recovery_count: usize) -> Command {
        let mut command = self.command();
        command.env("NOPAL_FAKE_NATIVE_MODE", "single").env(
            "NOPAL_FAKE_NATIVE_EXPECTED_RECOVERY_COUNT",
            expected_recovery_count.to_string(),
        );
        command
    }

    fn evidence(&self, first: &Output, second: &Output) -> String {
        format!(
            "events:\n{}\nfirst status: {:?}\nfirst stdout:\n{}\nfirst stderr:\n{}\nsecond status: {:?}\nsecond stdout:\n{}\nsecond stderr:\n{}",
            fs::read_to_string(self.events())
                .unwrap_or_else(|error| format!("<unreadable: {error}>")),
            first.status.code(),
            stdout(first),
            stderr(first),
            second.status.code(),
            stdout(second),
            stderr(second),
        )
    }

    fn wait_for_event(&self, prefix: &str, timeout: Duration) -> Result<(), String> {
        self.wait_for_event_count(prefix, 1, timeout)
    }

    fn wait_for_event_count(
        &self,
        prefix: &str,
        expected_count: usize,
        timeout: Duration,
    ) -> Result<(), String> {
        let started = std::time::Instant::now();
        loop {
            let events = fs::read_to_string(self.events()).unwrap_or_default();
            if events
                .lines()
                .filter(|line| line.starts_with(prefix))
                .count()
                >= expected_count
            {
                return Ok(());
            }
            if started.elapsed() >= timeout {
                return Err(format!(
                    "timed out after {timeout:?} waiting for {expected_count} event(s) with prefix {prefix:?}; events:\n{events}"
                ));
            }
            thread::sleep(Duration::from_millis(5));
        }
    }
}

#[cfg(unix)]
fn remove_native_e2e_gate(path: &Path) {
    match fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => panic!("remove fake native gate {}: {error}", path.display()),
    }
}

#[cfg(unix)]
fn complete_native_e2e_single_launch(
    fixture: &NativeFieldE2eFixture,
    mut command: Command,
    launch_ordinal: usize,
) -> Output {
    remove_native_e2e_gate(&fixture.release_gate());
    fs::write(fixture.start_gate(), "go").unwrap();
    fs::write(fixture.cleanup_gate(), "continue").unwrap();
    let child = NativeE2eChild::spawn(&mut command).unwrap();
    fixture
        .wait_for_event_count("primary_ready", launch_ordinal, Duration::from_secs(8))
        .unwrap();
    fs::write(fixture.release_gate(), "release").unwrap();
    child
        .wait_with_output_bounded(Duration::from_secs(8))
        .unwrap()
}

#[cfg(unix)]
fn force_kill_latest_native_process(fixture: &NativeFieldE2eFixture) {
    let native_pid = fs::read_to_string(fixture.events())
        .unwrap()
        .lines()
        .rev()
        .find(|line| line.starts_with("native_process_started "))
        .and_then(|line| {
            line.split_whitespace()
                .find_map(|part| part.strip_prefix("pid="))
        })
        .expect("native process PID was recorded")
        .to_owned();
    let kill = Command::new("kill")
        .args(["-KILL", native_pid.as_str()])
        .output()
        .unwrap();
    assert!(
        kill.status.success(),
        "force-kill native process {native_pid}: {}",
        stderr(&kill)
    );
}

#[cfg(unix)]
fn append_native_e2e_event(event: &str) -> Result<(), String> {
    let path = std::env::var("NOPAL_FAKE_NATIVE_EVENTS")
        .map_err(|error| format!("read NOPAL_FAKE_NATIVE_EVENTS: {error}"))?;
    let mut events = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|error| format!("open event log {path}: {error}"))?;
    let record = format!("{event}\n");
    events
        .write_all(record.as_bytes())
        .map_err(|error| format!("append event log {path}: {error}"))
}

#[cfg(unix)]
fn wait_for_native_e2e_gate(variable: &str, timeout: Duration) -> Result<(), String> {
    let path = std::env::var(variable).map_err(|error| format!("read {variable}: {error}"))?;
    let started = std::time::Instant::now();
    while !Path::new(&path).exists() {
        if started.elapsed() >= timeout {
            return Err(format!(
                "timed out after {timeout:?} waiting for {variable} at {path}"
            ));
        }
        thread::sleep(Duration::from_millis(5));
    }
    Ok(())
}

#[cfg(unix)]
struct NativeE2ePlatform {
    coordinator: nopal_native_lifecycle::platform::unix::UnixInstanceCoordinator,
    scope_fingerprint: String,
}

#[cfg(unix)]
impl nopal_native_lifecycle::instance::InstancePlatform for NativeE2ePlatform {
    type Primary = nopal_native_lifecycle::platform::unix::UnixPrimaryLease;
    type Secondary = nopal_native_lifecycle::transport::UnixActivationForwarder;

    fn acquire(
        &self,
        timeout: Duration,
    ) -> std::io::Result<
        nopal_native_lifecycle::instance::InstanceAcquisition<Self::Primary, Self::Secondary>,
    > {
        use nopal_native_lifecycle::instance::InstanceAcquisition;

        match self.coordinator.acquire(timeout)? {
            InstanceAcquisition::Primary(lease) => Ok(InstanceAcquisition::Primary(lease)),
            InstanceAcquisition::Secondary(stream) => {
                let forwarder = nopal_native_lifecycle::transport::UnixActivationForwarder::new(
                    stream,
                    &self.scope_fingerprint,
                    timeout,
                )
                .map_err(std::io::Error::other)?;
                Ok(InstanceAcquisition::Secondary(forwarder))
            }
        }
    }
}

#[cfg(unix)]
struct NativeE2eRecoveryAdapter;

#[cfg(unix)]
impl nopal_native_lifecycle::recovery::ExactRecoveryAdapter for NativeE2eRecoveryAdapter {
    fn recover_filesystem_exact(
        &mut self,
        recipe: &nopal_native_lifecycle::recovery::FilesystemRecoveryRecipe,
        deadline: nopal_native_lifecycle::recovery::RecoveryDeadline,
    ) -> Result<
        nopal_native_lifecycle::recovery::RecoveryDisposition,
        nopal_native_lifecycle::recovery::RecoveryAdapterError,
    > {
        use nopal_native_lifecycle::recovery::{RecoveryAdapterError, RecoveryDisposition};

        if deadline.is_expired() {
            return Err(RecoveryAdapterError::new(
                "fake filesystem recovery started after the shared deadline",
            ));
        }

        let observed = match fs::read_to_string(recipe.path()) {
            Ok(observed) => observed,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RecoveryDisposition::AlreadyAbsent);
            }
            Err(error) => return Err(RecoveryAdapterError::new(error.to_string())),
        };
        if recipe.identity().namespace() != "test.file.contents"
            || recipe.identity().value() != observed
        {
            return Ok(RecoveryDisposition::IdentityMismatch {
                observed_identity: Some(observed),
            });
        }
        fs::remove_file(recipe.path())
            .map_err(|error| RecoveryAdapterError::new(error.to_string()))?;
        append_native_e2e_event("recovery_removed stale-terminal-transport")
            .map_err(RecoveryAdapterError::new)?;
        Ok(RecoveryDisposition::Recovered)
    }

    fn recover_process_exact(
        &mut self,
        _recipe: &nopal_native_lifecycle::recovery::VerifiedProcessRecoveryRecipe,
        _deadline: nopal_native_lifecycle::recovery::RecoveryDeadline,
    ) -> Result<
        nopal_native_lifecycle::recovery::RecoveryDisposition,
        nopal_native_lifecycle::recovery::RecoveryAdapterError,
    > {
        Err(nopal_native_lifecycle::recovery::RecoveryAdapterError::new(
            "fake native host did not register a process recovery recipe",
        ))
    }
}

#[cfg(unix)]
struct NativeE2eCoreSource;

#[cfg(unix)]
impl nopal_native_lifecycle::application::CoreFieldSnapshotSource for NativeE2eCoreSource {
    fn load_field_snapshot(
        &self,
    ) -> Result<
        nopal_feed_client::field::FieldSnapshot,
        nopal_native_lifecycle::supervisor::NativeApplicationUnavailable,
    > {
        use nopal_native_lifecycle::supervisor::NativeApplicationUnavailable;

        let variant =
            std::env::var("NOPAL_FAKE_NATIVE_CORE_VARIANT").unwrap_or_else(|_| "full".to_owned());
        append_native_e2e_event(&format!("core_snapshot_loaded variant={variant}"))
            .map_err(NativeApplicationUnavailable::new)?;
        let mut field = serde_json::json!({
            "kind": "nopal.field/v1",
            "plots": [
                {
                    "kind": "nopal.plot/v1",
                    "plot_id": "plot-a",
                    "sessions": [{"session_id": "session-a"}]
                },
                {
                    "kind": "nopal.plot/v1",
                    "plot_id": "plot-b",
                    "selected_session_id": "session-b1",
                    "sessions": [
                        {"session_id": "session-b1"},
                        {"session_id": "session-b2"}
                    ]
                }
            ],
            "entries": []
        });
        match variant.as_str() {
            "full" => {}
            "missing-session-b2" => {
                field["plots"][1]["sessions"] = serde_json::json!([{"session_id": "session-b1"}]);
            }
            "missing-plot-b" => {
                field["plots"] = serde_json::json!([{
                    "kind": "nopal.plot/v1",
                    "plot_id": "plot-a",
                    "sessions": [{"session_id": "session-a"}]
                }]);
            }
            other => {
                return Err(NativeApplicationUnavailable::new(format!(
                    "unknown fake Core variant {other:?}"
                )));
            }
        }
        serde_json::from_value(field)
            .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))
    }
}

#[cfg(unix)]
fn native_e2e_restore_label(
    restore: &nopal_native_lifecycle::reconcile::RestoreResolution,
) -> String {
    use nopal_native_lifecycle::reconcile::{
        RestoreFallbackReason, RestoreResolution, RestoreSelection,
    };

    match restore {
        RestoreResolution::Exact(selection) => {
            format!("exact:{}/{}", selection.plot_id(), selection.session_id())
        }
        RestoreResolution::Fallback { selection, reason } => {
            let target = match selection {
                RestoreSelection::Session(selection) => {
                    format!("{}/{}", selection.plot_id(), selection.session_id())
                }
                RestoreSelection::PlotOnly { plot_id } => format!("{plot_id}/-"),
            };
            let reason = match reason {
                RestoreFallbackReason::PlotMissing { .. } => "plot-missing",
                RestoreFallbackReason::SessionMissing { .. } => "session-missing",
                RestoreFallbackReason::NoPreviousSelection => "no-previous-selection",
                RestoreFallbackReason::SessionBelongsToAnotherPlot { .. } => "session-moved",
                RestoreFallbackReason::DuplicatePlotIdentity { .. } => "duplicate-plot",
                RestoreFallbackReason::DuplicateSessionIdentity { .. } => "duplicate-session",
                RestoreFallbackReason::NoPlotsAvailable => "no-plots",
            };
            format!("fallback:{target}:{reason}")
        }
        RestoreResolution::Unavailable { reason } => format!("unavailable:{reason:?}"),
    }
}

#[cfg(unix)]
struct NativeE2eHostFactory;

#[cfg(unix)]
struct NativeE2eOutputBinding {
    identity: nopal_native_lifecycle::session_bindings::StructuredOutputBindingIdentity,
}

#[cfg(unix)]
impl nopal_native_lifecycle::session_bindings::StructuredOutputBinding for NativeE2eOutputBinding {
    fn identity(
        &self,
    ) -> &nopal_native_lifecycle::session_bindings::StructuredOutputBindingIdentity {
        &self.identity
    }

    fn close(&mut self) -> Result<(), nopal_native_lifecycle::session_bindings::BindingCloseError> {
        append_native_e2e_event("typed_output_closed")
            .map_err(nopal_native_lifecycle::session_bindings::BindingCloseError::new)
    }
}

#[cfg(unix)]
struct NativeE2eTerminalBinding {
    identity: nopal_native_lifecycle::session_bindings::TerminalBindingIdentity,
}

#[cfg(unix)]
impl nopal_native_lifecycle::session_bindings::TerminalBinding for NativeE2eTerminalBinding {
    fn identity(&self) -> &nopal_native_lifecycle::session_bindings::TerminalBindingIdentity {
        &self.identity
    }

    fn close(&mut self) -> Result<(), nopal_native_lifecycle::session_bindings::BindingCloseError> {
        append_native_e2e_event("typed_terminal_closed")
            .map_err(nopal_native_lifecycle::session_bindings::BindingCloseError::new)
    }
}

#[cfg(unix)]
struct NativeE2eOutputFactory;

#[cfg(unix)]
impl nopal_native_lifecycle::session_bindings::StructuredOutputBindingFactory
    for NativeE2eOutputFactory
{
    type Binding = NativeE2eOutputBinding;

    fn bind(
        &mut self,
        context: nopal_native_lifecycle::session_bindings::SessionBindingContext<'_>,
    ) -> Result<Self::Binding, nopal_native_lifecycle::session_bindings::BindingFactoryError> {
        use nopal_native_lifecycle::session_bindings::{
            StructuredOutputBindingIdentity, StructuredRuntimeIdentity,
        };

        let runtime =
            StructuredRuntimeIdentity::new("fake-structured-runtime").map_err(|error| {
                nopal_native_lifecycle::session_bindings::BindingFactoryError::new(
                    error.to_string(),
                )
            })?;
        append_native_e2e_event(&format!(
            "typed_output_bound plot={} session={} runtime={}",
            context.session().plot_id(),
            context.session().session_id(),
            runtime.as_str()
        ))
        .map_err(nopal_native_lifecycle::session_bindings::BindingFactoryError::new)?;
        Ok(NativeE2eOutputBinding {
            identity: StructuredOutputBindingIdentity::new(context.session().clone(), runtime),
        })
    }
}

#[cfg(unix)]
struct NativeE2eTerminalFactory;

#[cfg(unix)]
impl nopal_native_lifecycle::session_bindings::TerminalBindingFactory for NativeE2eTerminalFactory {
    type Binding = NativeE2eTerminalBinding;

    fn bind(
        &mut self,
        context: nopal_native_lifecycle::session_bindings::SessionBindingContext<'_>,
    ) -> Result<Self::Binding, nopal_native_lifecycle::session_bindings::BindingFactoryError> {
        use nopal_native_lifecycle::session_bindings::{
            TerminalBindingIdentity, TerminalPaneIdentity, TerminalProcessIdentity,
        };

        let process = TerminalProcessIdentity::new(context.session_host_process().get()).map_err(
            |error| {
                nopal_native_lifecycle::session_bindings::BindingFactoryError::new(
                    error.to_string(),
                )
            },
        )?;
        let pane = TerminalPaneIdentity::new("fake-terminal-pane").map_err(|error| {
            nopal_native_lifecycle::session_bindings::BindingFactoryError::new(error.to_string())
        })?;
        append_native_e2e_event(&format!(
            "typed_terminal_bound plot={} session={} process={} pane={}",
            context.session().plot_id(),
            context.session().session_id(),
            process.get(),
            pane.as_str()
        ))
        .map_err(nopal_native_lifecycle::session_bindings::BindingFactoryError::new)?;
        Ok(NativeE2eTerminalBinding {
            identity: TerminalBindingIdentity::new(context.session().clone(), process, pane),
        })
    }
}

#[cfg(unix)]
type NativeE2eSessionBindings = nopal_native_lifecycle::session_bindings::SessionBindingController<
    NativeE2eOutputFactory,
    NativeE2eTerminalFactory,
>;

#[cfg(unix)]
struct NativeE2eOwnedResource {
    label: &'static str,
    path: std::path::PathBuf,
    pause_during_close: bool,
}

#[cfg(unix)]
struct NativeE2eStagedResource {
    resource: NativeE2eOwnedResource,
    recovery_entry: nopal_native_lifecycle::recovery::DurableRecoveryEntry,
    contents: &'static str,
}

#[cfg(unix)]
impl nopal_native_lifecycle::resources::StagedRecoverableResource for NativeE2eStagedResource {
    type Resource = NativeE2eOwnedResource;
    type ActivationError = String;

    fn recovery_entry(&self) -> &nopal_native_lifecycle::recovery::DurableRecoveryEntry {
        &self.recovery_entry
    }

    fn activate(
        self,
        deadline: nopal_native_lifecycle::resources::ShutdownDeadline,
    ) -> Result<Self::Resource, Self::ActivationError> {
        if deadline.is_expired() {
            return Err(format!(
                "fake {} activation started after the shared deadline",
                self.resource.label
            ));
        }
        let pause = std::env::var("NOPAL_FAKE_NATIVE_STAGE_PAUSE").ok();
        let before_create = format!("before-create:{}", self.resource.label);
        if pause.as_deref() == Some(before_create.as_str()) {
            append_native_e2e_event(&format!(
                "stage_paused_before_create {}",
                self.resource.label
            ))?;
            wait_for_native_e2e_gate(
                "NOPAL_FAKE_NATIVE_STAGE_GATE",
                deadline.remaining().min(Duration::from_secs(5)),
            )?;
        }
        fs::write(&self.resource.path, self.contents).map_err(|error| {
            format!(
                "activate fake {} resource {}: {error}",
                self.resource.label,
                self.resource.path.display()
            )
        })?;
        append_native_e2e_event(&format!("resource_created {}", self.resource.label))?;
        let after_create = format!("after-create:{}", self.resource.label);
        if pause.as_deref() == Some(after_create.as_str()) {
            append_native_e2e_event(&format!(
                "stage_paused_after_create {}",
                self.resource.label
            ))?;
            wait_for_native_e2e_gate(
                "NOPAL_FAKE_NATIVE_STAGE_GATE",
                deadline.remaining().min(Duration::from_secs(5)),
            )?;
        }
        append_native_e2e_event(&format!("resource_activated {}", self.resource.label))?;
        Ok(self.resource)
    }
}

#[cfg(unix)]
impl nopal_native_lifecycle::resources::ApplicationResource for NativeE2eOwnedResource {
    fn close(
        &mut self,
        deadline: nopal_native_lifecycle::resources::ShutdownDeadline,
    ) -> Result<(), nopal_native_lifecycle::resources::ResourceCloseError> {
        use nopal_native_lifecycle::resources::ResourceCloseError;

        if deadline.is_expired() {
            return Err(ResourceCloseError::deadline_exceeded(format!(
                "{} cleanup started after the native shutdown deadline",
                self.label
            )));
        }
        append_native_e2e_event(&format!("resource_close_started {}", self.label))
            .map_err(ResourceCloseError::new)?;
        if self.pause_during_close {
            wait_for_native_e2e_gate(
                "NOPAL_FAKE_NATIVE_CLEANUP_GATE",
                deadline.remaining().min(Duration::from_secs(5)),
            )
            .map_err(ResourceCloseError::new)?;
        }
        fs::remove_file(&self.path).map_err(|error| {
            ResourceCloseError::new(format!(
                "remove fake {} resource {}: {error}",
                self.label,
                self.path.display()
            ))
        })?;
        append_native_e2e_event(&format!("resource_closed {}", self.label))
            .map_err(ResourceCloseError::new)
    }
}

#[cfg(unix)]
fn native_e2e_owned_resource(
    label: &'static str,
    variable: &str,
    contents: &'static str,
) -> Result<NativeE2eStagedResource, nopal_native_lifecycle::supervisor::NativeApplicationUnavailable>
{
    use nopal_native_lifecycle::recovery::{
        DurableIdentity, DurableRecoveryEntry, DurableRecoveryRecipe, FilesystemRecoveryRecipe,
    };
    use nopal_native_lifecycle::resources::ResourceOwnership;
    use nopal_native_lifecycle::supervisor::NativeApplicationUnavailable;

    let path = std::env::var_os(variable)
        .map(std::path::PathBuf::from)
        .ok_or_else(|| NativeApplicationUnavailable::new(format!("{variable} is not set")))?;
    let entry = DurableRecoveryEntry::new(
        format!("live-{label}-binding"),
        format!("live fake {label} binding"),
        ResourceOwnership::ApplicationOwned,
        DurableRecoveryRecipe::Filesystem(
            FilesystemRecoveryRecipe::new(
                &path,
                DurableIdentity::new("test.file.contents", contents)
                    .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?,
            )
            .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?,
        ),
    )
    .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
    Ok(NativeE2eStagedResource {
        resource: NativeE2eOwnedResource {
            label,
            path,
            pause_during_close: label == "terminal",
        },
        recovery_entry: entry,
        contents,
    })
}

#[cfg(unix)]
impl nopal_native_lifecycle::application::ResolvedNativeApplicationHostFactory
    for NativeE2eHostFactory
{
    type Host = NativeE2eHost;

    fn create_host(
        &self,
        _field: &nopal_feed_client::field::FieldSnapshot,
        restore: &nopal_native_lifecycle::reconcile::RestoreResolution,
        recovery: &nopal_native_lifecycle::application::OwnedResourceRecoveryReport,
        _preference_notice: Option<&nopal_native_lifecycle::application::RestorePreferenceNotice>,
        current_field: nopal_native_lifecycle::current_field::CurrentCoreFieldAuthority,
    ) -> Result<Self::Host, nopal_native_lifecycle::supervisor::NativeApplicationUnavailable> {
        use nopal_native_lifecycle::application::OwnedResourceRecoveryReport;
        use nopal_native_lifecycle::application::ScopedOwnedResourceRecovery;
        use nopal_native_lifecycle::reconcile::RestoreResolution;
        use nopal_native_lifecycle::recovery::RecoveryJournalStore;
        use nopal_native_lifecycle::resources::{
            ApplicationResources, ResourceDescriptor, ResourceKind,
        };
        use nopal_native_lifecycle::state_root::{
            CanonicalStateRoot, NativeInstanceScope, ReleaseChannel,
        };
        use nopal_native_lifecycle::supervisor::NativeApplicationUnavailable;

        let expected_recovery_count = std::env::var("NOPAL_FAKE_NATIVE_EXPECTED_RECOVERY_COUNT")
            .unwrap_or_else(|_| "1".to_owned())
            .parse::<usize>()
            .map_err(|error| {
                NativeApplicationUnavailable::new(format!("parse expected recovery count: {error}"))
            })?;
        let (recovery_count, remaining_entries) = match recovery {
            OwnedResourceRecoveryReport::Empty => (0, 0),
            OwnedResourceRecoveryReport::Reconciled(report) => {
                (report.attempts().len(), report.remaining_entries())
            }
        };
        if recovery_count != expected_recovery_count || remaining_entries != 0 {
            return Err(NativeApplicationUnavailable::new(format!(
                "unexpected recovery report: {} attempt(s), {} remaining; expected {expected_recovery_count}",
                recovery_count, remaining_entries
            )));
        }
        let restore_label = native_e2e_restore_label(restore);
        let expected_restore = std::env::var("NOPAL_FAKE_NATIVE_EXPECTED_RESTORE")
            .unwrap_or_else(|_| "exact:plot-b/session-b2".to_owned());
        if restore_label != expected_restore {
            return Err(NativeApplicationUnavailable::new(format!(
                "restored {restore_label} instead of {expected_restore}; resolution: {restore:?}"
            )));
        }

        if let Ok(selection) = std::env::var("NOPAL_FAKE_NATIVE_SELECT") {
            let (plot_id, session_id) = selection.split_once('/').ok_or_else(|| {
                NativeApplicationUnavailable::new(format!(
                    "fake native selection must be plot/session, got {selection:?}"
                ))
            })?;
            let outcome = current_field
                .persist_session(
                    current_field.accepted_generation(),
                    &nopal_native_lifecycle::reconcile::ExactSessionSelection::new(
                        plot_id, session_id,
                    ),
                )
                .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
            if !matches!(
                outcome,
                nopal_native_lifecycle::current_field::CurrentSelectionWriteOutcome::Written
            ) {
                return Err(NativeApplicationUnavailable::new(format!(
                    "fake native exact selection was rejected: {outcome:?}"
                )));
            }
            append_native_e2e_event(&format!("selection_persisted {selection}"))
                .map_err(NativeApplicationUnavailable::new)?;
        }

        let host_id = std::process::id();
        let state_dir = std::env::var("NOPAL_FAKE_NATIVE_STATE_DIR")
            .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
        let scope = NativeInstanceScope::new(
            CanonicalStateRoot::create(&state_dir)
                .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?,
            ReleaseChannel::Stable,
        );
        let journal_path =
            ScopedOwnedResourceRecovery::<NativeE2eRecoveryAdapter>::journal_path(&scope);
        let exact_session = match restore {
            RestoreResolution::Exact(selection) => {
                nopal_native_lifecycle::resources::ExactSessionIdentity::new(
                    selection.plot_id(),
                    selection.session_id(),
                )
            }
            RestoreResolution::Fallback { selection, .. } => {
                let Some(session_id) = selection.session_id() else {
                    return Err(NativeApplicationUnavailable::new(
                        "fake native host requires a Session-bearing fallback",
                    ));
                };
                nopal_native_lifecycle::resources::ExactSessionIdentity::new(
                    selection.plot_id(),
                    session_id,
                )
            }
            RestoreResolution::Unavailable { .. } => {
                return Err(NativeApplicationUnavailable::new(
                    "fake native host cannot bind an unavailable Session",
                ));
            }
        }
        .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
        let mut resources = ApplicationResources::new();
        let feed =
            native_e2e_owned_resource("feed", "NOPAL_FAKE_NATIVE_FEED_RESOURCE", "live-feed-v1")?;
        resources
            .register_recoverable_owned(
                ResourceDescriptor::new(ResourceKind::BackgroundResource, "fake feed binding"),
                feed,
                RecoveryJournalStore::new(&journal_path),
            )
            .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
        use nopal_native_lifecycle::session_bindings::{
            SessionBindingController, SessionHostProcessIdentity, StructuredContinuity,
            StructuredCursor, StructuredHistoryToken,
        };
        let session_host_process = std::env::var("NOPAL_FAKE_SESSION_HOST_PID")
            .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?
            .parse::<u32>()
            .map_err(|error| {
                NativeApplicationUnavailable::new(format!(
                    "parse fake Session-host process identity: {error}"
                ))
            })?;
        let session_host_process = SessionHostProcessIdentity::new(session_host_process)
            .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
        let mut bindings = SessionBindingController::start(
            exact_session,
            session_host_process,
            StructuredContinuity::new(
                StructuredCursor::new("cursor-before-host")
                    .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?,
                StructuredHistoryToken::new("history-before-host")
                    .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?,
            ),
            NativeE2eOutputFactory,
            NativeE2eTerminalFactory,
        )
        .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
        if bindings.terminal_binding_identity().is_some()
            || std::env::var_os("NOPAL_FAKE_NATIVE_TERMINAL_RESOURCE")
                .is_some_and(|path| Path::new(&path).exists())
        {
            return Err(NativeApplicationUnavailable::new(
                "healthy structured startup created Terminal eagerly",
            ));
        }
        append_native_e2e_event("output_only_started")
            .map_err(NativeApplicationUnavailable::new)?;
        let request_terminal =
            std::env::var("NOPAL_FAKE_NATIVE_REQUEST_TERMINAL").unwrap_or_else(|_| "0".to_owned());
        match request_terminal.as_str() {
            "0" => {}
            "1" => {
                bindings
                    .request_mode(nopal_native_lifecycle::resources::PresentationMode::Terminal)
                    .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
                let terminal = native_e2e_owned_resource(
                    "terminal",
                    "NOPAL_FAKE_NATIVE_TERMINAL_RESOURCE",
                    "live-terminal-v1",
                )?;
                resources
                    .register_recoverable_owned(
                        ResourceDescriptor::new(ResourceKind::Session, "fake Terminal binding"),
                        terminal,
                        RecoveryJournalStore::new(journal_path),
                    )
                    .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
            }
            other => {
                return Err(NativeApplicationUnavailable::new(format!(
                    "unknown fake Terminal request {other:?}"
                )));
            }
        }
        append_native_e2e_event(&format!(
            "host_constructed host={host_id} restore={restore_label}"
        ))
        .map_err(NativeApplicationUnavailable::new)?;
        let restored_session = bindings.identity().session_id();
        append_native_e2e_event(&format!(
            "feed_bound host={host_id} session={restored_session}"
        ))
        .map_err(NativeApplicationUnavailable::new)?;
        if bindings.terminal_binding_identity().is_some() {
            append_native_e2e_event(&format!(
                "terminal_bound host={host_id} session={restored_session}"
            ))
            .map_err(NativeApplicationUnavailable::new)?;
        }
        Ok(NativeE2eHost {
            host_id,
            bindings,
            resources,
        })
    }
}

#[cfg(unix)]
struct NativeE2eHost {
    host_id: u32,
    bindings: NativeE2eSessionBindings,
    resources: nopal_native_lifecycle::resources::ApplicationResources,
}

#[cfg(unix)]
impl nopal_native_lifecycle::supervisor::NativeApplicationHost for NativeE2eHost {
    fn activate(
        &mut self,
        _deadline: nopal_native_lifecycle::activation::ActivationDeadline,
    ) -> Result<
        nopal_native_lifecycle::supervisor::NativeApplicationAck,
        nopal_native_lifecycle::supervisor::NativeApplicationUnavailable,
    > {
        assert!(!self.resources.is_shutdown());
        if let Some(terminal) = self.bindings.terminal_binding_identity() {
            assert_eq!(terminal.session(), self.bindings.identity());
            assert_eq!(
                terminal.process().get(),
                self.bindings.session_host_process().get()
            );
        }
        append_native_e2e_event(&format!("activation_ack host={} focused", self.host_id))
            .map_err(nopal_native_lifecycle::supervisor::NativeApplicationUnavailable::new)?;
        Ok(nopal_native_lifecycle::supervisor::NativeApplicationAck::Focused)
    }
}

#[cfg(unix)]
#[test]
#[ignore = "helper process launched through the fake native Field wrapper"]
fn fake_native_field_process() {
    if let Err(error) = run_fake_native_field_process() {
        panic!("fake native Field process failed: {error}");
    }
}

#[cfg(unix)]
fn run_fake_native_field_process() -> Result<(), String> {
    use nopal_native_lifecycle::activation::ActivationRequestValidator;
    use nopal_native_lifecycle::application::{
        NativeApplicationStart, NativePrimaryApplication, ScopedOwnedResourceRecovery,
        ScopedRestorePreferenceSource, start_native_application,
    };
    use nopal_native_lifecycle::platform::unix::UnixInstanceCoordinator;
    use nopal_native_lifecycle::state_root::{
        CanonicalStateRoot, NativeInstanceScope, ReleaseChannel,
    };
    use nopal_native_lifecycle::supervisor::{NativeApplicationAck, SerializedPrimaryActivation};
    use nopal_native_lifecycle::transport::serve_unix_activation;

    append_native_e2e_event(&format!(
        "native_process_started pid={}",
        std::process::id()
    ))?;
    wait_for_native_e2e_gate("NOPAL_FAKE_NATIVE_START_GATE", Duration::from_secs(5))?;
    let state_dir = std::env::var("NOPAL_FAKE_NATIVE_STATE_DIR")
        .map_err(|error| format!("read NOPAL_FAKE_NATIVE_STATE_DIR: {error}"))?;
    let scope = NativeInstanceScope::new(
        CanonicalStateRoot::create(&state_dir)
            .map_err(|error| format!("open canonical state root {state_dir}: {error}"))?,
        ReleaseChannel::Stable,
    );
    let coordinator = UnixInstanceCoordinator::with_default_control_root(scope.clone())
        .map_err(|error| format!("create Unix instance coordinator: {error}"))?;
    let platform = NativeE2ePlatform {
        coordinator,
        scope_fingerprint: scope.fingerprint().to_owned(),
    };
    let mut recovery = ScopedOwnedResourceRecovery::new(NativeE2eRecoveryAdapter);
    let start = start_native_application(
        &scope,
        &platform,
        &mut recovery,
        &ScopedRestorePreferenceSource,
        &NativeE2eCoreSource,
        &NativeE2eHostFactory,
        Duration::from_secs(3),
    )
    .map_err(|error| format!("compose native application: {error}"))?;

    let NativeApplicationStart::Primary(application) = start else {
        let NativeApplicationStart::Secondary { acknowledgement } = start else {
            unreachable!();
        };
        let acknowledgement = match acknowledgement {
            NativeApplicationAck::Focused => "focused",
            NativeApplicationAck::Reopened => "reopened",
        };
        println!("fake-native role=secondary acknowledgement={acknowledgement}");
        return Ok(());
    };

    let mode = std::env::var("NOPAL_FAKE_NATIVE_MODE").unwrap_or_else(|_| "race".to_owned());
    if mode == "single" {
        append_native_e2e_event("primary_ready")?;
        wait_for_native_e2e_gate("NOPAL_FAKE_NATIVE_RELEASE_GATE", Duration::from_secs(10))?;
        drop(application);
        append_native_e2e_event("primary_lease_released")?;
        println!("fake-native role=primary mode=single");
        return Ok(());
    }
    if mode != "race" {
        return Err(format!("unknown fake native mode {mode:?}"));
    }

    application
        .lease()
        .listener()
        .set_nonblocking(true)
        .map_err(|error| format!("make activation listener nonblocking: {error}"))?;
    let accept_started = std::time::Instant::now();
    let activation_stream = loop {
        match application.lease().accept() {
            Ok(stream) => break stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if accept_started.elapsed() >= Duration::from_secs(5) {
                    return Err("timed out waiting for the racing secondary activation".to_owned());
                }
                thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(format!("accept secondary activation: {error}")),
        }
    };
    let mut validator = ActivationRequestValidator::new(scope.fingerprint())
        .map_err(|error| format!("create activation validator: {error}"))?;
    let service =
        SerializedPrimaryActivation::new(*application, NativePrimaryApplication::activate);
    let activation = serve_unix_activation(
        activation_stream,
        &mut validator,
        &service,
        Duration::from_secs(3),
    )
    .map_err(|error| format!("serve secondary activation: {error}"))?;
    append_native_e2e_event(&format!("activation_served outcome={activation:?}"))?;
    wait_for_native_e2e_gate("NOPAL_FAKE_NATIVE_RELEASE_GATE", Duration::from_secs(5))?;
    drop(service);
    append_native_e2e_event("primary_lease_released")?;
    println!("fake-native role=primary activation={activation:?}");
    Ok(())
}

#[cfg(unix)]
#[test]
fn real_cli_native_route_composes_one_recovered_exact_host_across_racing_launches() {
    use nopal_native_lifecycle::application::ScopedOwnedResourceRecovery;
    use nopal_native_lifecycle::instance::{InstanceAcquisition, InstancePlatform};
    use nopal_native_lifecycle::platform::unix::UnixInstanceCoordinator;
    use nopal_native_lifecycle::recovery::{RecoveryJournalReadOutcome, RecoveryJournalStore};
    use nopal_native_lifecycle::state_root::{
        CanonicalStateRoot, NativeInstanceScope, ReleaseChannel,
    };

    let fixture = NativeFieldE2eFixture::new();
    let mut first_command = fixture.command();
    let mut second_command = fixture.command();
    let mut first = NativeE2eChild::spawn(&mut first_command).unwrap();
    let mut second = NativeE2eChild::spawn(&mut second_command).unwrap();
    fs::write(fixture.start_gate(), "go").unwrap();

    let completion_started = std::time::Instant::now();
    let first_completed = loop {
        if first.has_exited().unwrap() {
            break true;
        }
        if second.has_exited().unwrap() {
            break false;
        }
        assert!(
            completion_started.elapsed() < Duration::from_secs(8),
            "neither racing launch completed; events:\n{}",
            fs::read_to_string(fixture.events()).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(10));
    };

    let (secondary_child, primary_child) = if first_completed {
        (first, second)
    } else {
        (second, first)
    };
    let secondary = secondary_child
        .wait_with_output_bounded(Duration::from_secs(8))
        .unwrap();
    fs::write(fixture.release_gate(), "release").unwrap();
    fixture
        .wait_for_event("resource_close_started terminal", Duration::from_secs(5))
        .unwrap();
    let scope = NativeInstanceScope::new(
        CanonicalStateRoot::create(fixture.state_dir()).unwrap(),
        ReleaseChannel::Stable,
    );
    let coordinator = UnixInstanceCoordinator::with_default_control_root(scope.clone()).unwrap();
    let (lease_held_during_cleanup, cleanup_acquisition_diagnostic) =
        match coordinator.acquire(Duration::from_millis(100)) {
            Ok(InstanceAcquisition::Secondary(_)) => (true, "secondary connection".to_owned()),
            Ok(InstanceAcquisition::Primary(_)) => (false, "unexpected primary lease".to_owned()),
            Err(error) => (false, format!("acquisition error: {error}")),
        };
    fs::write(fixture.cleanup_gate(), "continue").unwrap();
    let primary = primary_child
        .wait_with_output_bounded(Duration::from_secs(8))
        .unwrap();
    let evidence = fixture.evidence(&secondary, &primary);

    assert!(secondary.status.success(), "{evidence}");
    assert!(primary.status.success(), "{evidence}");
    assert!(
        lease_held_during_cleanup,
        "singleton lease was not held while host resources were closing ({cleanup_acquisition_diagnostic}); {evidence}"
    );
    assert!(
        stdout(&secondary).contains("role=secondary acknowledgement=focused"),
        "{evidence}"
    );
    assert!(
        stdout(&primary).contains("role=primary activation=Focused"),
        "{evidence}"
    );
    assert!(!fixture.stale_resource().exists(), "{evidence}");
    assert!(!fixture.live_feed_resource().exists(), "{evidence}");
    assert!(!fixture.live_terminal_resource().exists(), "{evidence}");

    let events = fs::read_to_string(fixture.events()).unwrap();
    let event_lines = events.lines().collect::<Vec<_>>();
    for unique_prefix in [
        "recovery_removed ",
        "core_snapshot_loaded",
        "host_constructed ",
        "feed_bound ",
        "terminal_bound ",
        "activation_ack ",
        "activation_served ",
        "resource_close_started terminal",
        "resource_close_started feed",
        "resource_closed terminal",
        "resource_closed feed",
        "primary_lease_released",
    ] {
        assert_eq!(
            event_lines
                .iter()
                .filter(|line| line.starts_with(unique_prefix))
                .count(),
            1,
            "expected one {unique_prefix:?} event; {evidence}"
        );
    }
    let position = |prefix: &str| {
        event_lines
            .iter()
            .position(|line| line.starts_with(prefix))
            .unwrap()
    };
    assert!(
        position("recovery_removed ") < position("core_snapshot_loaded")
            && position("core_snapshot_loaded") < position("host_constructed "),
        "recovery must precede Core and host construction; {evidence}"
    );
    assert!(
        position("resource_closed terminal") < position("resource_closed feed"),
        "owned resources must close in reverse acquisition order; {evidence}"
    );
    assert!(
        event_lines
            .iter()
            .any(|line| line.contains("restore=exact:plot-b/session-b2")),
        "{evidence}"
    );
    let host_identity = event_lines
        .iter()
        .find(|line| line.starts_with("host_constructed "))
        .and_then(|line| {
            line.split_whitespace()
                .find(|part| part.starts_with("host="))
        })
        .unwrap();
    for bound_event in ["feed_bound ", "terminal_bound ", "activation_ack "] {
        let line = event_lines
            .iter()
            .find(|line| line.starts_with(bound_event))
            .unwrap();
        assert!(line.contains(host_identity), "{evidence}");
    }

    let recovery_journal = RecoveryJournalStore::new(ScopedOwnedResourceRecovery::<
        NativeE2eRecoveryAdapter,
    >::journal_path(&scope));
    assert!(
        matches!(recovery_journal.read(), RecoveryJournalReadOutcome::Missing),
        "ordinary shutdown left a durable owned-resource journal; {evidence}"
    );
    assert!(
        matches!(
            coordinator.acquire(Duration::from_millis(100)).unwrap(),
            InstanceAcquisition::Primary(_)
        ),
        "primary lease remained held after clean exit; {evidence}"
    );
}

#[cfg(unix)]
#[test]
fn real_cli_native_route_recovers_owned_resources_after_forced_primary_crash() {
    use nopal_native_lifecycle::application::ScopedOwnedResourceRecovery;
    use nopal_native_lifecycle::recovery::{RecoveryJournalReadOutcome, RecoveryJournalStore};
    use nopal_native_lifecycle::state_root::{
        CanonicalStateRoot, NativeInstanceScope, ReleaseChannel,
    };

    let fixture = NativeFieldE2eFixture::new();
    fs::write(fixture.borrowed_session_marker(), "core-owned-session-b2").unwrap();
    remove_native_e2e_gate(&fixture.release_gate());
    remove_native_e2e_gate(&fixture.cleanup_gate());
    fs::write(fixture.start_gate(), "go").unwrap();
    let mut first_command = fixture.single_command(1);
    let first_child = NativeE2eChild::spawn(&mut first_command).unwrap();
    fixture
        .wait_for_event_count("primary_ready", 1, Duration::from_secs(8))
        .unwrap();

    let scope = NativeInstanceScope::new(
        CanonicalStateRoot::create(fixture.state_dir()).unwrap(),
        ReleaseChannel::Stable,
    );
    let recovery_journal = RecoveryJournalStore::new(ScopedOwnedResourceRecovery::<
        NativeE2eRecoveryAdapter,
    >::journal_path(&scope));
    let RecoveryJournalReadOutcome::Ready(journal_before_crash) = recovery_journal.read() else {
        panic!("live primary did not durably register its recoverable resources");
    };
    assert_eq!(journal_before_crash.entries().len(), 2);
    assert!(fixture.live_feed_resource().exists());
    assert!(fixture.live_terminal_resource().exists());

    force_kill_latest_native_process(&fixture);
    let crashed = first_child
        .wait_with_output_bounded(Duration::from_secs(8))
        .unwrap();
    assert!(
        !crashed.status.success(),
        "forced crash exited successfully"
    );
    assert!(fixture.live_feed_resource().exists());
    assert!(fixture.live_terminal_resource().exists());
    assert_eq!(
        fs::read_to_string(fixture.borrowed_session_marker()).unwrap(),
        "core-owned-session-b2"
    );

    let second = complete_native_e2e_single_launch(&fixture, fixture.single_command(2), 2);
    let evidence = fixture.evidence(&crashed, &second);
    assert!(second.status.success(), "{evidence}");
    assert!(!fixture.live_feed_resource().exists(), "{evidence}");
    assert!(!fixture.live_terminal_resource().exists(), "{evidence}");
    assert_eq!(
        fs::read_to_string(fixture.borrowed_session_marker()).unwrap(),
        "core-owned-session-b2",
        "recovery touched the borrowed Core Session marker; {evidence}"
    );
    assert!(
        matches!(recovery_journal.read(), RecoveryJournalReadOutcome::Missing),
        "relaunch left a durable owned-resource journal; {evidence}"
    );
    let events = fs::read_to_string(fixture.events()).unwrap();
    assert_eq!(
        events
            .lines()
            .filter(|line| line.starts_with("recovery_removed "))
            .count(),
        3,
        "initial stale recovery plus both crash-owned resources must be reconciled; {evidence}"
    );
    assert_eq!(
        events
            .lines()
            .filter(|line| line.contains("restore=exact:plot-b/session-b2"))
            .count(),
        2,
        "exact selection was not restored across crash restart; {evidence}"
    );
}

#[cfg(unix)]
#[test]
fn real_cli_native_route_recovers_crashes_on_both_sides_of_staged_activation() {
    use nopal_native_lifecycle::application::ScopedOwnedResourceRecovery;
    use nopal_native_lifecycle::recovery::{RecoveryJournalReadOutcome, RecoveryJournalStore};
    use nopal_native_lifecycle::state_root::{
        CanonicalStateRoot, NativeInstanceScope, ReleaseChannel,
    };

    let fixture = NativeFieldE2eFixture::new();
    fs::write(fixture.borrowed_session_marker(), "core-owned-session-b2").unwrap();
    fs::write(fixture.start_gate(), "go").unwrap();
    let scope = NativeInstanceScope::new(
        CanonicalStateRoot::create(fixture.state_dir()).unwrap(),
        ReleaseChannel::Stable,
    );
    let recovery_journal = RecoveryJournalStore::new(ScopedOwnedResourceRecovery::<
        NativeE2eRecoveryAdapter,
    >::journal_path(&scope));

    let mut before_create_command = fixture.single_command(1);
    before_create_command
        .env("NOPAL_FAKE_NATIVE_REQUEST_TERMINAL", "0")
        .env("NOPAL_FAKE_NATIVE_STAGE_PAUSE", "before-create:feed");
    let before_create = NativeE2eChild::spawn(&mut before_create_command).unwrap();
    fixture
        .wait_for_event("stage_paused_before_create feed", Duration::from_secs(8))
        .unwrap();
    let RecoveryJournalReadOutcome::Ready(before_create_journal) = recovery_journal.read() else {
        panic!("pre-activation crash window was not durably registered");
    };
    assert_eq!(before_create_journal.entries().len(), 1);
    assert!(!fixture.live_feed_resource().exists());
    force_kill_latest_native_process(&fixture);
    let before_create = before_create
        .wait_with_output_bounded(Duration::from_secs(8))
        .unwrap();
    assert!(!before_create.status.success());

    let mut after_create_command = fixture.single_command(1);
    after_create_command
        .env("NOPAL_FAKE_NATIVE_REQUEST_TERMINAL", "0")
        .env("NOPAL_FAKE_NATIVE_STAGE_PAUSE", "after-create:feed");
    let after_create = NativeE2eChild::spawn(&mut after_create_command).unwrap();
    fixture
        .wait_for_event("stage_paused_after_create feed", Duration::from_secs(8))
        .unwrap();
    let RecoveryJournalReadOutcome::Ready(after_create_journal) = recovery_journal.read() else {
        panic!("post-create crash window lost durable recovery authority");
    };
    assert_eq!(after_create_journal.entries().len(), 1);
    assert!(fixture.live_feed_resource().exists());
    force_kill_latest_native_process(&fixture);
    let after_create = after_create
        .wait_with_output_bounded(Duration::from_secs(8))
        .unwrap();
    assert!(!after_create.status.success());

    let mut recovery_command = fixture.single_command(1);
    recovery_command.env("NOPAL_FAKE_NATIVE_REQUEST_TERMINAL", "0");
    let recovered = complete_native_e2e_single_launch(&fixture, recovery_command, 1);
    let evidence = fixture.evidence(&after_create, &recovered);
    assert!(recovered.status.success(), "{evidence}");
    assert!(!fixture.live_feed_resource().exists(), "{evidence}");
    assert_eq!(
        fs::read_to_string(fixture.borrowed_session_marker()).unwrap(),
        "core-owned-session-b2",
        "staged recovery touched the borrowed Session marker; {evidence}"
    );
    assert!(
        matches!(recovery_journal.read(), RecoveryJournalReadOutcome::Missing),
        "staged crash recovery left a durable journal; {evidence}"
    );
}

#[cfg(unix)]
#[test]
fn real_cli_native_route_persists_exact_selection_and_falls_back_from_stale_core_facts() {
    let fixture = NativeFieldE2eFixture::new();

    let mut first = fixture.single_command(1);
    first.env("NOPAL_FAKE_NATIVE_SELECT", "plot-a/session-a");
    let first = complete_native_e2e_single_launch(&fixture, first, 1);
    assert!(first.status.success(), "{}", stderr(&first));

    let mut second = fixture.single_command(0);
    second
        .env(
            "NOPAL_FAKE_NATIVE_EXPECTED_RESTORE",
            "exact:plot-a/session-a",
        )
        .env("NOPAL_FAKE_NATIVE_SELECT", "plot-b/session-b2");
    let second = complete_native_e2e_single_launch(&fixture, second, 2);
    assert!(second.status.success(), "{}", stderr(&second));

    let mut stale_session = fixture.single_command(0);
    stale_session
        .env("NOPAL_FAKE_NATIVE_CORE_VARIANT", "missing-session-b2")
        .env(
            "NOPAL_FAKE_NATIVE_EXPECTED_RESTORE",
            "fallback:plot-a/session-a:session-missing",
        );
    let stale_session = complete_native_e2e_single_launch(&fixture, stale_session, 3);
    assert!(stale_session.status.success(), "{}", stderr(&stale_session));

    let mut stale_plot = fixture.single_command(0);
    stale_plot
        .env("NOPAL_FAKE_NATIVE_CORE_VARIANT", "missing-plot-b")
        .env(
            "NOPAL_FAKE_NATIVE_EXPECTED_RESTORE",
            "fallback:plot-a/session-a:plot-missing",
        );
    let stale_plot = complete_native_e2e_single_launch(&fixture, stale_plot, 4);
    assert!(stale_plot.status.success(), "{}", stderr(&stale_plot));

    let events = fs::read_to_string(fixture.events()).unwrap();
    for expected in [
        "selection_persisted plot-a/session-a",
        "restore=exact:plot-a/session-a",
        "selection_persisted plot-b/session-b2",
        "restore=fallback:plot-a/session-a:session-missing",
        "restore=fallback:plot-a/session-a:plot-missing",
    ] {
        assert!(
            events.contains(expected),
            "missing {expected:?}; events:\n{events}"
        );
    }
}

#[cfg(unix)]
#[test]
fn real_cli_native_route_starts_output_only_and_attaches_no_terminal_without_intent() {
    let fixture = NativeFieldE2eFixture::new();
    let mut command = fixture.single_command(1);
    command.env("NOPAL_FAKE_NATIVE_REQUEST_TERMINAL", "0");

    let output = complete_native_e2e_single_launch(&fixture, command, 1);
    let events = fs::read_to_string(fixture.events()).unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    assert!(
        events.contains(
            "typed_output_bound plot=plot-b session=session-b2 runtime=fake-structured-runtime"
        ),
        "structured Output was not typed to the exact restored Session; events:\n{events}"
    );
    assert!(events.contains("output_only_started"), "events:\n{events}");
    for forbidden in [
        "typed_terminal_bound",
        "resource_activated terminal",
        "terminal_bound",
        "typed_terminal_closed",
    ] {
        assert!(
            !events.contains(forbidden),
            "healthy output-only startup performed {forbidden:?}; events:\n{events}"
        );
    }
    assert!(!fixture.live_feed_resource().exists());
    assert!(!fixture.live_terminal_resource().exists());
}

#[cfg(unix)]
#[test]
fn explicit_native_field_launches_without_a_tty_and_forwards_exact_inputs() {
    let temp = tempfile::tempdir().unwrap();
    let native = temp.path().join("native-field-stub");
    let captured = temp.path().join("native-args.txt");
    fs::write(
        &native,
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$NOPAL_NATIVE_TEST_CAPTURE\"\nexit 23\n",
    )
    .unwrap();
    fs::set_permissions(&native, fs::Permissions::from_mode(0o700)).unwrap();

    let state_dir = temp.path().join("state root");
    let out = nopal_command()
        .args([
            "field",
            "native",
            "--state-dir",
            state_dir.to_str().unwrap(),
        ])
        .env("NOPAL_FIELD_NATIVE_BIN", &native)
        .env("NOPAL_NATIVE_TEST_CAPTURE", &captured)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(23), "stderr: {}", stderr(&out));
    assert_eq!(
        fs::read_to_string(captured).unwrap(),
        format!("--state-dir\n{}\n", state_dir.display())
    );
}

#[cfg(unix)]
#[test]
fn explicit_native_field_fails_honestly_when_binary_is_missing() {
    let temp = tempfile::tempdir().unwrap();
    let missing = temp.path().join("missing-nopal-field-native");
    let out = nopal_command()
        .args(["field", "native"])
        .env("NOPAL_FIELD_NATIVE_BIN", &missing)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(2));
    let error = stderr(&out);
    assert!(error.contains("nopal-field-native"), "{error}");
    assert!(error.contains(missing.to_str().unwrap()), "{error}");
    assert!(error.contains("failed to launch"), "{error}");
    assert!(
        !error.contains("needs a terminal"),
        "must not fall back: {error}"
    );
}

#[test]
fn field_help_names_native_and_legacy_product_routes() {
    let out = nopal(&["field", "--help"]);
    assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
    let help = stdout(&out);
    assert!(help.contains("native"), "{help}");
    assert!(help.contains("legacy"), "{help}");
    assert!(
        help.contains("tmux"),
        "legacy route must be explicit: {help}"
    );
}

#[test]
fn explicit_legacy_field_keeps_the_terminal_requirement() {
    let out = nopal(&["field", "legacy"]);
    assert_eq!(out.status.code(), Some(2));
    assert!(
        stderr(&out).contains("needs a terminal"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn placement_reuses_nopal_policy_decision() {
    let temp = tempfile::tempdir().unwrap();
    write_project(temp.path());

    let out = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "placement",
        "--mode",
        "nopal_tui",
        "--action",
        "run.start",
        "--class",
        "workspace_write",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.placement/v1");
    assert_eq!(doc["placement"], "dedicated_repo_runtime");
    assert_eq!(doc["placement_source"], "mode_default");
}

#[cfg(unix)]
#[test]
fn rondo_start_and_health_use_one_detached_user_scoped_runtime() {
    let temp = tempfile::tempdir().unwrap();
    write_project(temp.path());
    let lifecycle = LifecycleFixture::new();

    let start = lifecycle.nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "rondo",
        "start",
        "--placement",
        "dedicated_repo_runtime",
    ]);
    assert_eq!(start.status.code(), Some(0));
    let start_doc = json(&start);
    assert_eq!(start_doc["kind"], "nopal.rondo_service/v1");
    assert_eq!(start_doc["status"], "running");
    assert_eq!(start_doc["placement"], "shared_user_runtime");
    assert_eq!(
        start_doc["instance_id"],
        "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819"
    );
    assert!(lifecycle.state().join("rondo-core.log").is_file());
    assert!(!temp.path().join(".nopal/rondo-core.json").exists());
    assert!(!temp.path().join(".nopal/rondo-core.log").exists());

    let health = lifecycle.nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "rondo",
        "health",
    ]);
    assert_eq!(health.status.code(), Some(0));
    let health_doc = json(&health);
    assert_eq!(health_doc["status"], "running");
    assert_eq!(health_doc["ok"], true);
    assert_eq!(health_doc["base_url"], start_doc["base_url"]);
    assert_eq!(health_doc["instance_id"], start_doc["instance_id"]);

    let second = lifecycle.nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "rondo",
        "start",
        "--placement",
        "shared_user_runtime",
    ]);
    assert_eq!(second.status.code(), Some(0));
    let second_doc = json(&second);
    assert_eq!(second_doc["base_url"], start_doc["base_url"]);
    assert_eq!(second_doc["instance_id"], start_doc["instance_id"]);
}

#[cfg(unix)]
#[test]
fn concurrent_rondo_starts_converge_on_one_endpoint_and_instance() {
    let temp = tempfile::tempdir().unwrap();
    write_project(temp.path());
    let lifecycle = LifecycleFixture::new();
    let args = [
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "rondo",
        "start",
        "--placement",
        "shared_user_runtime",
    ];

    let first = lifecycle
        .command(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let second = lifecycle
        .command(&args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let first = first.wait_with_output().unwrap();
    let second = second.wait_with_output().unwrap();

    assert_eq!(first.status.code(), Some(0), "{}", stderr(&first));
    assert_eq!(second.status.code(), Some(0), "{}", stderr(&second));
    let first = json(&first);
    let second = json(&second);
    assert_eq!(first["base_url"], second["base_url"]);
    assert_eq!(first["instance_id"], second["instance_id"]);
    assert_eq!(first["instance_id"], "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819");
}

#[cfg(unix)]
#[test]
fn idle_core_restarts_with_a_new_process_and_stops_idempotently() {
    let fixture = LifecycleFixture::new();
    let repo = tempfile::tempdir().unwrap();
    write_project(repo.path());
    fs::create_dir_all(fixture.state()).unwrap();
    fs::write(
        fixture.state().join("rondo-core.log"),
        vec![b'x'; 10 * 1024 * 1024 + 1],
    )
    .unwrap();
    let start = fixture.nopal(&[
        "--dir",
        repo.path().to_str().unwrap(),
        "--json",
        "rondo",
        "start",
    ]);
    assert_eq!(start.status.code(), Some(0), "stderr: {}", stderr(&start));
    assert!(fixture.state().join("rondo-core.log.1").is_file());
    let before: serde_json::Value =
        serde_json::from_slice(&fs::read(fixture.state().join("runtime.json")).unwrap()).unwrap();

    let mut previous = before;
    for _ in 0..3 {
        let restart = fixture.nopal(&[
            "--dir",
            repo.path().to_str().unwrap(),
            "--json",
            "rondo",
            "restart",
        ]);
        assert_eq!(
            restart.status.code(),
            Some(0),
            "stderr: {}",
            stderr(&restart)
        );
        assert_eq!(json(&restart)["status"], "running");
        let after: serde_json::Value =
            serde_json::from_slice(&fs::read(fixture.state().join("runtime.json")).unwrap())
                .unwrap();
        assert_ne!(previous["core_pid"], after["core_pid"]);
        previous = after;
    }

    for _ in 0..2 {
        let stop = fixture.nopal(&["--json", "rondo", "stop"]);
        assert_eq!(stop.status.code(), Some(0), "stderr: {}", stderr(&stop));
        assert_eq!(json(&stop)["status"], "stopped");
        assert_eq!(json(&stop)["ok"], true);
    }
    assert!(!fixture.state().join("runtime.json").exists());
}

#[cfg(unix)]
#[test]
fn stop_and_restart_refuse_while_verified_runs_are_active() {
    let fixture = LifecycleFixture::new();
    let repo = tempfile::tempdir().unwrap();
    write_project(repo.path());
    fs::write(fixture.active_run_count_file(), "1").unwrap();
    let start = fixture
        .command(&[
            "--dir",
            repo.path().to_str().unwrap(),
            "--json",
            "rondo",
            "start",
        ])
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .output()
        .unwrap();
    assert_eq!(start.status.code(), Some(0), "stderr: {}", stderr(&start));
    let descriptor_before = fs::read(fixture.state().join("runtime.json")).unwrap();

    for operation in ["stop", "restart"] {
        let out = fixture.nopal(&[
            "--dir",
            repo.path().to_str().unwrap(),
            "--json",
            "rondo",
            operation,
        ]);
        assert_eq!(out.status.code(), Some(0), "stderr: {}", stderr(&out));
        let doc = json(&out);
        assert_eq!(doc["ok"], false);
        assert_eq!(doc["status"], "blocked");
        assert_eq!(doc["active_run_count"], 1);
        assert!(
            doc["diagnostics"][0]
                .as_str()
                .unwrap()
                .contains("active runs")
        );
        assert_eq!(
            fs::read(fixture.state().join("runtime.json")).unwrap(),
            descriptor_before
        );
    }
}

#[cfg(unix)]
#[test]
fn installed_layout_discovers_the_packaged_sibling_rondo_without_an_override() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = LifecycleFixture::new();
    let install = tempfile::tempdir().unwrap();
    let installed_nopal = install.path().join("nopal");
    let installed_rondo = install.path().join("rondo");
    fs::copy(env!("CARGO_BIN_EXE_nopal"), &installed_nopal).unwrap();
    fs::copy(&fixture.runtime, &installed_rondo).unwrap();
    fs::set_permissions(&installed_nopal, fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&installed_rondo, fs::Permissions::from_mode(0o700)).unwrap();
    let repo = tempfile::tempdir().unwrap();
    write_project(repo.path());

    let start = Command::new(&installed_nopal)
        .args([
            "--dir",
            repo.path().to_str().unwrap(),
            "--json",
            "rondo",
            "start",
        ])
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .env("NOPAL_RONDO_STATE_DIR", fixture.state())
        .env_remove("NOPAL_RONDO_RUNTIME")
        .output()
        .unwrap();

    assert_eq!(start.status.code(), Some(0), "stderr: {}", stderr(&start));
    assert_eq!(json(&start)["status"], "running");
    assert!(fixture.state().join("runtime.json").is_file());
}

#[cfg(unix)]
#[test]
fn real_cli_launch_starts_core_silently_and_core_survives_pi_exit() {
    use std::os::unix::fs::PermissionsExt;

    let fixture = LifecycleFixture::new();
    let repo = tempfile::tempdir().unwrap();
    write_portable_project(repo.path());
    write_bundle(repo.path(), "{ \"version\": \"nopal.bundle/v1\" }");
    let stub = repo.path().join("pi-stub.sh");
    fs::write(&stub, "#!/bin/sh\nexit 9\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();

    let out = fixture
        .command(&["cli", "--dir", repo.path().to_str().unwrap()])
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .env("NOPAL_PI_BIN", &stub)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(9), "stderr: {}", stderr(&out));
    assert!(!stderr(&out).contains("Rondo Core is unavailable"));
    assert!(fixture.state().join("runtime.json").is_file());
    let health = fixture.nopal(&["--json", "rondo", "health"]);
    assert_eq!(health.status.code(), Some(0), "stderr: {}", stderr(&health));
    assert_eq!(json(&health)["status"], "running");
}

#[cfg(unix)]
#[test]
fn real_cli_launch_warns_once_and_continues_when_core_is_unavailable() {
    use std::os::unix::fs::PermissionsExt;

    let repo = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    write_portable_project(repo.path());
    write_bundle(repo.path(), "{ \"version\": \"nopal.bundle/v1\" }");
    let stub = repo.path().join("pi-stub.sh");
    fs::write(&stub, "#!/bin/sh\nexit 9\n").unwrap();
    fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();

    let out = nopal_command()
        .args(["cli", "--dir", repo.path().to_str().unwrap()])
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .env("NOPAL_RONDO_STATE_DIR", state.path())
        .env("NOPAL_PI_BIN", &stub)
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(9), "stderr: {}", stderr(&out));
    let launch_stderr = stderr(&out);
    assert_eq!(
        launch_stderr.matches("Rondo Core is unavailable").count(),
        1
    );
    assert!(launch_stderr.contains("nopal rondo start"));
    assert!(launch_stderr.contains("nopal rondo health"));
    assert!(!state.path().join("runtime.json").exists());
}

#[cfg(unix)]
#[test]
fn guarded_noninteractive_invalid_and_dry_run_paths_never_start_core() {
    let fixture = LifecycleFixture::new();
    let repo = tempfile::tempdir().unwrap();
    write_portable_project(repo.path());

    for args in [
        vec!["--dir", repo.path().to_str().unwrap(), "cli"],
        vec!["--dir", repo.path().to_str().unwrap(), "field"],
        vec!["--dir", repo.path().to_str().unwrap()],
    ] {
        let out = fixture.nopal(&args);
        assert_ne!(out.status.code(), Some(0));
        assert!(!fixture.state().join("runtime.json").exists());
    }

    write_bundle(repo.path(), "{ \"version\": \"nopal.bundle/v1\" }");
    let dry_run = fixture.nopal(&["--dir", repo.path().to_str().unwrap(), "cli", "--dry-run"]);
    assert_eq!(dry_run.status.code(), Some(0));
    assert!(!fixture.state().join("runtime.json").exists());
}

#[cfg(all(unix, target_os = "macos"))]
#[test]
fn bare_nopal_and_field_launch_ensure_core_after_the_terminal_guard() {
    use std::os::unix::fs::PermissionsExt;

    for field_args in [false, true] {
        let fixture = LifecycleFixture::new();
        let repo = tempfile::tempdir().unwrap();
        write_portable_project(repo.path());
        write_bundle(repo.path(), "{ \"version\": \"nopal.bundle/v1\" }");
        let tools = tempfile::tempdir().unwrap();
        let tmux = tools.path().join("tmux");
        fs::write(
            &tmux,
            "#!/bin/sh\nif [ \"$1\" = has-session ]; then exit 1; fi\nif [ \"$1\" = new-session ]; then echo '%1'; fi\nif [ \"$1\" = attach-session ]; then exit 9; fi\nexit 0\n",
        )
        .unwrap();
        fs::set_permissions(&tmux, fs::Permissions::from_mode(0o700)).unwrap();
        let mut args = vec![
            "-q",
            "/dev/null",
            env!("CARGO_BIN_EXE_nopal"),
            "--dir",
            repo.path().to_str().unwrap(),
        ];
        if field_args {
            args.push("field");
        }
        let path = format!(
            "{}:{}",
            tools.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );

        let output = Command::new("script")
            .args(args)
            .env("NOPAL_CONFIG_DIR", isolated_config_dir())
            .env("NOPAL_RONDO_STATE_DIR", fixture.state())
            .env("NOPAL_RONDO_RUNTIME", &fixture.runtime)
            .env("PATH", path)
            .output()
            .unwrap();

        assert!(
            fixture.state().join("runtime.json").is_file(),
            "field_args={field_args}, stdout={}, stderr={}",
            stdout(&output),
            stderr(&output)
        );
        let health = fixture.nopal(&["--json", "rondo", "health"]);
        assert_eq!(json(&health)["status"], "running");
    }
}

#[test]
fn blocked_placement_does_not_start_rondo_stub() {
    let temp = tempfile::tempdir().unwrap();
    write_project(temp.path());

    let start = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "rondo",
        "start",
        "--placement",
        "blocked",
    ]);

    assert_eq!(start.status.code(), Some(0));
    let doc = json(&start);
    assert_eq!(doc["status"], "blocked");
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["placement"], "blocked");
    assert!(!temp.path().join(".nopal/rondo-core.json").exists());
    assert!(!temp.path().join(".nopal/rondo-core.log").exists());
}

#[test]
fn policy_blocked_placement_does_not_start_rondo_stub() {
    let temp = tempfile::tempdir().unwrap();
    write_project_with_placement(temp.path(), "blocked");

    let start = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "rondo",
        "start",
    ]);

    assert_eq!(start.status.code(), Some(0));
    let doc = json(&start);
    assert_eq!(doc["status"], "blocked");
    assert_eq!(doc["ok"], false);
    assert!(!temp.path().join(".nopal/rondo-core.json").exists());
    assert!(!temp.path().join(".nopal/rondo-core.log").exists());

    let dry_run = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "start",
    ]);
    let dry_run_doc = json(&dry_run);
    assert_eq!(dry_run_doc["placement"], "blocked");
    assert!(
        dry_run_doc["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().contains("blocked"))
    );
    assert!(
        !dry_run_doc["next_steps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().contains("rondo start"))
    );
}

#[test]
fn weaker_explicit_placement_cannot_bypass_blocked_policy() {
    let temp = tempfile::tempdir().unwrap();
    write_project_with_placement(temp.path(), "blocked");

    let start = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "rondo",
        "start",
        "--placement",
        "shared_user_runtime",
    ]);

    assert_eq!(start.status.code(), Some(0));
    let doc = json(&start);
    assert_eq!(doc["status"], "blocked");
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["placement"], "blocked");
    assert!(!temp.path().join(".nopal/rondo-core.json").exists());
    assert!(!temp.path().join(".nopal/rondo-core.log").exists());
}

#[cfg(unix)]
#[test]
fn run_start_policy_inputs_can_come_from_nopal_config() {
    let temp = tempfile::tempdir().unwrap();
    write_project_with_placement(temp.path(), "blocked");
    write_nopal_config(
        temp.path(),
        "configured_nopal",
        "configured.run",
        &["network_read"],
    );

    fs::write(
        temp.path().join(".nopal/policy.jsonc"),
        r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "nopal_tui": {
      "default_decision": "ask",
      "default_placement": "blocked",
      "rules": []
    },
    "configured_nopal": {
      "default_decision": "ask",
      "default_placement": "dedicated_run_runtime",
      "rules": [
        {
          "id": "configured-run-placement",
          "actions": ["configured.run"],
          "classes": ["network_read"],
          "placement": "dedicated_run_runtime"
        }
      ]
    }
  }
}
"#,
    )
    .unwrap();

    let dry_run = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "start",
    ]);

    assert_eq!(dry_run.status.code(), Some(0));
    let dry_run_doc = json(&dry_run);
    assert_eq!(dry_run_doc["placement"], "dedicated_run_runtime");
    assert!(
        !dry_run_doc["blockers"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().contains("blocked"))
    );

    let lifecycle = LifecycleFixture::new();
    let start = lifecycle.nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "rondo",
        "start",
        "--placement",
        "shared_user_runtime",
    ]);
    assert_eq!(start.status.code(), Some(0));
    let start_doc = json(&start);
    assert_eq!(start_doc["status"], "running");
    assert_eq!(start_doc["placement"], "shared_user_runtime");
}

#[test]
fn invalid_nopal_config_blocks_run_start_conservatively() {
    let temp = tempfile::tempdir().unwrap();
    write_project(temp.path());
    fs::create_dir_all(temp.path().join(".nopal")).unwrap();
    fs::write(
        temp.path().join(".nopal/config.jsonc"),
        r#"{
  "version": "nopal.config/v1",
  "run_start_policy": {
    "mode": "",
    "action": "run.start",
    "classes": ["workspace_write"]
  }
}
"#,
    )
    .unwrap();

    let start = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "rondo",
        "start",
    ]);

    assert_eq!(start.status.code(), Some(0));
    let doc = json(&start);
    assert_eq!(doc["status"], "blocked");
    assert_eq!(doc["ok"], false);
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item.as_str().unwrap().contains("nopal config"))
    );
    assert!(!temp.path().join(".nopal/rondo-core.json").exists());
}

#[test]
fn run_start_is_dry_run_only_and_does_not_submit() {
    let temp = tempfile::tempdir().unwrap();
    write_project(temp.path());

    let out = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "start",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.run_start_dry_run/v1");
    assert_eq!(doc["dry_run"], true);
    assert_eq!(doc["would_submit"], false);
    assert_eq!(doc["placement"], "dedicated_repo_runtime");
    assert_eq!(doc["rondo_status"], "not_started");
    assert!(!temp.path().join(".nopal/rondo-core.json").exists());
}

#[test]
fn run_help_registers_submit_and_one_shot_observe_without_replacing_start() {
    let run = nopal(&["run", "--help"]);
    assert_eq!(run.status.code(), Some(0));
    let run_help = stdout(&run);
    assert!(run_help.contains("start"), "stdout: {run_help}");
    assert!(run_help.contains("submit"), "stdout: {run_help}");
    assert!(run_help.contains("observe"), "stdout: {run_help}");

    let submit = nopal(&["run", "submit", "--help"]);
    assert_eq!(submit.status.code(), Some(0));
    assert!(
        stdout(&submit).contains("--manifest <MANIFEST>"),
        "stdout: {}",
        stdout(&submit)
    );

    let observe = nopal(&["run", "observe", "--help"]);
    assert_eq!(observe.status.code(), Some(0));
    let observe_help = stdout(&observe);
    for flag in [
        "--repo-id <REPO_ID>",
        "--run-id <RUN_ID>",
        "--cursor <CURSOR>",
    ] {
        assert!(observe_help.contains(flag), "stdout: {observe_help}");
    }
}

#[test]
fn run_submit_dispatches_versioned_report_and_fails_closed_without_runtime() {
    let temp = tempfile::tempdir().unwrap();
    let plot_state = tempfile::tempdir().unwrap();
    write_established_plot_state(plot_state.path());
    write_portable_project(temp.path());
    fs::write(
        temp.path().join(".nopal/policy.jsonc"),
        r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "nopal_tui": {
      "default_decision": "allow",
      "default_placement": "dedicated_run_runtime",
      "rules": []
    }
  }
}
"#,
    )
    .unwrap();
    let manifest = temp.path().join("approved-slice.json");
    fs::write(&manifest, b"exact approved bytes\n").unwrap();

    let out = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "submit",
        "--manifest",
        manifest.to_str().unwrap(),
        "--plot-id",
        TEST_PLOT_ID,
        "--state-dir",
        plot_state.path().to_str().unwrap(),
    ]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.run_submit/v1");
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["submitted"], false);
    assert_eq!(doc["manifest_path"], "approved-slice.json");
    assert_eq!(doc["manifest_sha256"].as_str().unwrap().len(), 64);
    assert_eq!(doc["decision"], "allow");
    assert_eq!(doc["placement"], "dedicated_run_runtime");
    assert_eq!(doc["handle"], serde_json::Value::Null);
    assert!(
        doc["diagnostics"][0]
            .as_str()
            .unwrap()
            .contains("Rondo runtime is unavailable")
    );
    assert!(!stdout(&out).contains(temp.path().to_str().unwrap()));
}

#[test]
fn run_submit_requires_an_existing_established_plot_before_coordination() {
    let repo = tempfile::tempdir().unwrap();
    let plot_state = tempfile::tempdir().unwrap();
    write_portable_project(repo.path());
    write_established_plot_state(plot_state.path());
    let manifest = repo.path().join("approved-slice.json");
    fs::write(&manifest, b"approved plot validation bytes\n").unwrap();
    let plot_path = plot_state
        .path()
        .join("plots")
        .join(format!("{TEST_PLOT_ID}.json"));
    let mut provisional: serde_json::Value =
        serde_json::from_slice(&fs::read(&plot_path).unwrap()).unwrap();
    provisional["provisional"] = serde_json::Value::Bool(true);
    provisional["establishment"] = serde_json::Value::Null;
    fs::write(&plot_path, serde_json::to_vec(&provisional).unwrap()).unwrap();

    for plot_id in [TEST_PLOT_ID, "plot-missing"] {
        let out = nopal(&[
            "--dir",
            repo.path().to_str().unwrap(),
            "--json",
            "run",
            "submit",
            "--manifest",
            manifest.to_str().unwrap(),
            "--plot-id",
            plot_id,
            "--state-dir",
            plot_state.path().to_str().unwrap(),
        ]);

        assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
        let report = json(&out);
        assert_eq!(report["kind"], "nopal.run_submit/v1");
        assert_eq!(report["submitted"], false);
        assert_eq!(report["handle"], serde_json::Value::Null);
        assert!(report["diagnostics"][0].as_str().unwrap().contains("Plot"));
    }
}

#[cfg(unix)]
#[test]
fn run_submit_starts_shared_lifecycle_on_demand_and_reuses_it_for_deduplication() {
    let fixture = LifecycleFixture::new();
    let repo = tempfile::tempdir().unwrap();
    write_portable_project(repo.path());
    fs::write(
        repo.path().join(".nopal/policy.jsonc"),
        r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "nopal_tui": {
      "default_decision": "allow",
      "default_placement": "dedicated_run_runtime",
      "rules": []
    }
  }
}
"#,
    )
    .unwrap();
    let manifest = repo.path().join("approved-slice.json");
    fs::write(&manifest, b"approved lifecycle fixture bytes\n").unwrap();
    let plot_state = fixture.plot_state();
    let args = [
        "--dir",
        repo.path().to_str().unwrap(),
        "--json",
        "run",
        "submit",
        "--manifest",
        manifest.to_str().unwrap(),
        "--plot-id",
        TEST_PLOT_ID,
        "--state-dir",
        plot_state.to_str().unwrap(),
    ];

    let first = fixture.nopal(&args);
    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    let first_doc = json(&first);
    assert_eq!(first_doc["ok"], true);
    assert_eq!(first_doc["deduplicated"], false);
    assert_eq!(first_doc["handle"]["run_id"], "run-lifecycle-owned");
    assert!(fixture.state().join("runtime.json").is_file());
    assert!(!repo.path().join(".nopal/rondo-runtime.json").exists());

    let duplicate = fixture.nopal(&args);
    assert_eq!(
        duplicate.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&duplicate)
    );
    let duplicate_doc = json(&duplicate);
    assert_eq!(duplicate_doc["deduplicated"], true);
    assert_eq!(duplicate_doc["handle"]["run_id"], "run-lifecycle-owned");

    let repo_id = duplicate_doc["handle"]["repo_id"].as_str().unwrap();
    let observed = fixture.nopal(&[
        "--dir",
        repo.path().to_str().unwrap(),
        "--json",
        "run",
        "observe",
        "--repo-id",
        repo_id,
        "--plot-id",
        TEST_PLOT_ID,
        "--run-id",
        "run-lifecycle-owned",
        "--state-dir",
        plot_state.to_str().unwrap(),
        "--cursor",
        "rondo.core/v1:0",
    ]);
    assert_eq!(
        observed.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&observed)
    );
    let observed_doc = json(&observed);
    assert_eq!(observed_doc["status"], "completed");
    assert_eq!(observed_doc["settled"], true);
}

#[test]
fn run_submit_uses_environment_endpoint_override() {
    let temp = tempfile::tempdir().unwrap();
    let plot_state = tempfile::tempdir().unwrap();
    write_established_plot_state(plot_state.path());
    write_portable_project(temp.path());
    fs::write(
        temp.path().join(".nopal/policy.jsonc"),
        r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "nopal_tui": {
      "default_decision": "allow",
      "default_placement": "dedicated_run_runtime",
      "rules": []
    }
  }
}
"#,
    )
    .unwrap();
    let manifest = temp.path().join("approved-slice.json");
    fs::write(&manifest, b"exact approved bytes\n").unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args([
            "--dir",
            temp.path().to_str().unwrap(),
            "--json",
            "run",
            "submit",
            "--manifest",
            manifest.to_str().unwrap(),
            "--plot-id",
            TEST_PLOT_ID,
            "--state-dir",
            plot_state.path().to_str().unwrap(),
        ])
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .env("NOPAL_RONDO_CORE_URL", "https://not-loopback.example")
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.run_submit/v1");
    assert!(
        doc["diagnostics"][0]
            .as_str()
            .unwrap()
            .contains("literal loopback HTTP origin")
    );
    assert!(!stdout(&out).contains("not-loopback.example"));
}

#[test]
fn production_cli_rejects_unrepresentable_rondo_timeout_as_structured_failure() {
    let temp = tempfile::tempdir().unwrap();
    let plot_state = tempfile::tempdir().unwrap();
    write_established_plot_state(plot_state.path());
    write_portable_project(temp.path());
    fs::write(
        temp.path().join(".nopal/policy.jsonc"),
        r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "nopal_tui": {
      "default_decision": "allow",
      "default_placement": "dedicated_run_runtime",
      "rules": []
    }
  }
}
"#,
    )
    .unwrap();
    fs::write(
        temp.path().join(".nopal/config.jsonc"),
        r#"{
  "version": "nopal.config/v1",
  "rondo_core": {
    "base_url": "http://127.0.0.1:1",
    "request_timeout_ms": 18446744073709551615,
    "repo_id": "test-repo"
  }
}
"#,
    )
    .unwrap();
    let manifest = temp.path().join("approved-slice.json");
    fs::write(&manifest, b"exact approved bytes\n").unwrap();

    let out = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "submit",
        "--manifest",
        manifest.to_str().unwrap(),
        "--plot-id",
        TEST_PLOT_ID,
        "--state-dir",
        plot_state.path().to_str().unwrap(),
    ]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.run_submit/v1");
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["submitted"], false);
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|message| message
                .as_str()
                .is_some_and(|message| message.contains("representable by the clock"))),
        "unexpected diagnostics: {}",
        doc["diagnostics"]
    );
}

#[test]
fn production_cli_submits_deduplicates_and_observes_through_loopback_http() {
    let server = ScriptedHttpServer::start(vec![
        (
            200,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "runtime_version": "0.1.0",
                "instance_id": "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
                "service_mode": "trackerless_core",
                "ready": true,
                "active_run_count": 0
            }),
        ),
        (
            202,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "service_id": "rondo-core",
                "repo_id": "test-repo",
                "plot_id": TEST_PLOT_ID,
                "run_id": "run-accepted-once",
                "status": "running",
                "event_cursor": "rondo.core/v1:0",
                "deduplicated": false
            }),
        ),
        (
            200,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "runtime_version": "0.1.0",
                "instance_id": "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
                "service_mode": "trackerless_core",
                "ready": true,
                "active_run_count": 1
            }),
        ),
        (
            200,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "service_id": "rondo-core",
                "repo_id": "test-repo",
                "plot_id": TEST_PLOT_ID,
                "run_id": "run-accepted-once",
                "status": "running",
                "event_cursor": "rondo.core/v1:0",
                "deduplicated": true
            }),
        ),
        (
            200,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "repo_id": "test-repo",
                "plot_id": TEST_PLOT_ID,
                "run_id": "run-accepted-once",
                "status": "completed",
                "last_event": {"type": "run.completed"},
                "evidence_pointers": [
                    {
                        "artifact_kind": "final_report",
                        "uri": "rondo-run://run-accepted-once/artifacts/final-report.json"
                    }
                ],
                "event_cursor": "rondo.core/v1:1"
            }),
        ),
        (
            200,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "repo_id": "test-repo",
                "plot_id": TEST_PLOT_ID,
                "run_id": "run-accepted-once",
                "events": [{
                    "type": "rondo.run.evidence_recorded",
                    "sequence": 1,
                    "repo_id": "test-repo",
                    "plot_id": TEST_PLOT_ID,
                    "run_id": "run-accepted-once",
                    "artifact_kind": "agent_events",
                    "uri": "rondo-run://run-accepted-once/artifacts/agent-events.ndjson",
                    "namespace": {
                        "repo_id": "test-repo",
                        "plot_id": TEST_PLOT_ID,
                        "run_id": "run-accepted-once"
                    }
                }],
                "next_event_cursor": "rondo.core/v1:1",
                "has_more": false
            }),
        ),
        (
            200,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "repo_id": "test-repo",
                "plot_id": TEST_PLOT_ID,
                "run_id": "run-accepted-once",
                "status": "completed",
                "last_event": {"type": "run.completed"},
                "evidence_pointers": [
                    {
                        "artifact_kind": "final_report",
                        "uri": "rondo-run://run-accepted-once/artifacts/final-report.json"
                    }
                ],
                "event_cursor": "rondo.core/v1:0"
            }),
        ),
        (
            200,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "repo_id": "test-repo",
                "plot_id": TEST_PLOT_ID,
                "run_id": "run-accepted-once",
                "events": [],
                "next_event_cursor": "rondo.core/v1:1",
                "has_more": false
            }),
        ),
        (
            200,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "repo_id": "test-repo",
                "plot_id": TEST_PLOT_ID,
                "run_id": "run-accepted-once",
                "status": "failed",
                "last_event": null,
                "evidence_pointers": [],
                "event_cursor": "rondo.core/v1:0"
            }),
        ),
    ]);
    let temp = tempfile::tempdir().unwrap();
    let plot_state = tempfile::tempdir().unwrap();
    write_established_plot_state(plot_state.path());
    write_portable_project(temp.path());
    fs::write(
        temp.path().join(".nopal/policy.jsonc"),
        r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "nopal_tui": {
      "default_decision": "allow",
      "default_placement": "dedicated_run_runtime",
      "rules": []
    }
  }
}

"#,
    )
    .unwrap();
    fs::write(
        temp.path().join(".nopal/config.jsonc"),
        format!(
            r#"{{
  "version": "nopal.config/v1",
  "rondo_core": {{
    "base_url": {:?},
    "request_timeout_ms": 1000,
    "repo_id": "test-repo"
  }}
}}
"#,
            server.base_url
        ),
    )
    .unwrap();
    let manifest = temp.path().join("approved-slice.json");
    fs::write(&manifest, b"harmless approved fixture bytes\n").unwrap();
    let submit_args = [
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "submit",
        "--manifest",
        manifest.to_str().unwrap(),
        "--plot-id",
        TEST_PLOT_ID,
        "--state-dir",
        plot_state.path().to_str().unwrap(),
    ];

    let first = nopal(&submit_args);
    assert_eq!(first.status.code(), Some(0), "stderr: {}", stderr(&first));
    let first_doc = json(&first);
    assert_eq!(first_doc["ok"], true);
    assert_eq!(first_doc["submitted"], true);
    assert_eq!(first_doc["deduplicated"], false);
    assert_eq!(first_doc["handle"]["run_id"], "run-accepted-once");
    let stored_plot: serde_json::Value = serde_json::from_slice(
        &fs::read(
            plot_state
                .path()
                .join("plots")
                .join(format!("{TEST_PLOT_ID}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(stored_plot["executions"].as_array().unwrap().len(), 1);
    assert_eq!(stored_plot["executions"][0]["service_id"], "rondo-core");
    assert_eq!(stored_plot["executions"][0]["repo_id"], "test-repo");
    assert_eq!(stored_plot["executions"][0]["run_id"], "run-accepted-once");
    assert_eq!(stored_plot["executions"][0]["status"], "running");
    assert_eq!(
        stored_plot["executions"][0]["event_cursor"],
        "rondo.core/v1:0"
    );

    let duplicate = nopal(&submit_args);
    assert_eq!(
        duplicate.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&duplicate)
    );
    let duplicate_doc = json(&duplicate);
    assert_eq!(duplicate_doc["deduplicated"], true);
    assert_eq!(duplicate_doc["handle"]["run_id"], "run-accepted-once");
    let replayed_plot: serde_json::Value = serde_json::from_slice(
        &fs::read(
            plot_state
                .path()
                .join("plots")
                .join(format!("{TEST_PLOT_ID}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(replayed_plot["executions"].as_array().unwrap().len(), 1);

    let observed = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "observe",
        "--repo-id",
        "test-repo",
        "--plot-id",
        TEST_PLOT_ID,
        "--run-id",
        "run-accepted-once",
        "--state-dir",
        plot_state.path().to_str().unwrap(),
        "--cursor",
        "rondo.core/v1:0",
    ]);
    assert_eq!(
        observed.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&observed)
    );
    let observed_doc = json(&observed);
    assert_eq!(observed_doc["status"], "completed");
    assert_eq!(observed_doc["settled"], true);
    assert_eq!(observed_doc["has_more"], false);
    assert_eq!(
        observed_doc["evidence_pointers"][0]["uri"],
        "rondo-run://run-accepted-once/artifacts/final-report.json"
    );
    let observed_plot: serde_json::Value = serde_json::from_slice(
        &fs::read(
            plot_state
                .path()
                .join("plots")
                .join(format!("{TEST_PLOT_ID}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(observed_plot["executions"].as_array().unwrap().len(), 1);
    assert_eq!(observed_plot["executions"][0]["status"], "completed");
    assert_eq!(observed_plot["executions"][0]["outcome"], "completed");
    assert_eq!(
        observed_plot["executions"][0]["event_cursor"],
        "rondo.core/v1:1"
    );
    assert_eq!(
        observed_plot["executions"][0]["evidence"][0]["artifact_kind"],
        "final_report"
    );
    assert_eq!(
        observed_plot["executions"][0]["evidence"][1]["artifact_kind"],
        "agent_events"
    );

    let resumed = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "observe",
        "--repo-id",
        "test-repo",
        "--plot-id",
        TEST_PLOT_ID,
        "--run-id",
        "run-accepted-once",
        "--state-dir",
        plot_state.path().to_str().unwrap(),
    ]);
    assert_eq!(
        resumed.status.code(),
        Some(0),
        "stderr: {}",
        stderr(&resumed)
    );
    assert_eq!(json(&resumed)["next_event_cursor"], "rondo.core/v1:1");

    let skipped = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "observe",
        "--repo-id",
        "test-repo",
        "--plot-id",
        TEST_PLOT_ID,
        "--run-id",
        "run-accepted-once",
        "--state-dir",
        plot_state.path().to_str().unwrap(),
        "--cursor",
        "rondo.core/v1:2",
    ]);
    assert_eq!(skipped.status.code(), Some(1));
    assert!(
        json(&skipped)["diagnostics"][0]
            .as_str()
            .unwrap()
            .contains("cannot skip ahead")
    );

    let conflicted = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "observe",
        "--repo-id",
        "test-repo",
        "--plot-id",
        TEST_PLOT_ID,
        "--run-id",
        "run-accepted-once",
        "--state-dir",
        plot_state.path().to_str().unwrap(),
    ]);
    assert_eq!(conflicted.status.code(), Some(1));
    let conflicted_report = json(&conflicted);
    assert_eq!(conflicted_report["status"], "failed");
    assert_eq!(
        conflicted_report["next_event_cursor"],
        serde_json::Value::Null
    );
    assert!(
        conflicted_report["diagnostics"][0]
            .as_str()
            .unwrap()
            .contains("could not update its durable Plot")
    );
    let unchanged_plot: serde_json::Value = serde_json::from_slice(
        &fs::read(
            plot_state
                .path()
                .join("plots")
                .join(format!("{TEST_PLOT_ID}.json")),
        )
        .unwrap(),
    )
    .unwrap();
    assert_eq!(unchanged_plot["executions"][0]["outcome"], "completed");

    let requests = server.finish(9);
    assert!(requests[0].starts_with("GET /api/v1/health HTTP/1.1"));
    assert!(requests[1].starts_with("POST /api/v1/execution-requests HTTP/1.1"));
    assert!(requests[2].starts_with("GET /api/v1/health HTTP/1.1"));
    assert!(requests[3].starts_with("POST /api/v1/execution-requests HTTP/1.1"));
    assert!(
        requests[4].starts_with("GET /api/v1/runs/run-accepted-once?repo_id=test-repo HTTP/1.1")
    );
    assert!(requests[5].starts_with(
        "GET /api/v1/runs/run-accepted-once/events?repo_id=test-repo&cursor=rondo.core%2Fv1%3A0 HTTP/1.1"
    ));
    assert!(requests[7].starts_with(
        "GET /api/v1/runs/run-accepted-once/events?repo_id=test-repo&cursor=rondo.core%2Fv1%3A1 HTTP/1.1"
    ));
    let first_body = requests[1].split("\r\n\r\n").nth(1).unwrap();
    let duplicate_body = requests[3].split("\r\n\r\n").nth(1).unwrap();
    assert_eq!(first_body, duplicate_body);
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(first_body).unwrap()["plot_id"],
        TEST_PLOT_ID
    );
}

#[test]
fn accepted_rondo_run_reports_an_honest_handle_when_plot_attachment_conflicts() {
    let server = ScriptedHttpServer::start(vec![
        (
            200,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "runtime_version": "0.1.0",
                "instance_id": "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
                "service_mode": "trackerless_core",
                "ready": true,
                "active_run_count": 0
            }),
        ),
        (
            202,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "service_id": "rondo-core",
                "repo_id": "test-repo",
                "plot_id": TEST_PLOT_ID,
                "run_id": "run-conflict",
                "status": "running",
                "event_cursor": "rondo.core/v1:0",
                "deduplicated": false
            }),
        ),
    ]);
    let repo = tempfile::tempdir().unwrap();
    let plot_state = tempfile::tempdir().unwrap();
    write_established_plot_state(plot_state.path());
    let plot_path = plot_state
        .path()
        .join("plots")
        .join(format!("{TEST_PLOT_ID}.json"));
    let mut plot: serde_json::Value =
        serde_json::from_slice(&fs::read(&plot_path).unwrap()).unwrap();
    plot["executions"] = serde_json::json!([{
        "service_id": "rondo-core",
        "repo_id": "test-repo",
        "run_id": "run-conflict",
        "manifest_sha256": "b".repeat(64),
        "status": "running",
        "outcome": null,
        "event_cursor": "rondo.core/v1:0",
        "evidence": [],
        "created_at": "2026-07-12T00:00:00Z",
        "updated_at": "2026-07-12T00:00:00Z"
    }]);
    fs::write(&plot_path, serde_json::to_vec_pretty(&plot).unwrap()).unwrap();
    write_portable_project(repo.path());
    fs::write(
        repo.path().join(".nopal/policy.jsonc"),
        r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "nopal_tui": {
      "default_decision": "allow",
      "default_placement": "dedicated_run_runtime",
      "rules": []
    }
  }
}
"#,
    )
    .unwrap();
    fs::write(
        repo.path().join(".nopal/config.jsonc"),
        format!(
            r#"{{
  "version": "nopal.config/v1",
  "rondo_core": {{
    "base_url": {:?},
    "request_timeout_ms": 1000,
    "repo_id": "test-repo"
  }}
}}
"#,
            server.base_url
        ),
    )
    .unwrap();
    let manifest = repo.path().join("approved-slice.json");
    fs::write(&manifest, b"different accepted manifest bytes\n").unwrap();

    let out = nopal(&[
        "--dir",
        repo.path().to_str().unwrap(),
        "--json",
        "run",
        "submit",
        "--manifest",
        manifest.to_str().unwrap(),
        "--plot-id",
        TEST_PLOT_ID,
        "--state-dir",
        plot_state.path().to_str().unwrap(),
    ]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let report = json(&out);
    assert_eq!(report["ok"], false);
    assert_eq!(report["submitted"], true);
    assert_eq!(report["handle"]["run_id"], "run-conflict");
    assert!(
        report["diagnostics"][0]
            .as_str()
            .unwrap()
            .contains("could not attach")
    );
    assert!(
        !report
            .to_string()
            .contains(plot_state.path().to_str().unwrap())
    );
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&fs::read(plot_path).unwrap()).unwrap()["executions"]
            [0]["manifest_sha256"],
        "b".repeat(64)
    );
    let _requests = server.finish(2);
}

#[test]
fn validated_status_persists_when_the_following_event_page_fails() {
    let server = ScriptedHttpServer::start(vec![
        (
            200,
            serde_json::json!({
                "surface": "rondo.core/v1",
                "repo_id": "test-repo",
                "plot_id": TEST_PLOT_ID,
                "run_id": "run-partial",
                "status": "completed",
                "last_event": null,
                "evidence_pointers": [{
                    "artifact_kind": "final_report",
                    "uri": "rondo-run://run-partial/artifacts/final-report.json"
                }],
                "event_cursor": "rondo.core/v1:0"
            }),
        ),
        (
            503,
            serde_json::json!({
                "error": {
                    "code": "core_unavailable",
                    "message": "safe unavailable message"
                }
            }),
        ),
    ]);
    let repo = tempfile::tempdir().unwrap();
    let plot_state = tempfile::tempdir().unwrap();
    write_established_plot_state(plot_state.path());
    let plot_path = plot_state
        .path()
        .join("plots")
        .join(format!("{TEST_PLOT_ID}.json"));
    let mut plot: serde_json::Value =
        serde_json::from_slice(&fs::read(&plot_path).unwrap()).unwrap();
    plot["executions"] = serde_json::json!([{
        "service_id": "rondo-core",
        "repo_id": "test-repo",
        "run_id": "run-partial",
        "manifest_sha256": "a".repeat(64),
        "status": "running",
        "outcome": null,
        "event_cursor": "rondo.core/v1:0",
        "evidence": [],
        "created_at": "2026-07-12T00:00:00Z",
        "updated_at": "2026-07-12T00:00:00Z"
    }]);
    fs::write(&plot_path, serde_json::to_vec_pretty(&plot).unwrap()).unwrap();
    write_portable_project(repo.path());
    fs::write(
        repo.path().join(".nopal/config.jsonc"),
        format!(
            r#"{{
  "version": "nopal.config/v1",
  "rondo_core": {{
    "base_url": {:?},
    "request_timeout_ms": 1000,
    "repo_id": "test-repo"
  }}
}}
"#,
            server.base_url
        ),
    )
    .unwrap();

    let out = nopal(&[
        "--dir",
        repo.path().to_str().unwrap(),
        "--json",
        "run",
        "observe",
        "--repo-id",
        "test-repo",
        "--plot-id",
        TEST_PLOT_ID,
        "--run-id",
        "run-partial",
        "--state-dir",
        plot_state.path().to_str().unwrap(),
    ]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let report = json(&out);
    assert_eq!(report["ok"], false);
    assert_eq!(report["status"], "completed");
    assert_eq!(
        report["evidence_pointers"][0]["artifact_kind"],
        "final_report"
    );
    assert!(
        report["diagnostics"][0]
            .as_str()
            .unwrap()
            .contains("event observation failed")
    );
    let stored: serde_json::Value = serde_json::from_slice(&fs::read(plot_path).unwrap()).unwrap();
    assert_eq!(stored["executions"][0]["status"], "completed");
    assert_eq!(stored["executions"][0]["outcome"], "completed");
    assert_eq!(stored["executions"][0]["event_cursor"], "rondo.core/v1:0");
    assert_eq!(
        stored["executions"][0]["evidence"][0]["artifact_kind"],
        "final_report"
    );
    let _requests = server.finish(2);
}

#[test]
fn production_cli_rejects_incompatible_explicit_core_before_submission() {
    let server = ScriptedHttpServer::start(vec![(
        200,
        serde_json::json!({
            "surface": "rondo.core/v1",
            "runtime_version": "99.0.0",
            "instance_id": "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
            "service_mode": "trackerless_core",
            "ready": true,
            "active_run_count": 0
        }),
    )]);
    let temp = tempfile::tempdir().unwrap();
    let state = tempfile::tempdir().unwrap();
    let plot_state = tempfile::tempdir().unwrap();
    write_established_plot_state(plot_state.path());
    write_portable_project(temp.path());
    fs::write(
        temp.path().join(".nopal/policy.jsonc"),
        r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "nopal_tui": {
      "default_decision": "allow",
      "default_placement": "dedicated_run_runtime",
      "rules": []
    }
  }
}
"#,
    )
    .unwrap();
    let manifest = temp.path().join("approved-slice.json");
    fs::write(&manifest, b"approved explicit fixture bytes\n").unwrap();

    let out = nopal_command()
        .args([
            "--dir",
            temp.path().to_str().unwrap(),
            "--json",
            "run",
            "submit",
            "--manifest",
            manifest.to_str().unwrap(),
            "--plot-id",
            TEST_PLOT_ID,
            "--state-dir",
            plot_state.path().to_str().unwrap(),
        ])
        .env("NOPAL_CONFIG_DIR", isolated_config_dir())
        .env("NOPAL_RONDO_CORE_URL", &server.base_url)
        .env("NOPAL_RONDO_STATE_DIR", state.path())
        .output()
        .unwrap();

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["submitted"], false);
    assert!(
        doc["diagnostics"][0]
            .as_str()
            .unwrap()
            .contains("runtime version is incompatible")
    );
    assert!(!state.path().join("runtime.json").exists());
    let requests = server.finish(1);
    assert!(requests[0].starts_with("GET /api/v1/health HTTP/1.1"));
}

#[cfg(unix)]
#[test]
fn denied_submission_does_not_start_the_automatic_lifecycle() {
    let fixture = LifecycleFixture::new();
    let repo = tempfile::tempdir().unwrap();
    write_portable_project(repo.path());
    let manifest = repo.path().join("approved-slice.json");
    fs::write(&manifest, b"approved denied fixture bytes\n").unwrap();

    let out = fixture.nopal(&[
        "--dir",
        repo.path().to_str().unwrap(),
        "--json",
        "run",
        "submit",
        "--manifest",
        manifest.to_str().unwrap(),
        "--plot-id",
        TEST_PLOT_ID,
        "--state-dir",
        fixture.plot_state().to_str().unwrap(),
    ]);

    assert_eq!(out.status.code(), Some(1), "stderr: {}", stderr(&out));
    assert_eq!(json(&out)["submitted"], false);
    assert!(!fixture.state().join("runtime.json").exists());
}

#[test]
fn run_observe_dispatches_sanitized_failure_before_network_contact() {
    let temp = tempfile::tempdir().unwrap();
    write_portable_project(temp.path());
    let oversized_repo_id = "é".repeat(257);

    let out = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "run",
        "observe",
        "--repo-id",
        &oversized_repo_id,
        "--plot-id",
        TEST_PLOT_ID,
        "--run-id",
        "run-1",
    ]);

    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.run_observation/v1");
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["handle"]["repo_id"], "-");
    assert_eq!(doc["handle"]["run_id"], "-");
    assert_eq!(doc["has_more"], false);
    assert_eq!(doc["settled"], false);
    assert!(
        doc["diagnostics"][0]
            .as_str()
            .unwrap()
            .contains("512 UTF-8 bytes")
    );
    assert!(!stdout(&out).contains(&oversized_repo_id));
}

#[test]
fn empty_config_dir_env_is_unset_and_never_reads_a_cwd_relative_template() {
    // `NOPAL_CONFIG_DIR=` (set but empty) must mean UNSET, not "current
    // directory": without the filter in `resolve_config_dir`, the template
    // lookup becomes the bare relative path `bundle-default.jsonc`, and a
    // file by that name in whatever repo nopal runs from would silently
    // become the user's standing template.
    // HOME is pointed at an empty temp dir so the fallback
    // `~/.config/nopal` cannot leak a real template from the dev machine.
    let temp = tempfile::tempdir().unwrap();
    let fake_home = tempfile::tempdir().unwrap();
    std::fs::write(
        temp.path().join("bundle-default.jsonc"),
        "{ \"version\": \"nopal.bundle/v1\", \"inherit_ambient\": [\"themes\"] }",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", ".", "cli", "--dry-run", "--json"])
        .current_dir(temp.path())
        .env("NOPAL_CONFIG_DIR", "")
        .env("HOME", fake_home.path())
        .output()
        .expect("failed to spawn nopal binary");

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["scaffold"], "would_create");
    // Hermetic fallback won, not the cwd-relative file's ["themes"].
    assert_eq!(doc["ambient_kinds"].as_array().unwrap().len(), 0);
    let diag_text = doc["diagnostics"].to_string();
    assert!(
        diag_text.contains("built-in hermetic defaults"),
        "diagnostics should name the hermetic source: {diag_text}"
    );
    assert!(
        !diag_text.contains("bundle-default.jsonc"),
        "cwd-relative template must not be consulted: {diag_text}"
    );
}

#[test]
fn legacy_product_config_dir_is_ignored() {
    let temp = tempfile::tempdir().unwrap();
    let fake_home = tempfile::tempdir().unwrap();
    let legacy_config = tempfile::tempdir().unwrap();
    std::fs::write(
        legacy_config.path().join("bundle-default.jsonc"),
        "{ \"version\": \"nopal.bundle/v1\", \"inherit_ambient\": [\"themes\"] }",
    )
    .unwrap();

    let out = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(["--dir", ".", "cli", "--dry-run", "--json"])
        .current_dir(temp.path())
        .env_remove("NOPAL_CONFIG_DIR")
        .env("CRUST_CONFIG_DIR", legacy_config.path())
        .env("HOME", fake_home.path())
        .output()
        .expect("failed to spawn nopal binary");

    assert!(out.status.success(), "stderr: {}", stderr(&out));
    let doc = json(&out);
    assert_eq!(doc["scaffold"], "would_create");
    assert_eq!(doc["ambient_kinds"].as_array().unwrap().len(), 0);
}
