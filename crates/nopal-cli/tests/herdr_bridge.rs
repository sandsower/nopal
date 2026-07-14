// Unix socket tests require the host sandbox to allow AF_UNIX bind/connect.
#![allow(clippy::unwrap_used)]

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

fn nopal(args: &[&str], state: &Path) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(args)
        .env("BEISLID_STATE_DIR", state)
        .output()
        .unwrap()
}

fn read_request(reader: &mut BufReader<UnixStream>) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

fn reply(reader: &mut BufReader<UnixStream>, value: serde_json::Value) {
    serde_json::to_writer(reader.get_mut(), &value).unwrap();
    reader.get_mut().write_all(b"\n").unwrap();
    reader.get_mut().flush().unwrap();
}

fn fake_server(
    socket: &Path,
    repo: PathBuf,
) -> (
    thread::JoinHandle<()>,
    std::sync::mpsc::Receiver<serde_json::Value>,
) {
    fake_server_with_panes(
        socket,
        vec![serde_json::json!({
            "pane_id": "w1:p1",
            "cwd": repo,
            "foreground_cwd": repo.join("crates/nopal-cli")
        })],
    )
}

fn fake_server_with_panes(
    socket: &Path,
    panes: Vec<serde_json::Value>,
) -> (
    thread::JoinHandle<()>,
    std::sync::mpsc::Receiver<serde_json::Value>,
) {
    let listener = UnixListener::bind(socket).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        let mut reader = BufReader::new(stream);

        let ping = read_request(&mut reader);
        reply(
            &mut reader,
            serde_json::json!({
                "id": ping["id"],
                "result": {"type": "pong", "version": "0.7.3", "protocol": 16}
            }),
        );

        let snapshot = read_request(&mut reader);
        reply(
            &mut reader,
            serde_json::json!({
                "id": snapshot["id"],
                "result": {
                    "type": "session_snapshot",
                    "snapshot": {
                        "protocol": 16,
                        "panes": panes
                    }
                }
            }),
        );

        let report = read_request(&mut reader);
        tx.send(report.clone()).unwrap();
        reply(
            &mut reader,
            serde_json::json!({"id": report["id"], "result": {"type": "ok"}}),
        );
    });
    (handle, rx)
}

fn bridge_once(repo: &Path, state: &Path, socket: &Path) -> Output {
    nopal(
        &[
            "--dir",
            repo.to_str().unwrap(),
            "bridge",
            "herdr",
            "--socket",
            socket.to_str().unwrap(),
            "--once",
        ],
        state,
    )
}

fn accept_until(listener: &UnixListener, timeout: Duration) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                return stream;
            }
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(
                    Instant::now() < deadline,
                    "bridge did not reconnect in time"
                );
                thread::sleep(Duration::from_millis(10));
            }
            Err(err) => panic!("failed to accept bridge connection: {err}"),
        }
    }
}

#[test]
fn once_bridges_a_real_ledger_run_to_a_fake_herdr_server() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(repo.join("crates/nopal-cli")).unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();
    let socket = tmp.path().join("herdr.sock");

    let init = nopal(
        &[
            "--dir",
            repo.to_str().unwrap(),
            "ledger",
            "--state-dir",
            state.to_str().unwrap(),
            "init",
            "--skill",
            "implement",
            "--branch",
            "codex/task-44",
            "--run-id",
            "run-1",
        ],
        &state,
    );
    assert_eq!(init.status.code(), Some(0));

    let (server, reports) = fake_server(&socket, repo.clone());
    let bridge = nopal(
        &[
            "--dir",
            repo.to_str().unwrap(),
            "bridge",
            "herdr",
            "--socket",
            socket.to_str().unwrap(),
            "--once",
        ],
        &state,
    );

    assert_eq!(
        bridge.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&bridge.stderr)
    );
    let report = reports.recv().unwrap();
    assert_eq!(report["method"], "pane.report_agent");
    assert_eq!(report["params"]["pane_id"], "w1:p1");
    assert_eq!(report["params"]["state"], "working");
    assert_eq!(report["params"]["custom_status"], "run:running");
    server.join().unwrap();
}

