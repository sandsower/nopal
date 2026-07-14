#![allow(clippy::unwrap_used)]

use std::fs;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::thread;

use nopal_rondo_lifecycle::{RuntimeDescriptor, StatePaths, health, stop};
use serde_json::json;

#[test]
fn state_paths_are_user_scoped_and_never_repository_local() {
    let temp = tempfile::tempdir().unwrap();
    let paths = StatePaths::new(temp.path().join("state"));

    assert_eq!(paths.descriptor(), temp.path().join("state/runtime.json"));
    assert_eq!(paths.startup_lock(), temp.path().join("state/startup.lock"));
    assert_eq!(paths.host_lock(), temp.path().join("state/host.lock"));
    assert_eq!(paths.log(), temp.path().join("state/rondo-core.log"));
}

#[test]
fn health_is_read_only_and_requires_the_exact_recorded_instance() {
    let temp = tempfile::tempdir().unwrap();
    let paths = StatePaths::new(temp.path().join("state"));
    fs::create_dir_all(paths.root()).unwrap();

    let (base_url, server) = one_health_response(json!({
        "surface": "rondo.core/v1",
        "runtime_version": "0.1.0",
        "instance_id": "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
        "service_mode": "trackerless_core",
        "ready": true,
        "active_run_count": 0
    }));
    write_descriptor(
        &paths.descriptor(),
        &RuntimeDescriptor::verified(
            base_url,
            "0.1.0",
            "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
            123,
            456,
        ),
    );
    let before = fs::read(paths.descriptor()).unwrap();

    let report = health(&paths);

    assert!(report.ok);
    assert_eq!(report.status, "running");
    assert_eq!(
        report.instance_id.as_deref(),
        Some("019b8941-4a0c-7ad5-b7ef-cb3c45e4a819")
    );
    assert_eq!(fs::read(paths.descriptor()).unwrap(), before);
    server.join().unwrap();
}

#[test]
fn health_rejects_identity_mismatch_without_rewriting_or_deleting_state() {
    let temp = tempfile::tempdir().unwrap();
    let paths = StatePaths::new(temp.path().join("state"));
    fs::create_dir_all(paths.root()).unwrap();

    let (base_url, server) = one_health_response(json!({
        "surface": "rondo.core/v1",
        "runtime_version": "0.1.0",
        "instance_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "service_mode": "trackerless_core",
        "ready": true,
        "active_run_count": 0
    }));
    write_descriptor(
        &paths.descriptor(),
        &RuntimeDescriptor::verified(
            base_url,
            "0.1.0",
            "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
            123,
            456,
        ),
    );
    let before = fs::read(paths.descriptor()).unwrap();

    let report = health(&paths);

    assert!(!report.ok);
    assert_eq!(report.status, "unverified");
    assert_eq!(fs::read(paths.descriptor()).unwrap(), before);
    server.join().unwrap();
}

#[test]
fn health_rejects_an_unpinned_runtime_version_without_mutating_state() {
    let temp = tempfile::tempdir().unwrap();
    let paths = StatePaths::new(temp.path().join("state"));
    fs::create_dir_all(paths.root()).unwrap();

    let (base_url, server) = one_health_response(json!({
        "surface": "rondo.core/v1",
        "runtime_version": "9.9.9",
        "instance_id": "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
        "service_mode": "trackerless_core",
        "ready": true,
        "active_run_count": 0
    }));
    write_descriptor(
        &paths.descriptor(),
        &RuntimeDescriptor::verified(
            base_url,
            "9.9.9",
            "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
            123,
            456,
        ),
    );
    let before = fs::read(paths.descriptor()).unwrap();

    let report = health(&paths);

    assert!(!report.ok);
    assert_eq!(report.status, "unverified");
    assert_eq!(fs::read(paths.descriptor()).unwrap(), before);
    server.join().unwrap();
}

#[test]
fn stop_refuses_an_identity_mismatch_without_signaling_or_mutating_state() {
    let temp = tempfile::tempdir().unwrap();
    let paths = StatePaths::new(temp.path().join("state"));
    fs::create_dir_all(paths.root()).unwrap();
    let (base_url, server) = one_health_response(json!({
        "surface": "rondo.core/v1",
        "runtime_version": "0.1.0",
        "instance_id": "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "service_mode": "trackerless_core",
        "ready": true,
        "active_run_count": 0
    }));
    write_descriptor(
        &paths.descriptor(),
        &RuntimeDescriptor::verified(
            base_url,
            "0.1.0",
            "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
            1,
            1,
        ),
    );
    let before = fs::read(paths.descriptor()).unwrap();

    let report = stop(&paths).unwrap();

    assert!(!report.ok);
    assert!(report.diagnostics[0].contains("not verified"));
    assert_eq!(fs::read(paths.descriptor()).unwrap(), before);
    server.join().unwrap();
}

fn write_descriptor(path: &Path, descriptor: &RuntimeDescriptor) {
    fs::write(path, serde_json::to_vec(descriptor).unwrap()).unwrap();
}

fn one_health_response(body: serde_json::Value) -> (String, thread::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = [0_u8; 2048];
        let size = stream.read(&mut request).unwrap();
        let request = String::from_utf8_lossy(&request[..size]);
        assert!(request.starts_with("GET /api/v1/health HTTP/1.1"));
        let body = body.to_string();
        write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .unwrap();
    });
    (format!("http://{address}"), handle)
}
