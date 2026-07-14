use std::collections::VecDeque;
use std::io::{self, Cursor, Read, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use nopal_feed_client::field::FieldSnapshot;
use nopal_native_lifecycle::activation::{
    ActivationAck, ActivationAckValidator, ActivationDeadline, ActivationOutcome,
    ActivationProtocolError, ActivationRequest, ActivationRequestValidator,
    MAX_ACTIVATION_FRAME_BYTES, encode_ack, encode_request,
};
use nopal_native_lifecycle::application::{
    CoreFieldSnapshotSource, NativeApplicationStart, NativePrimaryApplication,
    NativeRestorePreferenceSource, OwnedResourceRecoveryReport, PrimaryStartupRecovery,
    ResolvedNativeApplicationHostFactory, RestorePreferenceNotice, start_native_application,
};
use nopal_native_lifecycle::current_field::CurrentCoreFieldAuthority;
use nopal_native_lifecycle::instance::{InstanceAcquisition, InstancePlatform};
use nopal_native_lifecycle::preferences::RestorePreferenceReadOutcome;
use nopal_native_lifecycle::reconcile::RestoreResolution;
use nopal_native_lifecycle::recovery::RecoveryReconcileOutcome;
use nopal_native_lifecycle::state_root::{CanonicalStateRoot, NativeInstanceScope, ReleaseChannel};
use nopal_native_lifecycle::supervisor::{
    NativeApplicationAck, NativeApplicationHost, NativeApplicationHostFactory,
    NativeApplicationLaunchOutcome, NativeApplicationSupervisor, NativeApplicationUnavailable,
    PrimaryActivationService, SecondaryActivationForwarder, SerializedPrimaryActivation,
};
use nopal_native_lifecycle::transport::{
    ActivationTransportError, exchange_activation_request, generate_activation_request,
    serve_activation,
};

const SCOPE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const OTHER_SCOPE: &str = "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const NONCE: &str = "0123456789abcdef0123456789abcdef";
const OTHER_NONCE: &str = "1123456789abcdef0123456789abcdef";

fn activation_deadline() -> ActivationDeadline {
    must(
        ActivationDeadline::after(Duration::from_secs(2)),
        "create activation deadline",
    )
}

fn must<T, E: std::fmt::Display>(result: Result<T, E>, context: &str) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("{context}: {error}"),
    }
}

#[derive(Clone)]
struct CountingFactory {
    starts: Arc<AtomicUsize>,
    host_result: Result<CountingHost, NativeApplicationUnavailable>,
}

impl NativeApplicationHostFactory for CountingFactory {
    type Host = CountingHost;

    fn create_host(&self) -> Result<Self::Host, NativeApplicationUnavailable> {
        self.starts.fetch_add(1, Ordering::SeqCst);
        self.host_result.clone()
    }
}

#[derive(Clone)]
struct CountingHost {
    activations: Arc<AtomicUsize>,
    result: Result<NativeApplicationAck, NativeApplicationUnavailable>,
}

impl NativeApplicationHost for CountingHost {
    fn activate(
        &mut self,
        _deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        self.activations.fetch_add(1, Ordering::SeqCst);
        self.result.clone()
    }
}

fn supervisor(
    result: Result<NativeApplicationAck, NativeApplicationUnavailable>,
) -> (
    NativeApplicationSupervisor<CountingFactory>,
    Arc<AtomicUsize>,
    Arc<AtomicUsize>,
) {
    let starts = Arc::new(AtomicUsize::new(0));
    let activations = Arc::new(AtomicUsize::new(0));
    let supervisor = NativeApplicationSupervisor::new(CountingFactory {
        starts: Arc::clone(&starts),
        host_result: Ok(CountingHost {
            activations: Arc::clone(&activations),
            result,
        }),
    });
    assert_eq!(
        supervisor.launch_primary(),
        NativeApplicationLaunchOutcome::Ready
    );
    (supervisor, starts, activations)
}

struct ScriptedIo {
    read: Cursor<Vec<u8>>,
    written: Vec<u8>,
}

impl ScriptedIo {
    fn new(read: Vec<u8>) -> Self {
        Self {
            read: Cursor::new(read),
            written: Vec::new(),
        }
    }
}

impl Read for ScriptedIo {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.read.read(buffer)
    }
}