#[test]
fn explicit_state_dir_beats_the_environment_for_the_child_field_query() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let explicit_state = tmp.path().join("explicit-state");
    let env_state = tmp.path().join("env-state");
    std::fs::create_dir_all(repo.join("crates/nopal-cli")).unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();
    let socket = tmp.path().join("herdr.sock");

    let init = nopal(
        &[
            "--dir",
            repo.to_str().unwrap(),
            "ledger",
            "--state-dir",
            explicit_state.to_str().unwrap(),
            "init",
            "--skill",
            "implement",
            "--branch",
            "codex/task-44",
            "--run-id",
            "run-explicit",
        ],
        &env_state,
    );
    assert_eq!(init.status.code(), Some(0));

    let (server, mutations) = fake_server(&socket, repo.clone());
    let bridge = nopal(
        &[
            "--dir",
            repo.to_str().unwrap(),
            "bridge",
            "herdr",
            "--state-dir",
            explicit_state.to_str().unwrap(),
            "--socket",
            socket.to_str().unwrap(),
            "--once",
        ],
        &env_state,
    );

    assert_eq!(
        bridge.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&bridge.stderr)
    );
    let report = mutations.recv().unwrap();
    assert_eq!(report["method"], "pane.report_agent");
    assert_eq!(report["params"]["pane_id"], "w1:p1");
    assert_eq!(report["params"]["custom_status"], "run:running");
    server.join().unwrap();
}

#[test]
fn separate_once_invocation_releases_a_current_pane_after_its_run_disappears() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let active_state = tmp.path().join("active-state");
    let empty_state = tmp.path().join("empty-state");
    std::fs::create_dir_all(repo.join("crates/nopal-cli")).unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();

    let init = nopal(
        &[
            "--dir",
            repo.to_str().unwrap(),
            "ledger",
            "--state-dir",
            active_state.to_str().unwrap(),
            "init",
            "--skill",
            "implement",
            "--branch",
            "codex/task-44",
            "--run-id",
            "run-1",
        ],
        &active_state,
    );
    assert_eq!(init.status.code(), Some(0));

    let first_socket = tmp.path().join("first.sock");
    let (first_server, first_mutations) = fake_server(&first_socket, repo.clone());
    assert_eq!(
        bridge_once(&repo, &active_state, &first_socket)
            .status
            .code(),
        Some(0)
    );
    assert_eq!(
        first_mutations.recv().unwrap()["method"],
        "pane.report_agent"
    );
    first_server.join().unwrap();

    let second_socket = tmp.path().join("second.sock");
    let (second_server, second_mutations) = fake_server(&second_socket, repo.clone());
    let second = bridge_once(&repo, &empty_state, &second_socket);
    assert_eq!(
        second.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );
    let release = second_mutations.recv().unwrap();
    assert_eq!(release["method"], "pane.release_agent");
    assert_eq!(release["params"]["pane_id"], "w1:p1");
    assert_eq!(release["params"]["source"], "custom:nopal");
    assert_eq!(release["params"]["agent"], "nopal");
    second_server.join().unwrap();
}

#[test]
fn restarted_bridge_reconciles_only_panes_in_the_new_snapshot() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let active_state = tmp.path().join("active-state");
    let empty_state = tmp.path().join("empty-state");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();

    let init = nopal(
        &[
            "--dir",
            repo.to_str().unwrap(),
            "ledger",
            "--state-dir",
            active_state.to_str().unwrap(),
            "init",
            "--skill",
            "implement",
            "--branch",
            "codex/task-44",
            "--run-id",
            "run-1",
        ],
        &active_state,
    );
    assert_eq!(init.status.code(), Some(0));

    let first_socket = tmp.path().join("first.sock");
    let (first_server, first_mutations) = fake_server_with_panes(
        &first_socket,
        vec![serde_json::json!({"pane_id": "gone", "cwd": repo})],
    );
    assert_eq!(
        bridge_once(&repo, &active_state, &first_socket)
            .status
            .code(),
        Some(0)
    );
    assert_eq!(first_mutations.recv().unwrap()["params"]["pane_id"], "gone");
    first_server.join().unwrap();

    let second_socket = tmp.path().join("second.sock");
    let (second_server, second_mutations) = fake_server_with_panes(
        &second_socket,
        vec![serde_json::json!({"pane_id": "current", "cwd": repo})],
    );
    assert_eq!(
        bridge_once(&repo, &empty_state, &second_socket)
            .status
            .code(),
        Some(0)
    );
    let release = second_mutations.recv().unwrap();
    assert_eq!(release["method"], "pane.release_agent");
    assert_eq!(release["params"]["pane_id"], "current");
    second_server.join().unwrap();
}

