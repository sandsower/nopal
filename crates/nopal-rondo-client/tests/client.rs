use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex, mpsc};
use std::thread;
use std::time::Duration;

use nopal_rondo_client::{
    ClientError, ConfigurationError, CoreErrorCode, HttpMethod, HttpRequest, HttpResponse,
    ProtocolError, RequestError, RondoCoreClient, RunHandle, SubmitRequest, Transport,
    TransportError, WireError,
};
use serde_json::{Value, json};

const DIGEST: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const RESPONSE_LIMIT_BYTES: usize = 1024 * 1024;

fn merge(value: &Value, key: &str, replacement: Value) -> Value {
    let mut updated = value.clone();
    if let Some(object) = updated.as_object_mut() {
        object.insert(key.to_owned(), replacement);
    }
    updated
}

#[derive(Clone)]
struct RecordingTransport {
    requests: Arc<Mutex<Vec<HttpRequest>>>,
    responses: Arc<Mutex<Vec<Result<HttpResponse, WireError>>>>,
}

impl RecordingTransport {
    fn new(responses: Vec<Result<HttpResponse, WireError>>) -> Self {
        Self {
            requests: Arc::new(Mutex::new(Vec::new())),
            responses: Arc::new(Mutex::new(responses.into_iter().rev().collect())),
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        match self.requests.lock() {
            Ok(requests) => requests.clone(),
            Err(error) => panic!("request recorder poisoned: {error}"),
        }
    }
}

impl Transport for RecordingTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, WireError> {
        match self.requests.lock() {
            Ok(mut requests) => requests.push(request),
            Err(_) => return Err(WireError::Unavailable),
        }

        match self.responses.lock() {
            Ok(mut responses) => responses.pop().unwrap_or(Err(WireError::Unavailable)),
            Err(_) => Err(WireError::Unavailable),
        }
    }
}

#[test]
fn rejects_every_non_loopback_or_non_origin_base_url() {
    let rejected = [
        "https://127.0.0.1:4400",
        "http://localhost:4400",
        "http://192.0.2.10:4400",
        "http://[::2]:4400",
        "http://user:secret@127.0.0.1:4400",
        "http://127.0.0.1:4400/api",
        "http://127.0.0.1:4400?token=secret",
        "http://127.0.0.1:4400/#fragment",
    ];

    for base_url in rejected {
        let result = RondoCoreClient::with_transport(base_url, RecordingTransport::new(vec![]));
        assert!(matches!(
            result,
            Err(ClientError::Configuration(
                ConfigurationError::InvalidBaseUrl
            ))
        ));
    }

    for base_url in ["http://127.42.5.9:4400", "http://[::1]:4400/"] {
        let result = RondoCoreClient::with_transport(base_url, RecordingTransport::new(vec![]));
        assert!(result.is_ok(), "literal loopback origin should be accepted");
    }
}

#[test]
fn production_client_rejects_zero_or_unrepresentable_timeouts() {
    for timeout in [Duration::ZERO, Duration::MAX] {
        let result = RondoCoreClient::new("http://127.0.0.1:4400", timeout);
        assert!(matches!(
            result,
            Err(ClientError::Configuration(
                ConfigurationError::InvalidTimeout
            ))
        ));
    }

    assert!(
        RondoCoreClient::new("http://127.0.0.1:4400", Duration::from_secs(1)).is_ok(),
        "an ordinary nonzero timeout should remain valid"
    );
}

#[test]
fn health_builds_the_exact_request_and_accepts_a_trackerless_core_identity() {
    let transport = RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        json!({
            "surface": "rondo.core/v1",
            "runtime_version": "0.1.0",
            "instance_id": "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
            "service_mode": "trackerless_core",
            "ready": true,
            "active_run_count": 2
        }),
    ))]);
    let client = fake_client(transport.clone());

    let health = must_ok(client.health());

    assert_eq!(health.surface, "rondo.core/v1");
    assert_eq!(health.runtime_version, "0.1.0");
    assert_eq!(health.instance_id, "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819");
    assert_eq!(health.service_mode, "trackerless_core");
    assert!(health.ready);
    assert_eq!(health.active_run_count, 2);

    let requests = transport.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, HttpMethod::Get);
    assert_eq!(
        requests[0].url.as_str(),
        "http://127.0.0.1:4400/api/v1/health"
    );
    assert_eq!(requests[0].json_body, None);
}

#[test]
fn health_rejects_incompatible_or_malformed_identity_fields() {
    let valid = json!({
        "surface": "rondo.core/v1",
        "runtime_version": "0.1.0",
        "instance_id": "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819",
        "service_mode": "trackerless_core",
        "ready": true,
        "active_run_count": 0
    });
    let invalid = [
        merge(&valid, "surface", json!("rondo.core/v2")),
        merge(&valid, "runtime_version", json!("")),
        merge(&valid, "instance_id", json!("not-an-instance-id")),
        merge(&valid, "service_mode", json!("tracker_daemon")),
        merge(&valid, "ready", json!("true")),
        merge(&valid, "active_run_count", json!(-1)),
    ];

    for body in invalid {
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            200, body,
        ))]));
        assert!(matches!(client.health(), Err(ClientError::Protocol(_))));
    }
}

#[test]
fn rejects_invalid_request_fields_before_transport() {
    let transport = RecordingTransport::new(vec![]);
    let client = fake_client(transport.clone());

    let cases = [
        (
            SubmitRequest::new("", DIGEST, "repo"),
            RequestError::MissingManifestPath,
        ),
        (
            SubmitRequest::new(
                "/repo/slice.json",
                "Aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "repo",
            ),
            RequestError::InvalidManifestDigest,
        ),
        (
            SubmitRequest::new("/repo/slice.json", "short", "repo"),
            RequestError::InvalidManifestDigest,
        ),
        (
            SubmitRequest::new("/repo/slice.json", DIGEST, " "),
            RequestError::MissingRepoId,
        ),
        (
            SubmitRequest::new("/repo/slice.json", DIGEST, " repo "),
            RequestError::InvalidRepoId,
        ),
    ];

    for (request, expected) in cases {
        assert_eq!(client.submit(request), Err(ClientError::Request(expected)));
    }

    assert!(transport.requests().is_empty());
}