impl Write for ScriptedIo {
    fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
        self.written.extend_from_slice(buffer);
        Ok(buffer.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn request() -> ActivationRequest {
    must(
        ActivationRequest::new(SCOPE, NONCE),
        "valid activation fixture",
    )
}

#[test]
fn generated_client_nonces_are_random_exact_lowercase_hex() {
    let first = must(generate_activation_request(SCOPE), "generate first request");
    let second = must(
        generate_activation_request(SCOPE),
        "generate second request",
    );

    assert_eq!(first.nonce().len(), 32);
    assert!(
        first
            .nonce()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    );
    assert_ne!(first.nonce(), second.nonce());
}

#[test]
fn exchange_requires_a_complete_matching_ack_frame() {
    let request = request();
    let ack = must(
        encode_ack(&ActivationAck::for_request(
            &request,
            ActivationOutcome::Focused,
        )),
        "encode ack",
    );
    let mut transport = ScriptedIo::new(ack);

    assert_eq!(
        must(
            exchange_activation_request(&mut transport, &request),
            "exchange activation",
        ),
        ActivationOutcome::Focused
    );
    assert_eq!(
        transport.written,
        must(encode_request(&request), "encode request")
    );

    for incomplete in [Vec::new(), b"{}".to_vec()] {
        let mut transport = ScriptedIo::new(incomplete);
        assert!(matches!(
            exchange_activation_request(&mut transport, &request),
            Err(ActivationTransportError::IncompleteFrame)
        ));
    }
}

#[test]
fn secondary_rejects_ack_scope_nonce_and_frame_contract_drift() {
    let request = request();
    let wrong_scope_request = must(
        ActivationRequest::new(OTHER_SCOPE, NONCE),
        "valid wrong-scope fixture",
    );
    let wrong_nonce_request = must(
        ActivationRequest::new(SCOPE, OTHER_NONCE),
        "valid wrong-nonce fixture",
    );
    let wrong_scope = must(
        encode_ack(&ActivationAck::for_request(
            &wrong_scope_request,
            ActivationOutcome::Focused,
        )),
        "encode wrong-scope ack",
    );
    let wrong_nonce = must(
        encode_ack(&ActivationAck::for_request(
            &wrong_nonce_request,
            ActivationOutcome::Focused,
        )),
        "encode wrong-nonce ack",
    );

    for (frame, expected) in [
        (wrong_scope, ActivationProtocolError::ScopeMismatch),
        (wrong_nonce, ActivationProtocolError::NonceMismatch),
    ] {
        let mut transport = ScriptedIo::new(frame);
        let error = match exchange_activation_request(&mut transport, &request) {
            Ok(outcome) => panic!("mismatched acknowledgement returned {outcome:?}"),
            Err(error) => error,
        };
        assert!(matches!(error, ActivationTransportError::Protocol(actual) if actual == expected));
    }

    let good = must(
        encode_ack(&ActivationAck::for_request(
            &request,
            ActivationOutcome::Focused,
        )),
        "encode good ack",
    );
    let trailing = must(String::from_utf8(good.clone()), "ack is UTF-8")
        .replace("}\n", "}{}\n")
        .into_bytes();
    for frame in [
        b"not-json\n".to_vec(),
        vec![b'x'; MAX_ACTIVATION_FRAME_BYTES + 1],
        good[..good.len() - 1].to_vec(),
        trailing,
    ] {
        let mut transport = ScriptedIo::new(frame);
        assert!(exchange_activation_request(&mut transport, &request).is_err());
    }
}

#[test]
fn primary_acknowledges_only_the_completed_host_outcome() {
    let (supervisor, starts, activations) = supervisor(Ok(NativeApplicationAck::Reopened));
    let mut validator = must(ActivationRequestValidator::new(SCOPE), "valid scope");
    let mut transport = ScriptedIo::new(must(encode_request(&request()), "encode request"));

    assert_eq!(
        must(
            serve_activation(
                &mut transport,
                &mut validator,
                &supervisor,
                activation_deadline(),
            ),
            "serve activation",
        ),
        ActivationOutcome::Reopened
    );
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(activations.load(Ordering::SeqCst), 1);
    let mut ack_validator = ActivationAckValidator::new(&request());
    assert_eq!(
        must(
            ack_validator.decode_and_validate(&transport.written),
            "validate ack",
        )
        .outcome(),
        ActivationOutcome::Reopened
    );
}

#[test]
fn unavailable_host_action_is_reported_honestly_in_the_ack() {
    let (supervisor, _, activations) = supervisor(Err(NativeApplicationUnavailable::new(
        "window system unavailable",
    )));
    let mut validator = must(ActivationRequestValidator::new(SCOPE), "valid scope");
    let mut transport = ScriptedIo::new(must(encode_request(&request()), "encode request"));

    assert_eq!(
        must(
            serve_activation(
                &mut transport,
                &mut validator,
                &supervisor,
                activation_deadline(),
            ),
            "unavailable is a completed protocol outcome",
        ),
        ActivationOutcome::Unavailable
    );
    assert_eq!(activations.load(Ordering::SeqCst), 1);
    let mut ack_validator = ActivationAckValidator::new(&request());
    assert_eq!(
        must(
            ack_validator.decode_and_validate(&transport.written),
            "validate unavailable ack",
        )
        .outcome(),
        ActivationOutcome::Unavailable
    );
}

struct PanickingActivationService;

impl PrimaryActivationService for PanickingActivationService {
    fn activate_primary(
        &self,
        _deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        panic!("renderer panic fixture")
    }
}

#[test]
fn renderer_panic_is_contained_as_an_unavailable_ack() {
    let mut validator = must(ActivationRequestValidator::new(SCOPE), "valid scope");
    let mut transport = ScriptedIo::new(must(encode_request(&request()), "encode request"));

    assert_eq!(
        must(
            serve_activation(
                &mut transport,
                &mut validator,
                &PanickingActivationService,
                activation_deadline(),
            ),
            "renderer panic is a completed protocol outcome",
        ),
        ActivationOutcome::Unavailable
    );
    let mut ack_validator = ActivationAckValidator::new(&request());
    assert_eq!(
        must(
            ack_validator.decode_and_validate(&transport.written),
            "validate panic acknowledgement",
        )
        .outcome(),
        ActivationOutcome::Unavailable
    );
}

struct PanicOnceActivationTarget {
    calls: Arc<AtomicUsize>,
}

fn activate_panic_once(
    target: &mut PanicOnceActivationTarget,
    _deadline: ActivationDeadline,
) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
    if target.calls.fetch_add(1, Ordering::SeqCst) == 0 {
        panic!("renderer mutated state and panicked");
    }
    Ok(NativeApplicationAck::Focused)
}

#[test]
fn renderer_panic_makes_later_requests_sticky_unavailable() {
    let calls = Arc::new(AtomicUsize::new(0));
    let service = SerializedPrimaryActivation::new(
        PanicOnceActivationTarget {
            calls: Arc::clone(&calls),
        },
        activate_panic_once,
    );
    let mut validator = must(ActivationRequestValidator::new(SCOPE), "valid scope");

    for nonce in [NONCE, OTHER_NONCE] {
        let request = must(
            ActivationRequest::new(SCOPE, nonce),
            "valid panic-sequence request",
        );
        let mut transport = ScriptedIo::new(must(encode_request(&request), "encode request"));
        assert_eq!(
            must(
                serve_activation(
                    &mut transport,
                    &mut validator,
                    &service,
                    activation_deadline(),
                ),
                "panic sequence remains a completed protocol outcome",
            ),
            ActivationOutcome::Unavailable
        );
        let mut ack_validator = ActivationAckValidator::new(&request);
        assert_eq!(
            must(
                ack_validator.decode_and_validate(&transport.written),
                "validate sticky unavailable acknowledgement",
            )
            .outcome(),
            ActivationOutcome::Unavailable
        );
    }

    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

struct SequencedComposedHost {
    activations: Arc<AtomicUsize>,
    outcomes: VecDeque<NativeApplicationAck>,
    _current_field: CurrentCoreFieldAuthority,
}

struct PrimaryOnlyPlatform;

struct UnreachableSecondary;

impl SecondaryActivationForwarder for UnreachableSecondary {
    fn forward(&self) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        Err(NativeApplicationUnavailable::new(
            "primary-only test platform produced a secondary",
        ))
    }
}

impl InstancePlatform for PrimaryOnlyPlatform {
    type Primary = ();
    type Secondary = UnreachableSecondary;