#[test]
fn daemon_survives_pane_churn_between_snapshot_and_report_then_polls_again() {
    let tmp = tempfile::tempdir().unwrap();
    let repo = tmp.path().join("repo");
    let state = tmp.path().join("state");
    std::fs::create_dir_all(&repo).unwrap();
    let repo = std::fs::canonicalize(repo).unwrap();
    let socket = tmp.path().join("herdr.sock");

    let init = nopal(
        &[
            "--dir",
            repo.to_str().unwrap(),
            "ledger",
            "--state-dir",
            state.to_str().unwrap(),
            "init",
            "--skill",
            "implement",
            "--branch",
            "codex/task-44",
            "--run-id",
            "run-1",
        ],
        &state,
    );
    assert_eq!(init.status.code(), Some(0));

    let listener = UnixListener::bind(&socket).unwrap();
    let (tx, rx) = std::sync::mpsc::channel();
    let server_repo = repo.clone();
    let server = thread::spawn(move || {
        for poll in 0..2 {
            let stream = accept_until(&listener, Duration::from_secs(3));
            let mut reader = BufReader::new(stream);

            let ping = read_request(&mut reader);
            reply(
                &mut reader,
                serde_json::json!({
                    "id": ping["id"],
                    "result": {"type": "pong", "version": "0.7.3", "protocol": 16}
                }),
            );

            let snapshot = read_request(&mut reader);
            reply(
                &mut reader,
                serde_json::json!({
                    "id": snapshot["id"],
                    "result": {
                        "type": "session_snapshot",
                        "snapshot": {
                            "protocol": 16,
                            "panes": [{"pane_id": "churned", "cwd": server_repo}]
                        }
                    }
                }),
            );

            let report = read_request(&mut reader);
            assert_eq!(report["method"], "pane.report_agent");
            if poll == 0 {
                reply(
                    &mut reader,
                    serde_json::json!({
                        "id": report["id"],
                        "error": {"code": "pane_not_found", "message": "pane closed"}
                    }),
                );
            } else {
                reply(
                    &mut reader,
                    serde_json::json!({"id": report["id"], "result": {"type": "ok"}}),
                );
                tx.send(()).unwrap();
            }
        }
    });

    let mut bridge = Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args([
            "--dir",
            repo.to_str().unwrap(),
            "bridge",
            "herdr",
            "--socket",
            socket.to_str().unwrap(),
            "--interval",
            "1",
        ])
        .env("BEISLID_STATE_DIR", &state)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();

    let reached_second_poll = rx.recv_timeout(Duration::from_secs(4));
    let early_exit = bridge.try_wait().unwrap();
    if early_exit.is_none() {
        bridge.kill().unwrap();
    }
    bridge.wait().unwrap();
    let server_result = server.join();

    assert!(
        reached_second_poll.is_ok(),
        "bridge exited before a successful later poll: {early_exit:?}; server: {server_result:?}"
    );
    assert!(
        early_exit.is_none(),
        "daemon exited after recoverable pane churn"
    );
    server_result.unwrap();
}

#[test]
fn once_treats_a_missing_socket_as_a_successful_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let socket = tmp.path().join("absent.sock");
    let out = nopal(
        &[
            "bridge",
            "herdr",
            "--socket",
            socket.to_str().unwrap(),
            "--once",
        ],
        &state,
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(out.stdout.is_empty());
    assert!(out.stderr.is_empty());
}

#[test]
fn malformed_protocol_is_an_observable_failure() {
    let tmp = tempfile::tempdir().unwrap();
    let state = tmp.path().join("state");
    let socket = tmp.path().join("herdr.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut line = String::new();
        BufReader::new(stream.try_clone().unwrap())
            .read_line(&mut line)
            .unwrap();
        stream.write_all(b"not json\n").unwrap();
    });

    let out = nopal(
        &[
            "bridge",
            "herdr",
            "--socket",
            socket.to_str().unwrap(),
            "--once",
        ],
        &state,
    );
    assert_eq!(out.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid herdr response for ping"));
    server.join().unwrap();
}