#[test]
fn rejects_invalid_plot_identifiers_before_transport() {
    let transport = RecordingTransport::new(vec![]);
    let client = fake_client(transport.clone());
    let cases = [
        ("", RequestError::MissingPlotId),
        (" TASK-52", RequestError::InvalidPlotId),
        ("TASK-52\nsecret", RequestError::PlotIdContainsControl),
    ];

    for (plot_id, expected) in cases {
        assert_eq!(
            client.submit(SubmitRequest::for_plot(
                "/repo/slice.json",
                DIGEST,
                "repo",
                plot_id,
            )),
            Err(ClientError::Request(expected))
        );
    }

    assert_eq!(
        client.submit(SubmitRequest::for_plot(
            "/repo/slice.json",
            DIGEST,
            "repo",
            "p".repeat(513),
        )),
        Err(ClientError::Request(RequestError::PlotIdTooLong))
    );
    assert!(transport.requests().is_empty());
}

#[test]
fn rejects_padded_repository_ids_in_handles_before_transport() {
    let transport = RecordingTransport::new(vec![]);
    let client = fake_client(transport.clone());

    assert_eq!(
        client.status(RunHandle::new(" repo", "run")),
        Err(ClientError::Request(RequestError::InvalidRepoId))
    );
    assert_eq!(
        client.events(RunHandle::new("repo ", "run"), None),
        Err(ClientError::Request(RequestError::InvalidRepoId))
    );
    assert!(transport.requests().is_empty());
}

#[test]
fn rejects_oversized_or_control_bearing_repository_ids_before_transport() {
    let transport = RecordingTransport::new(vec![]);
    let client = fake_client(transport.clone());
    let oversized = "r".repeat(513);

    assert_eq!(
        client.submit(SubmitRequest::new("/slice.json", DIGEST, &oversized)),
        Err(ClientError::Request(RequestError::RepoIdTooLong))
    );
    assert_eq!(
        client.status(RunHandle::new("repo\nsecret", "run")),
        Err(ClientError::Request(RequestError::RepoIdContainsControl))
    );
    assert_eq!(
        client.events(RunHandle::new("repo\0secret", "run"), None),
        Err(ClientError::Request(RequestError::RepoIdContainsControl))
    );
    assert!(transport.requests().is_empty());
}

#[test]
fn submit_builds_the_exact_request_and_accepts_202_or_deduplicated_200() {
    for (status, deduplicated) in [(202, false), (200, true)] {
        let transport = RecordingTransport::new(vec![Ok(HttpResponse::new(
            status,
            submit_success(deduplicated),
        ))]);
        let client = fake_client(transport.clone());
        let response = must_ok(client.submit(SubmitRequest::for_plot(
            "/repo/.beislid/exports/bundle/slices/slice.json",
            DIGEST,
            "nopal.repo/v1:opaque",
            "TASK-52",
        )));

        assert_eq!(response.surface, "rondo.core/v1");
        assert_eq!(response.service_id, "rondo-core");
        assert_eq!(response.repo_id, "nopal.repo/v1:opaque");
        assert_eq!(response.plot_id.as_deref(), Some("TASK-52"));
        assert_eq!(response.run_id, "run-opaque");
        assert_eq!(response.status, "running");
        assert_eq!(response.event_cursor, "rondo.core/v1:0");
        assert_eq!(response.deduplicated, deduplicated);

        let requests = transport.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].method, HttpMethod::Post);
        assert_eq!(
            requests[0].url.as_str(),
            "http://127.0.0.1:4400/api/v1/execution-requests"
        );
        assert_eq!(
            requests[0].json_body,
            Some(json!({
                "manifest_path": "/repo/.beislid/exports/bundle/slices/slice.json",
                "manifest_sha256": DIGEST,
                "repo_id": "nopal.repo/v1:opaque",
                "plot_id": "TASK-52"
            }))
        );
    }
}

#[test]
fn plot_scoped_calls_fail_closed_on_missing_or_mismatched_echoes() {
    for plot_id in [Value::Null, json!("OLI-foreign")] {
        let response = merge(&submit_success(false), "plot_id", plot_id);
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            202, response,
        ))]));
        assert_eq!(
            client.submit(SubmitRequest::for_plot(
                "/slice.json",
                DIGEST,
                "nopal.repo/v1:opaque",
                "TASK-52",
            )),
            Err(ClientError::Protocol(ProtocolError::PlotIdMismatch))
        );
    }

    let status = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "run_id": "run",
        "status": "running",
        "last_event": null,
        "evidence_pointers": [],
        "event_cursor": "rondo.core/v1:0"
    });
    let events = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "run_id": "run",
        "plot_id": "OLI-foreign",
        "events": [],
        "next_event_cursor": "rondo.core/v1:0",
        "has_more": false
    });
    let client = fake_client(RecordingTransport::new(vec![
        Ok(HttpResponse::new(200, status)),
        Ok(HttpResponse::new(200, events)),
    ]));
    let handle = RunHandle::for_plot("repo", "run", "TASK-52");

    assert_eq!(
        client.status(handle.clone()),
        Err(ClientError::Protocol(ProtocolError::PlotIdMismatch))
    );
    assert_eq!(
        client.events(handle, None),
        Err(ClientError::Protocol(ProtocolError::PlotIdMismatch))
    );
}

#[test]
fn no_plot_calls_reject_unsolicited_plot_echoes() {
    let submit = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        202,
        submit_success(false),
    ))]));
    assert_eq!(
        submit.submit(SubmitRequest::new(
            "/slice.json",
            DIGEST,
            "nopal.repo/v1:opaque"
        )),
        Err(ClientError::Protocol(ProtocolError::PlotIdMismatch))
    );

    let status = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        run_status_success(Some("TASK-52"), Value::Null),
    ))]));
    assert_eq!(
        status.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::PlotIdMismatch))
    );

    let events = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        run_events_success(Some("TASK-52"), vec![]),
    ))]));
    assert_eq!(
        events.events(RunHandle::new("repo", "run"), None),
        Err(ClientError::Protocol(ProtocolError::PlotIdMismatch))
    );
}