    fn acquire(
        &self,
        _secondary_connect_timeout: Duration,
    ) -> io::Result<InstanceAcquisition<Self::Primary, Self::Secondary>> {
        Ok(InstanceAcquisition::Primary(()))
    }
}

struct MissingPreference;

struct EmptyRecovery;

impl PrimaryStartupRecovery for EmptyRecovery {
    type Error = io::Error;

    fn reconcile_for_scope(
        &mut self,
        _scope: &NativeInstanceScope,
    ) -> Result<RecoveryReconcileOutcome, Self::Error> {
        Ok(RecoveryReconcileOutcome::Empty)
    }
}

impl NativeRestorePreferenceSource for MissingPreference {
    fn read_for_scope(
        &self,
        _scope: &NativeInstanceScope,
    ) -> io::Result<RestorePreferenceReadOutcome> {
        Ok(RestorePreferenceReadOutcome::Missing)
    }
}

struct StaticCoreSnapshot(FieldSnapshot);

impl CoreFieldSnapshotSource for StaticCoreSnapshot {
    fn load_field_snapshot(&self) -> Result<FieldSnapshot, NativeApplicationUnavailable> {
        Ok(self.0.clone())
    }
}

struct SequencedComposedHostFactory {
    constructions: Arc<AtomicUsize>,
    activations: Arc<AtomicUsize>,
}

impl ResolvedNativeApplicationHostFactory for SequencedComposedHostFactory {
    type Host = SequencedComposedHost;