#[test]
fn plot_scoped_calls_reject_foreign_or_missing_run_fact_identity() {
    let handle = RunHandle::for_plot("repo", "run", "TASK-52");
    let valid = run_status_event("repo", Some("TASK-52"), "run");

    for (invalid, expected) in [
        (
            merge(&valid, "plot_id", Value::Null),
            ProtocolError::InvalidResponse,
        ),
        (
            merge(&valid, "plot_id", json!("OLI-foreign")),
            ProtocolError::PlotIdMismatch,
        ),
        (
            merge(&valid, "repo_id", json!("repo-foreign")),
            ProtocolError::RepoIdMismatch,
        ),
        (
            merge(&valid, "run_id", json!("run-foreign")),
            ProtocolError::RunIdMismatch,
        ),
        (
            merge(
                &valid,
                "namespace",
                json!({"repo_id": "repo", "run_id": "run"}),
            ),
            ProtocolError::PlotIdMismatch,
        ),
        (
            merge(
                &valid,
                "namespace",
                json!({"repo_id": "repo", "plot_id": "OLI-foreign", "run_id": "run"}),
            ),
            ProtocolError::PlotIdMismatch,
        ),
        (
            merge(
                &valid,
                "namespace",
                json!({"repo_id": "repo-foreign", "plot_id": "TASK-52", "run_id": "run"}),
            ),
            ProtocolError::RepoIdMismatch,
        ),
        (
            merge(
                &valid,
                "namespace",
                json!({"repo_id": "repo", "plot_id": "TASK-52", "run_id": "run-foreign"}),
            ),
            ProtocolError::RunIdMismatch,
        ),
    ] {
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            200,
            run_events_success(Some("TASK-52"), vec![invalid]),
        ))]));
        assert_eq!(
            client.events(handle.clone(), None),
            Err(ClientError::Protocol(expected))
        );
    }

    let invalid_last_event = merge(
        &valid,
        "namespace",
        json!({"repo_id": "repo", "plot_id": "OLI-foreign", "run_id": "run"}),
    );
    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        run_status_success(Some("TASK-52"), invalid_last_event),
    ))]));
    assert_eq!(
        client.status(handle.clone()),
        Err(ClientError::Protocol(ProtocolError::PlotIdMismatch))
    );

    let bounded = json!({
        "type": "rondo.run.status_changed",
        "sequence": 1,
        "payload_omitted": true,
        "reason": "event_exceeds_observation_budget",
        "namespace": {"repo_id": "repo", "plot_id": "TASK-52", "run_id": "run"}
    });
    let service = json!({
        "type": "rondo.service.status_changed",
        "sequence": 2,
        "service_id": "rondo-core",
        "namespace": {"service_id": "rondo-core"}
    });
    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        run_events_success(Some("TASK-52"), vec![bounded, service]),
    ))]));
    assert_eq!(must_ok(client.events(handle, None)).events.len(), 2);
}

#[test]
fn submit_requires_the_pinned_success_shape_and_exact_repo_echo() {
    let invalid = [
        json!({
            "surface": "rondo.core/v2",
            "service_id": "rondo-core",
            "repo_id": "repo",
            "run_id": "run",
            "status": "running",
            "event_cursor": "rondo.core/v1:0",
            "deduplicated": false
        }),
        json!({
            "surface": "rondo.core/v1",
            "service_id": "",
            "repo_id": "repo",
            "run_id": "run",
            "status": "running",
            "event_cursor": "rondo.core/v1:0",
            "deduplicated": false
        }),
        json!({
            "surface": "rondo.core/v1",
            "service_id": "rondo-core",
            "repo_id": "different-repo",
            "run_id": "run",
            "status": "running",
            "event_cursor": "rondo.core/v1:0",
            "deduplicated": false
        }),
        json!({
            "surface": "rondo.core/v1",
            "service_id": "rondo-core",
            "repo_id": "repo",
            "run_id": "",
            "status": "running",
            "event_cursor": "rondo.core/v1:0",
            "deduplicated": false
        }),
        json!({
            "surface": "rondo.core/v1",
            "service_id": "rondo-core",
            "repo_id": "repo",
            "run_id": "run",
            "status": "running",
            "event_cursor": "",
            "deduplicated": false
        }),
        json!({
            "surface": "rondo.core/v1",
            "service_id": "rondo-core",
            "repo_id": "repo",
            "run_id": "run",
            "status": "running",
            "event_cursor": "rondo.core/v1:0"
        }),
    ];

    for body in invalid {
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            202, body,
        ))]));
        let result = client.submit(SubmitRequest::new("/slice.json", DIGEST, "repo"));
        assert!(matches!(result, Err(ClientError::Protocol(_))));
    }

    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        submit_success(false),
    ))]));
    assert_eq!(
        client.submit(SubmitRequest::for_plot(
            "/slice.json",
            DIGEST,
            "nopal.repo/v1:opaque",
            "TASK-52"
        )),
        Err(ClientError::Protocol(
            ProtocolError::InconsistentSubmitStatus
        ))
    );

    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        202,
        json!({"surface": "rondo.core/v1"}),
    ))]));
    assert_eq!(
        client.submit(SubmitRequest::new("/slice.json", DIGEST, "repo")),
        Err(ClientError::Protocol(ProtocolError::InvalidResponse))
    );
}

#[test]
fn status_and_events_encode_opaque_path_and_query_values_exactly() {
    let status_body = json!({
        "surface": "rondo.core/v1",
        "run_id": "run /?#",
        "repo_id": "repo /?#",
        "plot_id": "TASK-52",
        "status": "running",
        "last_event": null,
        "evidence_pointers": [
            {"artifact_kind": "transcript", "uri": "rondo-run://opaque/evidence", "future": 7}
        ],
        "event_cursor": "rondo.core/v1:0",
        "future": true
    });
    let events_body = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo /?#",
        "run_id": "run /?#",
        "plot_id": "TASK-52",
        "events": [{"type": "run.status_changed", "future": {"nested": true}}],
        "next_event_cursor": "rondo.core/v1:8",
        "has_more": true,
        "future": "allowed"
    });
    let transport = RecordingTransport::new(vec![
        Ok(HttpResponse::new(200, status_body)),
        Ok(HttpResponse::new(200, events_body)),
    ]);
    let client = fake_client(transport.clone());
    let handle = RunHandle::for_plot("repo /?#", "run /?#", "TASK-52");

    let status = must_ok(client.status(handle.clone()));
    assert_eq!(status.run_id, "run /?#");
    assert_eq!(status.plot_id.as_deref(), Some("TASK-52"));
    assert_eq!(status.status, "running");
    assert_eq!(status.last_event, None);
    assert_eq!(status.evidence_pointers.len(), 1);
    assert_eq!(status.evidence_pointers[0].artifact_kind, "transcript");
    assert_eq!(
        status.evidence_pointers[0].uri,
        "rondo-run://opaque/evidence"
    );
    assert_eq!(status.event_cursor, "rondo.core/v1:0");

    let events = must_ok(client.events(handle, Some("rondo.core/v1:7")));
    assert_eq!(events.events.len(), 1);
    assert_eq!(events.plot_id.as_deref(), Some("TASK-52"));
    assert_eq!(events.events[0]["future"]["nested"], true);
    assert_eq!(events.next_event_cursor, "rondo.core/v1:8");
    assert!(events.has_more);

    let requests = transport.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].method, HttpMethod::Get);
    assert_eq!(
        requests[0].url.as_str(),
        "http://127.0.0.1:4400/api/v1/runs/run%20%2F%3F%23?repo_id=repo+%2F%3F%23"
    );
    assert_eq!(requests[0].json_body, None);
    assert_eq!(requests[1].method, HttpMethod::Get);
    assert_eq!(
        requests[1].url.as_str(),
        "http://127.0.0.1:4400/api/v1/runs/run%20%2F%3F%23/events?repo_id=repo+%2F%3F%23&cursor=rondo.core%2Fv1%3A7"
    );
}

#[test]
fn status_rejects_malformed_evidence_pointers() {
    let invalid_pointers = [
        json!({"uri": "rondo-run://run/evidence"}),
        json!({"artifact_kind": "report", "uri": ""}),
        json!({"artifact_kind": 7, "uri": "rondo-run://run/evidence"}),
        json!({"artifact_kind": " report", "uri": "rondo-run://run/evidence"}),
        json!({"artifact_kind": "report", "uri": "rondo-run://"}),
        json!({"artifact_kind": "report", "uri": "/private/run/evidence"}),
        json!({"artifact_kind": "report", "uri": "rondo-run://run/evidence\n"}),
    ];

    for pointer in invalid_pointers {
        let mut body = run_status_success(None, Value::Null);
        body["evidence_pointers"] = json!([pointer]);
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            200, body,
        ))]));

        assert_eq!(
            client.status(RunHandle::new("repo", "run")),
            Err(ClientError::Protocol(ProtocolError::InvalidResponse))
        );
    }
}

#[test]
fn status_validates_evidence_last_event_but_accepts_its_bounded_diagnostic() {
    let malformed = json!({
        "type": "rondo.run.evidence_recorded",
        "sequence": 1,
        "repo_id": "repo",
        "run_id": "run",
        "artifact_kind": "delivery_artifact",
        "namespace": {"repo_id": "repo", "run_id": "run"}
    });
    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        run_status_success(None, malformed),
    ))]));
    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::InvalidResponse))
    );

    let diagnostic = json!({
        "type": "rondo.run.evidence_recorded",
        "sequence": 1,
        "payload_omitted": true,
        "reason": "event_exceeds_observation_budget",
        "namespace": {"repo_id": "repo", "run_id": "run"},
        "future": "compatible"
    });
    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        run_status_success(None, diagnostic.clone()),
    ))]));
    assert_eq!(
        must_ok(client.status(RunHandle::new("repo", "run"))).last_event,
        Some(diagnostic)
    );
}

#[test]
fn events_project_typed_evidence_and_reject_malformed_evidence_facts() {
    let evidence = json!({
        "type": "rondo.run.evidence_recorded",
        "sequence": 1,
        "repo_id": "repo",
        "run_id": "run",
        "artifact_kind": "delivery_artifact",
        "uri": "rondo-run://run/artifacts/delivery.json",
        "namespace": {"repo_id": "repo", "run_id": "run"},
        "future": {"compatible": true}
    });
    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        run_events_success(None, vec![evidence.clone()]),
    ))]));

    let page = must_ok(client.events(RunHandle::new("repo", "run"), None));
    assert_eq!(
        page.evidence_pointers(),
        vec![nopal_rondo_client::EvidencePointer {
            artifact_kind: "delivery_artifact".to_owned(),
            uri: "rondo-run://run/artifacts/delivery.json".to_owned(),
        }]
    );

    for (field, value) in [
        ("uri", Value::Null),
        ("uri", json!("/private/run/evidence")),
        ("artifact_kind", json!("")),
        ("artifact_kind", json!(7)),
    ] {
        let mut malformed = evidence.clone();
        malformed[field] = value;
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            200,
            run_events_success(None, vec![malformed]),
        ))]));
        assert_eq!(
            client.events(RunHandle::new("repo", "run"), None),
            Err(ClientError::Protocol(ProtocolError::InvalidResponse))
        );
    }

    let mut missing_uri = evidence;
    missing_uri.as_object_mut().unwrap().remove("uri");
    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        run_events_success(None, vec![missing_uri]),
    ))]));
    assert_eq!(
        client.events(RunHandle::new("repo", "run"), None),
        Err(ClientError::Protocol(ProtocolError::InvalidResponse))
    );
}

#[test]
fn oversized_evidence_diagnostics_remain_valid_but_do_not_fabricate_pointers() {
    let diagnostic = json!({
        "type": "rondo.run.evidence_recorded",
        "sequence": 1,
        "payload_omitted": true,
        "reason": "event_exceeds_observation_budget",
        "namespace": {"repo_id": "repo", "plot_id": "TASK-52", "run_id": "run"},
        "future": "compatible"
    });
    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        run_events_success(Some("TASK-52"), vec![diagnostic.clone()]),
    ))]));

    let page = must_ok(client.events(RunHandle::for_plot("repo", "run", "TASK-52"), None));
    assert_eq!(page.events, vec![diagnostic]);
    assert!(page.evidence_pointers().is_empty());
}