    fn create_host(
        &self,
        _field: &FieldSnapshot,
        _restore: &RestoreResolution,
        _recovery_report: &OwnedResourceRecoveryReport,
        _preference_notice: Option<&RestorePreferenceNotice>,
        current_field: CurrentCoreFieldAuthority,
    ) -> Result<Self::Host, NativeApplicationUnavailable> {
        self.constructions.fetch_add(1, Ordering::SeqCst);
        Ok(SequencedComposedHost {
            activations: Arc::clone(&self.activations),
            outcomes: VecDeque::from([
                NativeApplicationAck::Focused,
                NativeApplicationAck::Reopened,
            ]),
            _current_field: current_field,
        })
    }
}

impl NativeApplicationHost for SequencedComposedHost {
    fn activate(
        &mut self,
        _deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        self.activations.fetch_add(1, Ordering::SeqCst);
        self.outcomes
            .pop_front()
            .ok_or_else(|| NativeApplicationUnavailable::new("no composed host outcome remains"))
    }
}

#[test]
fn one_composed_host_serves_focus_and_reopen_without_duplicate_construction() {
    let constructions = Arc::new(AtomicUsize::new(0));
    let activations = Arc::new(AtomicUsize::new(0));
    let sandbox = must(tempfile::tempdir(), "create composed application sandbox");
    let scope = NativeInstanceScope::new(
        must(
            CanonicalStateRoot::create(sandbox.path().join("state")),
            "create composed application state root",
        ),
        ReleaseChannel::Development,
    );
    let field = must(
        serde_json::from_value(serde_json::json!({
            "kind": "nopal.field/v1",
            "plots": [],
            "entries": [],
        })),
        "create empty Field snapshot",
    );
    let factory = SequencedComposedHostFactory {
        constructions: Arc::clone(&constructions),
        activations: Arc::clone(&activations),
    };
    let mut recovery = EmptyRecovery;
    let start = must(
        start_native_application(
            &scope,
            &PrimaryOnlyPlatform,
            &mut recovery,
            &MissingPreference,
            &StaticCoreSnapshot(field),
            &factory,
            Duration::from_millis(50),
        ),
        "compose primary application",
    );
    let NativeApplicationStart::Primary(application) = start else {
        panic!("primary-only platform returned a secondary");
    };
    let service =
        SerializedPrimaryActivation::new(*application, NativePrimaryApplication::activate);
    let mut validator = must(
        ActivationRequestValidator::new(scope.fingerprint()),
        "valid composed application scope",
    );

    for (nonce, expected) in [
        (NONCE, ActivationOutcome::Focused),
        (OTHER_NONCE, ActivationOutcome::Reopened),
    ] {
        let request = must(
            ActivationRequest::new(scope.fingerprint(), nonce),
            "valid sequenced request",
        );
        let mut transport = ScriptedIo::new(must(encode_request(&request), "encode request"));

        assert_eq!(
            must(
                serve_activation(
                    &mut transport,
                    &mut validator,
                    &service,
                    activation_deadline(),
                ),
                "serve composed host activation",
            ),
            expected
        );
        let mut ack_validator = ActivationAckValidator::new(&request);
        assert_eq!(
            must(
                ack_validator.decode_and_validate(&transport.written),
                "validate composed host acknowledgement",
            )
            .outcome(),
            expected
        );
    }

    assert_eq!(constructions.load(Ordering::SeqCst), 1);
    assert_eq!(activations.load(Ordering::SeqCst), 2);
}

#[test]
fn malformed_oversized_truncated_trailing_and_replayed_requests_fail_closed() {
    let (supervisor, _, activations) = supervisor(Ok(NativeApplicationAck::Focused));
    let valid = must(encode_request(&request()), "encode request");
    let trailing = must(String::from_utf8(valid.clone()), "request is UTF-8")
        .replace("}\n", "}{}\n")
        .into_bytes();
    let fixtures = [
        b"not-json\n".to_vec(),
        vec![b'x'; MAX_ACTIVATION_FRAME_BYTES + 1],
        valid[..valid.len() - 1].to_vec(),
        trailing,
    ];

    for fixture in fixtures {
        let mut validator = must(ActivationRequestValidator::new(SCOPE), "valid scope");
        let mut transport = ScriptedIo::new(fixture);
        assert!(
            serve_activation(
                &mut transport,
                &mut validator,
                &supervisor,
                activation_deadline(),
            )
            .is_err()
        );
        assert!(transport.written.is_empty());
    }
    assert_eq!(activations.load(Ordering::SeqCst), 0);

    let mut validator = must(ActivationRequestValidator::new(SCOPE), "valid scope");
    let mut first = ScriptedIo::new(valid.clone());
    must(
        serve_activation(
            &mut first,
            &mut validator,
            &supervisor,
            activation_deadline(),
        ),
        "accept nonce once",
    );
    let mut replay = ScriptedIo::new(valid);
    assert!(matches!(
        serve_activation(
            &mut replay,
            &mut validator,
            &supervisor,
            activation_deadline(),
        ),
        Err(ActivationTransportError::Protocol(
            ActivationProtocolError::ReplayedNonce
        ))
    ));
    assert!(replay.written.is_empty());
    assert_eq!(activations.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[test]
fn concurrent_duplicate_launch_has_one_primary_and_secondary_gets_completed_ack() {
    use std::sync::{Barrier, mpsc};
    use std::thread;
    use std::time::Duration;

    use nopal_native_lifecycle::instance::{InstanceAcquisition, InstancePlatform};
    use nopal_native_lifecycle::platform::unix::UnixInstanceCoordinator;
    use nopal_native_lifecycle::state_root::{
        CanonicalStateRoot, NativeInstanceScope, ReleaseChannel,
    };
    use nopal_native_lifecycle::supervisor::SecondaryActivationForwarder;
    use nopal_native_lifecycle::transport::{UnixActivationForwarder, serve_unix_activation};

    let sandbox = must(tempfile::tempdir_in("/tmp"), "create socket sandbox");
    let state_root = must(
        CanonicalStateRoot::create(sandbox.path().join("state")),
        "create state root",
    );
    let scope = NativeInstanceScope::new(state_root, ReleaseChannel::Stable);
    let control_root = sandbox.path().join("control");
    let barrier = Arc::new(Barrier::new(3));
    let starts = Arc::new(AtomicUsize::new(0));
    let activations = Arc::new(AtomicUsize::new(0));
    let (sender, receiver) = mpsc::channel();

    let mut joins = Vec::new();
    for _ in 0..2 {
        let scope = scope.clone();
        let control_root = control_root.clone();
        let barrier = Arc::clone(&barrier);
        let starts = Arc::clone(&starts);
        let activations = Arc::clone(&activations);
        let sender = sender.clone();
        joins.push(thread::spawn(move || {
            let coordinator = must(
                UnixInstanceCoordinator::new(scope.clone(), control_root),
                "create coordinator",
            );
            barrier.wait();
            let acquisition = must(
                coordinator.acquire(Duration::from_secs(2)),
                "acquire instance",
            );
            let role = match acquisition {
                InstanceAcquisition::Primary(lease) => {
                    let supervisor = NativeApplicationSupervisor::new(CountingFactory {
                        starts,
                        host_result: Ok(CountingHost {
                            activations,
                            result: Ok(NativeApplicationAck::Focused),
                        }),
                    });
                    assert_eq!(
                        supervisor.launch_primary(),
                        NativeApplicationLaunchOutcome::Ready
                    );
                    let stream = must(lease.accept(), "accept secondary");
                    let mut validator = must(
                        ActivationRequestValidator::new(scope.fingerprint()),
                        "valid scope",
                    );
                    let outcome = must(
                        serve_unix_activation(
                            stream,
                            &mut validator,
                            &supervisor,
                            Duration::from_secs(2),
                        ),
                        "serve secondary",
                    );
                    assert_eq!(outcome, ActivationOutcome::Focused);
                    "primary"
                }
                InstanceAcquisition::Secondary(stream) => {
                    let forwarder = must(
                        UnixActivationForwarder::new(
                            stream,
                            scope.fingerprint(),
                            Duration::from_secs(2),
                        ),
                        "create forwarder",
                    );
                    assert_eq!(
                        must(forwarder.forward(), "forward to primary"),
                        NativeApplicationAck::Focused
                    );
                    "secondary"
                }
            };
            must(sender.send(role), "report role");
        }));
    }

    barrier.wait();
    drop(sender);
    let mut roles: Vec<_> = receiver.iter().collect();
    roles.sort_unstable();
    for join in joins {
        if let Err(error) = join.join() {
            panic!("launch thread failed: {error:?}");
        }
    }

    assert_eq!(roles, vec!["primary", "secondary"]);
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert_eq!(activations.load(Ordering::SeqCst), 1);
}

#[cfg(unix)]
#[test]
fn unix_server_slow_drip_cannot_extend_the_total_activation_deadline() {
    use std::os::unix::net::UnixStream;
    use std::thread;
    use std::time::{Duration, Instant};

    use nopal_native_lifecycle::transport::serve_unix_activation;

    let (mut peer, stream) = must(UnixStream::pair(), "create Unix stream pair");
    let writer = thread::spawn(move || {
        for byte in b"{\"kind\":\"x" {
            if peer.write_all(&[*byte]).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(350));
        }
    });
    let (supervisor, _, activations) = supervisor(Ok(NativeApplicationAck::Focused));
    let mut validator = must(ActivationRequestValidator::new(SCOPE), "valid scope");
    let started = Instant::now();
    let result = serve_unix_activation(
        stream,
        &mut validator,
        &supervisor,
        Duration::from_millis(500),
    );
    let elapsed = started.elapsed();
    if let Err(error) = writer.join() {
        panic!("slow writer thread failed: {error:?}");
    }

    assert!(
        matches!(
            result,
            Err(ActivationTransportError::Io(ref error))
                if matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut | io::ErrorKind::WouldBlock
                )
        ),
        "slow drip should hit the total I/O deadline, got {result:?}"
    );
    assert!(
        elapsed >= Duration::from_millis(300),
        "server failed before exercising the 500ms total budget: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "slow drip extended a 500ms server budget to {elapsed:?}"
    );
    assert_eq!(activations.load(Ordering::SeqCst), 0);
}

#[cfg(unix)]
#[test]
fn unix_client_slow_drip_ack_cannot_extend_the_total_activation_deadline() {
    use std::os::unix::net::UnixStream;
    use std::thread;
    use std::time::{Duration, Instant};

    use nopal_native_lifecycle::supervisor::SecondaryActivationForwarder;
    use nopal_native_lifecycle::transport::{UnixActivationForwarder, read_activation_frame};

    let (stream, mut peer) = must(UnixStream::pair(), "create Unix stream pair");
    let server = thread::spawn(move || {
        must(read_activation_frame(&mut peer), "read activation request");
        for byte in b"{\"kind\":\"x" {
            if peer.write_all(&[*byte]).is_err() {
                return;
            }
            thread::sleep(Duration::from_millis(350));
        }
    });
    let forwarder = must(
        UnixActivationForwarder::new(stream, SCOPE, Duration::from_millis(500)),
        "create Unix activation forwarder",
    );
    let started = Instant::now();
    let result = forwarder.forward();
    let elapsed = started.elapsed();
    if let Err(error) = server.join() {
        panic!("slow server thread failed: {error:?}");
    }

    assert!(
        result.is_err(),
        "slow acknowledgement unexpectedly succeeded"
    );
    assert!(
        elapsed >= Duration::from_millis(300),
        "client failed before exercising the 500ms total budget: {elapsed:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "slow drip extended a 500ms client budget to {elapsed:?}"
    );
}

#[cfg(unix)]
#[test]
fn unix_activation_rejects_zero_and_unrepresentable_total_deadlines() {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use nopal_native_lifecycle::transport::UnixActivationForwarder;

    for timeout in [Duration::ZERO, Duration::MAX] {
        let (stream, _peer) = must(UnixStream::pair(), "create Unix stream pair");
        let error = match UnixActivationForwarder::new(stream, SCOPE, timeout) {
            Ok(_) => panic!("invalid timeout {timeout:?} created a forwarder"),
            Err(error) => error,
        };
        assert!(matches!(
            error,
            ActivationTransportError::Io(ref error)
                if error.kind() == io::ErrorKind::InvalidInput
        ));
    }
}