#[test]
fn status_and_events_preserve_opaque_dot_segment_run_ids() {
    let status_body = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "run_id": ".",
        "status": "running",
        "last_event": null,
        "evidence_pointers": [],
        "event_cursor": "rondo.core/v1:0"
    });
    let events_body = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "run_id": "..",
        "events": [],
        "next_event_cursor": "rondo.core/v1:0",
        "has_more": false
    });
    let transport = RecordingTransport::new(vec![
        Ok(HttpResponse::new(200, status_body)),
        Ok(HttpResponse::new(200, events_body)),
    ]);
    let client = fake_client(transport.clone());

    must_ok(client.status(RunHandle::new("repo", ".")));
    must_ok(client.events(RunHandle::new("repo", ".."), None));

    let requests = transport.requests();
    assert_eq!(
        requests[0].url.as_str(),
        "http://127.0.0.1:4400/api/v1/runs/%2E?repo_id=repo"
    );
    assert_eq!(
        requests[1].url.as_str(),
        "http://127.0.0.1:4400/api/v1/runs/%2E%2E/events?repo_id=repo"
    );
}

#[test]
fn status_and_events_reject_invalid_handles_and_response_echoes() {
    let transport = RecordingTransport::new(vec![]);
    let client = fake_client(transport.clone());

    assert_eq!(
        client.status(RunHandle::new("", "run")),
        Err(ClientError::Request(RequestError::MissingRepoId))
    );
    assert_eq!(
        client.events(RunHandle::new("repo", ""), None),
        Err(ClientError::Request(RequestError::MissingRunId))
    );
    assert_eq!(
        client.events(RunHandle::new("repo", "run"), Some(" ")),
        Err(ClientError::Request(RequestError::InvalidEventCursor))
    );
    assert_eq!(
        client.events(RunHandle::new("repo", "run"), Some("opaque")),
        Err(ClientError::Request(RequestError::InvalidEventCursor))
    );
    assert_eq!(
        client.events(
            RunHandle::new("repo", "run"),
            Some("rondo.core/v1:123456789012345678901")
        ),
        Err(ClientError::Request(RequestError::InvalidEventCursor))
    );
    assert!(transport.requests().is_empty());

    let wrong_status_echo = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "run_id": "other-run",
        "status": "running",
        "last_event": null,
        "evidence_pointers": [],
        "event_cursor": "rondo.core/v1:0"
    });
    let wrong_events_echo = json!({
        "surface": "rondo.core/v1",
        "repo_id": "other-repo",
        "run_id": "run",
        "events": [],
        "next_event_cursor": "rondo.core/v1:0",
        "has_more": false
    });
    let client = fake_client(RecordingTransport::new(vec![
        Ok(HttpResponse::new(200, wrong_status_echo)),
        Ok(HttpResponse::new(200, wrong_events_echo)),
    ]));

    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::RunIdMismatch))
    );
    assert_eq!(
        client.events(RunHandle::new("repo", "run"), None),
        Err(ClientError::Protocol(ProtocolError::RepoIdMismatch))
    );
}

#[test]
fn rejects_malformed_cursors_on_every_response_surface() {
    for cursor in [
        "",
        "rondo.core/v1:",
        "rondo.core/v1:-1",
        "rondo.core/v1:+1",
        "rondo.core/v1:1x",
        "rondo.core/v1:123456789012345678901",
        "rondo.core/v2:1",
    ] {
        let mut submit = submit_success(false);
        submit["event_cursor"] = json!(cursor);
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            202, submit,
        ))]));
        assert_eq!(
            client.submit(SubmitRequest::for_plot(
                "/slice.json",
                DIGEST,
                "nopal.repo/v1:opaque",
                "TASK-52"
            )),
            Err(ClientError::Protocol(ProtocolError::InvalidResponse))
        );

        let status = json!({
            "surface": "rondo.core/v1",
            "repo_id": "repo",
            "run_id": "run",
            "status": "running",
            "last_event": null,
            "evidence_pointers": [],
            "event_cursor": cursor
        });
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            200, status,
        ))]));
        assert_eq!(
            client.status(RunHandle::new("repo", "run")),
            Err(ClientError::Protocol(ProtocolError::InvalidResponse))
        );

        let events = json!({
            "surface": "rondo.core/v1",
            "repo_id": "repo",
            "run_id": "run",
            "events": [],
            "next_event_cursor": cursor,
            "has_more": false
        });
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            200, events,
        ))]));
        assert_eq!(
            client.events(RunHandle::new("repo", "run"), None),
            Err(ClientError::Protocol(ProtocolError::InvalidResponse))
        );
    }
}

#[test]
fn events_require_exact_cursor_advancement() {
    for (requested, events, next) in [
        (
            Some("rondo.core/v1:4"),
            vec![json!({"type": "one"})],
            "rondo.core/v1:4",
        ),
        (
            Some("rondo.core/v1:4"),
            vec![json!({"type": "one"})],
            "rondo.core/v1:6",
        ),
        (Some("rondo.core/v1:4"), Vec::new(), "rondo.core/v1:5"),
        (None, Vec::new(), "rondo.core/v1:1"),
    ] {
        let response = json!({
            "surface": "rondo.core/v1",
            "repo_id": "repo",
            "run_id": "run",
            "events": events,
            "next_event_cursor": next,
            "has_more": false
        });
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            200, response,
        ))]));
        assert_eq!(
            client.events(RunHandle::new("repo", "run"), requested),
            Err(ClientError::Protocol(ProtocolError::InvalidResponse))
        );
    }

    let response = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "run_id": "run",
        "events": [],
        "next_event_cursor": "rondo.core/v1:4",
        "has_more": false
    });
    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200, response,
    ))]));
    let page = must_ok(client.events(RunHandle::new("repo", "run"), Some("rondo.core/v1:4")));
    assert!(page.events.is_empty());
    assert_eq!(page.next_event_cursor, "rondo.core/v1:4");

    let maximum = "rondo.core/v1:18446744073709551615";
    let response = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "run_id": "run",
        "events": [],
        "next_event_cursor": maximum,
        "has_more": false
    });
    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200, response,
    ))]));
    let page = must_ok(client.events(RunHandle::new("repo", "run"), Some(maximum)));
    assert_eq!(page.next_event_cursor, maximum);
}

#[test]
fn status_and_events_require_surface_and_exact_handle_echoes() {
    let status_missing_repo = json!({
        "surface": "rondo.core/v1",
        "run_id": "run",
        "status": "running",
        "last_event": null,
        "evidence_pointers": [],
        "event_cursor": "rondo.core/v1:0"
    });
    let events_missing_run = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "events": [],
        "next_event_cursor": "rondo.core/v1:0",
        "has_more": false
    });
    let wrong_status_surface = json!({
        "surface": "rondo.core/v2",
        "repo_id": "repo",
        "run_id": "run",
        "status": "running",
        "last_event": null,
        "evidence_pointers": [],
        "event_cursor": "rondo.core/v1:0"
    });
    let wrong_events_surface = json!({
        "surface": "rondo.core/v2",
        "repo_id": "repo",
        "run_id": "run",
        "events": [],
        "next_event_cursor": "rondo.core/v1:0",
        "has_more": false
    });
    let client = fake_client(RecordingTransport::new(vec![
        Ok(HttpResponse::new(200, status_missing_repo)),
        Ok(HttpResponse::new(200, events_missing_run)),
        Ok(HttpResponse::new(200, wrong_status_surface)),
        Ok(HttpResponse::new(200, wrong_events_surface)),
    ]));

    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::InvalidResponse))
    );
    assert_eq!(
        client.events(RunHandle::new("repo", "run"), None),
        Err(ClientError::Protocol(ProtocolError::InvalidResponse))
    );
    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::SurfaceMismatch))
    );
    assert_eq!(
        client.events(RunHandle::new("repo", "run"), None),
        Err(ClientError::Protocol(ProtocolError::SurfaceMismatch))
    );

    let missing_has_more = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "run_id": "run",
        "events": [],
        "next_event_cursor": "rondo.core/v1:0"
    });
    let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        200,
        missing_has_more,
    ))]));
    assert_eq!(
        client.events(RunHandle::new("repo", "run"), None),
        Err(ClientError::Protocol(ProtocolError::InvalidResponse))
    );
}

#[test]
fn maps_every_pinned_core_error_code_without_retaining_server_messages() {
    let cases = [
        (400, "invalid_request", CoreErrorCode::InvalidRequest),
        (409, "digest_conflict", CoreErrorCode::DigestConflict),
        (422, "invalid_manifest", CoreErrorCode::InvalidManifest),
        (
            422,
            "unapproved_manifest",
            CoreErrorCode::UnapprovedManifest,
        ),
        (429, "capacity_exhausted", CoreErrorCode::CapacityExhausted),
        (404, "run_not_found", CoreErrorCode::RunNotFound),
        (
            503,
            "orchestrator_unavailable",
            CoreErrorCode::OrchestratorUnavailable,
        ),
        (503, "core_unavailable", CoreErrorCode::CoreUnavailable),
        (403, "loopback_required", CoreErrorCode::LoopbackRequired),
    ];

    for (status, code, expected) in cases {
        let body = json!({
            "error": {
                "code": code,
                "message": "TOP-SECRET /private/rondo/ledger/run.json"
            }
        });
        let client = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
            status, body,
        ))]));
        let error = must_err(client.status(RunHandle::new("repo", "run")));
        assert_eq!(error, ClientError::Core(expected));
        assert!(!error.to_string().contains("TOP-SECRET"));
        assert!(!error.to_string().contains("/private/rondo"));
    }
}

#[test]
fn rejects_unknown_or_status_mismatched_error_codes_as_protocol_failures() {
    let unknown = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        404,
        json!({"error": {"code": "not_found", "message": "secret"}}),
    ))]));
    assert_eq!(
        unknown.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::UnexpectedStatus(404)))
    );

    let mismatch = fake_client(RecordingTransport::new(vec![Ok(HttpResponse::new(
        500,
        json!({"error": {"code": "run_not_found"}}),
    ))]));
    assert_eq!(
        mismatch.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::UnexpectedStatus(500)))
    );
}

#[test]
fn real_http_submit_accepts_202_and_sends_json() {
    for (status, deduplicated) in [(202, false), (200, true)] {
        let server = OneShotServer::respond(
            status,
            &submit_success(deduplicated).to_string(),
            &[],
            Duration::ZERO,
        );
        let client = must_ok(RondoCoreClient::new(
            &server.base_url,
            Duration::from_secs(1),
        ));
        let response = must_ok(client.submit(SubmitRequest::for_plot(
            "/repo/slice.json",
            DIGEST,
            "nopal.repo/v1:opaque",
            "TASK-52",
        )));
        assert_eq!(response.run_id, "run-opaque");
        assert_eq!(response.deduplicated, deduplicated);

        let request = server.request();
        assert!(request.starts_with("POST /api/v1/execution-requests HTTP/1.1\r\n"));
        assert!(
            request
                .to_ascii_lowercase()
                .contains("content-type: application/json")
        );
        let body = request.split("\r\n\r\n").nth(1).unwrap_or_default();
        let body: Value = match serde_json::from_str(body) {
            Ok(body) => body,
            Err(error) => panic!("request body should be JSON: {error}"),
        };
        assert_eq!(body["manifest_sha256"], DIGEST);
    }
}

#[test]
fn real_http_status_and_events_use_encoded_queries_and_parse_additive_fields() {
    let status_server = OneShotServer::respond(
        200,
        &json!({
            "surface": "rondo.core/v1",
            "repo_id": "repo /?#",
            "run_id": "run /?#",
            "status": "completed",
            "last_event": {"type": "run.status_changed"},
            "evidence_pointers": [{"uri": "rondo-run://run/evidence", "artifact_kind": "report"}],
            "event_cursor": "rondo.core/v1:0",
            "future": "ok"
        })
        .to_string(),
        &[],
        Duration::ZERO,
    );
    let client = must_ok(RondoCoreClient::new(
        &status_server.base_url,
        Duration::from_secs(1),
    ));
    let response = must_ok(client.status(RunHandle::new("repo /?#", "run /?#")));
    assert_eq!(response.status, "completed");
    assert!(
        status_server
            .request()
            .starts_with("GET /api/v1/runs/run%20%2F%3F%23?repo_id=repo+%2F%3F%23 HTTP/1.1\r\n")
    );

    let events_server = OneShotServer::respond(
        200,
        &json!({
            "surface": "rondo.core/v1",
            "repo_id": "repo /?#",
            "run_id": "run /?#",
            "events": [{
                "type": "rondo.run.status_changed",
                "repo_id": "repo /?#",
                "run_id": "run /?#",
                "namespace": {"repo_id": "repo /?#", "run_id": "run /?#"}
            }],
            "next_event_cursor": "rondo.core/v1:2",
            "has_more": true,
            "future": true
        })
        .to_string(),
        &[],
        Duration::ZERO,
    );
    let client = must_ok(RondoCoreClient::new(
        &events_server.base_url,
        Duration::from_secs(1),
    ));
    let response = must_ok(client.events(
        RunHandle::new("repo /?#", "run /?#"),
        Some("rondo.core/v1:1"),
    ));
    assert_eq!(response.next_event_cursor, "rondo.core/v1:2");
    assert!(response.has_more);
    assert!(
        events_server.request().starts_with(
            "GET /api/v1/runs/run%20%2F%3F%23/events?repo_id=repo+%2F%3F%23&cursor=rondo.core%2Fv1%3A1 HTTP/1.1\r\n"
        )
    );

    let dot_server = OneShotServer::respond(
        200,
        &json!({
            "surface": "rondo.core/v1",
            "repo_id": "repo",
            "run_id": ".",
            "status": "running",
            "last_event": null,
            "evidence_pointers": [],
            "event_cursor": "rondo.core/v1:0"
        })
        .to_string(),
        &[],
        Duration::ZERO,
    );
    let client = must_ok(RondoCoreClient::new(
        &dot_server.base_url,
        Duration::from_secs(1),
    ));
    must_ok(client.status(RunHandle::new("repo", ".")));
    assert!(
        dot_server
            .request()
            .starts_with("GET /api/v1/runs/%2E?repo_id=repo HTTP/1.1\r\n")
    );
}

#[test]
fn real_http_rejects_redirects_and_malformed_json() {
    let redirect_server = OneShotServer::respond(
        302,
        "",
        &[("Location", "http://127.0.0.1:9/must-not-follow")],
        Duration::ZERO,
    );
    let client = must_ok(RondoCoreClient::new(
        &redirect_server.base_url,
        Duration::from_secs(1),
    ));
    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::UnexpectedStatus(302)))
    );

    let malformed_server = OneShotServer::respond(200, "{not-json", &[], Duration::ZERO);
    let client = must_ok(RondoCoreClient::new(
        &malformed_server.base_url,
        Duration::from_secs(1),
    ));
    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::MalformedJson))
    );
}

#[test]
fn wire_failures_map_to_exact_client_error_categories() {
    let cases = [
        (
            WireError::Timeout,
            ClientError::Transport(TransportError::Timeout),
        ),
        (
            WireError::Unavailable,
            ClientError::Transport(TransportError::Unavailable),
        ),
        (
            WireError::MalformedHttp,
            ClientError::Protocol(ProtocolError::MalformedHttp),
        ),
        (
            WireError::InvalidUtf8,
            ClientError::Protocol(ProtocolError::InvalidUtf8),
        ),
        (
            WireError::ResponseTooLarge,
            ClientError::Protocol(ProtocolError::ResponseTooLarge),
        ),
    ];

    for (wire_error, expected) in cases {
        assert_eq!(ClientError::from(wire_error), expected);
    }

    fn connectivity_class(error: TransportError) -> &'static str {
        match error {
            TransportError::Timeout => "timeout",
            TransportError::Unavailable => "unavailable",
        }
    }

    assert_eq!(connectivity_class(TransportError::Timeout), "timeout");
    assert_eq!(
        connectivity_class(TransportError::Unavailable),
        "unavailable"
    );
}

#[test]
fn real_http_classifies_malformed_http_invalid_utf8_and_oversized_bodies() {
    let malformed_http = OneShotServer::raw(b"NOT HTTP\r\n\r\n", Duration::ZERO);
    let client = must_ok(RondoCoreClient::new(
        &malformed_http.base_url,
        Duration::from_secs(1),
    ));
    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::MalformedHttp))
    );
    let _request = malformed_http.request();

    let invalid_utf8 = OneShotServer::respond_bytes(200, &[0xff], &[], Duration::ZERO);
    let client = must_ok(RondoCoreClient::new(
        &invalid_utf8.base_url,
        Duration::from_secs(1),
    ));
    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::InvalidUtf8))
    );
    let _request = invalid_utf8.request();

    let exact_prefix = concat!(
        "{\"surface\":\"rondo.core/v1\",\"repo_id\":\"repo\",\"run_id\":\"run\",",
        "\"status\":\"running\",\"last_event\":null,\"evidence_pointers\":[],",
        "\"event_cursor\":\"rondo.core/v1:0\",\"padding\":\""
    );
    let exact_suffix = "\"}";
    let padding_length = RESPONSE_LIMIT_BYTES - exact_prefix.len() - exact_suffix.len();
    let exact_body = format!("{exact_prefix}{}{exact_suffix}", "x".repeat(padding_length));
    assert_eq!(exact_body.len(), RESPONSE_LIMIT_BYTES);
    let exact = OneShotServer::respond_bytes(200, exact_body.as_bytes(), &[], Duration::ZERO);
    let client = must_ok(RondoCoreClient::new(
        &exact.base_url,
        Duration::from_secs(1),
    ));
    let response = must_ok(client.status(RunHandle::new("repo", "run")));
    assert_eq!(response.status, "running");
    let _request = exact.request();

    let oversized_body = vec![b' '; RESPONSE_LIMIT_BYTES + 1];
    let oversized = OneShotServer::respond_bytes(200, &oversized_body, &[], Duration::ZERO);
    let client = must_ok(RondoCoreClient::new(
        &oversized.base_url,
        Duration::from_secs(1),
    ));
    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Protocol(ProtocolError::ResponseTooLarge))
    );
    let _request = oversized.request();
}

#[test]
fn real_http_maps_all_stable_error_bodies() {
    let cases = [
        (400, "invalid_request", CoreErrorCode::InvalidRequest),
        (409, "digest_conflict", CoreErrorCode::DigestConflict),
        (422, "invalid_manifest", CoreErrorCode::InvalidManifest),
        (
            422,
            "unapproved_manifest",
            CoreErrorCode::UnapprovedManifest,
        ),
        (429, "capacity_exhausted", CoreErrorCode::CapacityExhausted),
        (404, "run_not_found", CoreErrorCode::RunNotFound),
        (
            503,
            "orchestrator_unavailable",
            CoreErrorCode::OrchestratorUnavailable,
        ),
        (503, "core_unavailable", CoreErrorCode::CoreUnavailable),
        (403, "loopback_required", CoreErrorCode::LoopbackRequired),
    ];

    for (status, code, expected) in cases {
        let server = OneShotServer::respond(
            status,
            &json!({"error": {"code": code, "message": "private detail"}}).to_string(),
            &[],
            Duration::ZERO,
        );
        let client = must_ok(RondoCoreClient::new(
            &server.base_url,
            Duration::from_secs(1),
        ));
        assert_eq!(
            client.status(RunHandle::new("repo", "run")),
            Err(ClientError::Core(expected))
        );
    }
}

#[test]
fn real_http_distinguishes_timeout_and_unavailable_transport() {
    let timeout_server = OneShotServer::respond(200, "{}", &[], Duration::from_millis(150));
    let client = must_ok(RondoCoreClient::new(
        &timeout_server.base_url,
        Duration::from_millis(20),
    ));
    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Transport(TransportError::Timeout))
    );

    let listener = match TcpListener::bind("127.0.0.1:0") {
        Ok(listener) => listener,
        Err(error) => panic!("failed to reserve loopback port: {error}"),
    };
    let address = match listener.local_addr() {
        Ok(address) => address,
        Err(error) => panic!("failed to read loopback address: {error}"),
    };
    drop(listener);
    let client = must_ok(RondoCoreClient::new(
        &format!("http://{address}"),
        Duration::from_millis(100),
    ));
    assert_eq!(
        client.status(RunHandle::new("repo", "run")),
        Err(ClientError::Transport(TransportError::Unavailable))
    );
}

fn fake_client(transport: RecordingTransport) -> RondoCoreClient<RecordingTransport> {
    must_ok(RondoCoreClient::with_transport(
        "http://127.0.0.1:4400",
        transport,
    ))
}

fn submit_success(deduplicated: bool) -> Value {
    json!({
        "surface": "rondo.core/v1",
        "service_id": "rondo-core",
        "repo_id": "nopal.repo/v1:opaque",
        "plot_id": "TASK-52",
        "run_id": "run-opaque",
        "status": "running",
        "event_cursor": "rondo.core/v1:0",
        "deduplicated": deduplicated,
        "future": {"additive": true}
    })
}

fn run_status_success(plot_id: Option<&str>, last_event: Value) -> Value {
    let mut response = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "run_id": "run",
        "status": "running",
        "last_event": last_event,
        "evidence_pointers": [],
        "event_cursor": "rondo.core/v1:0"
    });
    if let Some(plot_id) = plot_id {
        response["plot_id"] = json!(plot_id);
    }
    response
}

fn run_events_success(plot_id: Option<&str>, events: Vec<Value>) -> Value {
    let mut response = json!({
        "surface": "rondo.core/v1",
        "repo_id": "repo",
        "run_id": "run",
        "events": events,
        "next_event_cursor": format!("rondo.core/v1:{}", events.len()),
        "has_more": false
    });
    if let Some(plot_id) = plot_id {
        response["plot_id"] = json!(plot_id);
    }
    response
}

fn run_status_event(repo_id: &str, plot_id: Option<&str>, run_id: &str) -> Value {
    let mut event = json!({
        "type": "rondo.run.status_changed",
        "sequence": 1,
        "repo_id": repo_id,
        "run_id": run_id,
        "status": "running",
        "namespace": {"repo_id": repo_id, "run_id": run_id}
    });
    if let Some(plot_id) = plot_id {
        event["plot_id"] = json!(plot_id);
        event["namespace"]["plot_id"] = json!(plot_id);
    }
    event
}

fn must_ok<T, E: std::fmt::Debug>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("expected success, got {error:?}"),
    }
}

fn must_err<T: std::fmt::Debug, E>(result: Result<T, E>) -> E {
    match result {
        Ok(value) => panic!("expected error, got {value:?}"),
        Err(error) => error,
    }
}

struct OneShotServer {
    base_url: String,
    request_rx: mpsc::Receiver<String>,
    handle: thread::JoinHandle<()>,
}

impl OneShotServer {
    fn respond(status: u16, body: &str, headers: &[(&str, &str)], delay: Duration) -> Self {
        Self::respond_bytes(status, body.as_bytes(), headers, delay)
    }

    fn respond_bytes(status: u16, body: &[u8], headers: &[(&str, &str)], delay: Duration) -> Self {
        let reason = match status {
            200 => "OK",
            202 => "Accepted",
            302 => "Found",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            409 => "Conflict",
            422 => "Unprocessable Content",
            429 => "Too Many Requests",
            503 => "Service Unavailable",
            _ => "Test",
        };
        let mut response = format!(
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
            body.len()
        );
        for (name, value) in headers {
            response.push_str(&format!("{name}: {value}\r\n"));
        }
        response.push_str("\r\n");

        let mut response_bytes = response.into_bytes();
        response_bytes.extend_from_slice(body);
        Self::raw(&response_bytes, delay)
    }

    fn raw(response: &[u8], delay: Duration) -> Self {
        let listener = match TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(error) => panic!("failed to bind loopback test server: {error}"),
        };
        let address = match listener.local_addr() {
            Ok(address) => address,
            Err(error) => panic!("failed to inspect loopback test server: {error}"),
        };
        let response = response.to_owned();
        let (request_tx, request_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let (mut stream, _) = match listener.accept() {
                Ok(connection) => connection,
                Err(error) => panic!("failed to accept loopback request: {error}"),
            };
            let request = read_http_request(&mut stream);
            let _ = request_tx.send(request);
            thread::sleep(delay);
            let _ = stream.write_all(&response);
        });

        Self {
            base_url: format!("http://{address}"),
            request_rx,
            handle,
        }
    }

    fn request(self) -> String {
        let request = match self.request_rx.recv_timeout(Duration::from_secs(2)) {
            Ok(request) => request,
            Err(error) => panic!("test server did not observe request: {error}"),
        };
        match self.handle.join() {
            Ok(()) => request,
            Err(_) => panic!("loopback test server panicked"),
        }
    }
}

fn read_http_request(stream: &mut impl Read) -> String {
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 1024];
    let mut expected_length = None;

    loop {
        let count = match stream.read(&mut buffer) {
            Ok(0) => break,
            Ok(count) => count,
            Err(error) => panic!("failed to read loopback request: {error}"),
        };
        bytes.extend_from_slice(&buffer[..count]);

        if expected_length.is_none()
            && let Some(header_end) = find_header_end(&bytes)
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

    match String::from_utf8(bytes) {
        Ok(request) => request,
        Err(error) => panic!("loopback request was not UTF-8: {error}"),
    }
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}
