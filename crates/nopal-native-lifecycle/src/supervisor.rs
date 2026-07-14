//! Framework-neutral ownership and activation of the native application host.

use std::fmt;
use std::sync::{Mutex, MutexGuard, TryLockError};
use std::thread;
use std::time::Duration;

use crate::activation::ActivationDeadline;

const ACTIVATION_LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(1);
const POISONED_ACTIVATION_TARGET: &str =
    "native activation target is unavailable after a prior panic";

/// A completed native host action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeApplicationAck {
    /// The native host completed its focus operation.
    Focused,
    /// The native host completed its reopen operation.
    Reopened,
}

/// An honest reason a native startup or action could not complete.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeApplicationUnavailable {
    message: String,
}

impl NativeApplicationUnavailable {
    /// Creates a visible diagnostic without treating it as a successful acknowledgement.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }

    /// Returns the diagnostic supplied by the failing boundary.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for NativeApplicationUnavailable {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for NativeApplicationUnavailable {}

/// The renderer-specific primary application behind the lifecycle seam.
///
/// Implementations own Core loading and all window, feed, Composer, Session runtime,
/// and Terminal construction. A secondary instance never receives this interface.
pub trait NativeApplicationHost: Send {
    /// Decides whether to focus or reopen, completes that operation, then returns its result.
    ///
    /// Implementations must honor the shared deadline while dispatching renderer work.
    fn activate(
        &mut self,
        deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable>;
}

/// The sole injected boundary allowed to construct a primary application host.
pub trait NativeApplicationHostFactory: Send + Sync {
    type Host: NativeApplicationHost;

    /// Loads Core and constructs the complete primary host, or reports why it is unavailable.
    fn create_host(&self) -> Result<Self::Host, NativeApplicationUnavailable>;
}

/// Transport-only forwarding used after instance acquisition selected a secondary.
pub trait SecondaryActivationForwarder {
    /// Requests activation from the already-running primary and waits for its completed ack.
    fn forward(&self) -> Result<NativeApplicationAck, NativeApplicationUnavailable>;
}

/// Serialized activation capability exposed by the sole primary application.
///
/// Transport depends on this narrow seam so it can serve an already-composed
/// application without knowing how its host was constructed or who owns its
/// singleton lease.
pub trait PrimaryActivationService: Send + Sync {
    /// Completes one focus or reopen action before returning its acknowledgement.
    fn activate_primary(
        &self,
        deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable>;
}

/// Mutex-owned activation service for an already-composed application or host.
///
/// The supplied function is usually a method pointer such as the composed
/// application's `activate` method. All callers share one owned target and host
/// actions are serialized without introducing another construction boundary.
pub struct SerializedPrimaryActivation<T> {
    target: Mutex<T>,
    activate: fn(
        &mut T,
        ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable>,
}

impl<T> SerializedPrimaryActivation<T> {
    /// Takes ownership of one target and the method used to activate it.
    pub fn new(
        target: T,
        activate: fn(
            &mut T,
            ActivationDeadline,
        ) -> Result<NativeApplicationAck, NativeApplicationUnavailable>,
    ) -> Self {
        Self {
            target: Mutex::new(target),
            activate,
        }
    }

    fn lock_target(
        &self,
        deadline: ActivationDeadline,
    ) -> Result<MutexGuard<'_, T>, NativeApplicationUnavailable> {
        lock_activation_target(&self.target, deadline)
    }
}

impl<T> PrimaryActivationService for SerializedPrimaryActivation<T>
where
    T: Send,
{
    fn activate_primary(
        &self,
        deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        let mut target = self.lock_target(deadline)?;
        let result = (self.activate)(&mut target, deadline);
        ensure_deadline_open(deadline)?;
        result
    }
}

/// The outcome of asking this process to own the primary host.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeApplicationLaunchOutcome {
    /// The one host was constructed and is ready.
    Ready,
    /// This supervisor already owns the ready host.
    AlreadyReady,
    /// Startup failed and the supervisor remains fail-closed.
    Unavailable(NativeApplicationUnavailable),
}

enum PrimaryState<H> {
    NotStarted,
    Ready(H),
    Unavailable(NativeApplicationUnavailable),
}

/// Serializes startup and host actions for one acquired primary instance.
///
/// The OS instance lock remains the singleton authority. This supervisor begins only after
/// acquisition yields the primary lease. A caller holding a secondary stream uses
/// [`Self::forward_secondary`], which never examines the factory or primary state.
pub struct NativeApplicationSupervisor<F>
where
    F: NativeApplicationHostFactory,
{
    factory: F,
    primary: Mutex<PrimaryState<F::Host>>,
}

impl<F> NativeApplicationSupervisor<F>
where
    F: NativeApplicationHostFactory,
{
    /// Creates a cold supervisor without constructing any primary resources.
    pub fn new(factory: F) -> Self {
        Self {
            factory,
            primary: Mutex::new(PrimaryState::NotStarted),
        }
    }

    /// Constructs the primary host at most once, serializing concurrent callers.
    pub fn launch_primary(&self) -> NativeApplicationLaunchOutcome {
        let mut primary = match self.lock_primary() {
            Ok(primary) => primary,
            Err(reason) => return NativeApplicationLaunchOutcome::Unavailable(reason),
        };
        match &*primary {
            PrimaryState::Ready(_) => return NativeApplicationLaunchOutcome::AlreadyReady,
            PrimaryState::Unavailable(reason) => {
                return NativeApplicationLaunchOutcome::Unavailable(reason.clone());
            }
            PrimaryState::NotStarted => {}
        }

        match self.factory.create_host() {
            Ok(host) => {
                *primary = PrimaryState::Ready(host);
                NativeApplicationLaunchOutcome::Ready
            }
            Err(reason) => {
                *primary = PrimaryState::Unavailable(reason.clone());
                NativeApplicationLaunchOutcome::Unavailable(reason)
            }
        }
    }

    /// Lets the ready host choose and complete its activation before acknowledging it.
    pub fn activate_primary(
        &self,
        deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        let mut primary = lock_activation_target(&self.primary, deadline)?;
        let host = match &mut *primary {
            PrimaryState::Ready(host) => host,
            PrimaryState::Unavailable(reason) => return Err(reason.clone()),
            PrimaryState::NotStarted => {
                return Err(NativeApplicationUnavailable::new(
                    "native application primary has not started",
                ));
            }
        };

        let result = host.activate(deadline);
        ensure_deadline_open(deadline)?;
        result
    }

    /// Forwards from a secondary without loading or constructing any primary resources.
    pub fn forward_secondary<T>(
        &self,
        forwarder: &T,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable>
    where
        T: SecondaryActivationForwarder + ?Sized,
    {
        forwarder.forward()
    }

    /// Reports readiness without changing startup state.
    pub fn is_primary_ready(&self) -> bool {
        self.lock_primary()
            .is_ok_and(|primary| matches!(&*primary, PrimaryState::Ready(_)))
    }

    fn lock_primary(
        &self,
    ) -> Result<MutexGuard<'_, PrimaryState<F::Host>>, NativeApplicationUnavailable> {
        self.primary
            .lock()
            .map_err(|_| NativeApplicationUnavailable::new(POISONED_ACTIVATION_TARGET))
    }
}

impl<F> PrimaryActivationService for NativeApplicationSupervisor<F>
where
    F: NativeApplicationHostFactory,
{
    fn activate_primary(
        &self,
        deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        NativeApplicationSupervisor::activate_primary(self, deadline)
    }
}

fn lock_activation_target<T>(
    mutex: &Mutex<T>,
    deadline: ActivationDeadline,
) -> Result<MutexGuard<'_, T>, NativeApplicationUnavailable> {
    loop {
        match mutex.try_lock() {
            Ok(target) => return Ok(target),
            Err(TryLockError::Poisoned(_)) => {
                return Err(NativeApplicationUnavailable::new(
                    POISONED_ACTIVATION_TARGET,
                ));
            }
            Err(TryLockError::WouldBlock) => {
                let remaining = deadline
                    .remaining()
                    .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
                thread::sleep(ACTIVATION_LOCK_RETRY_INTERVAL.min(remaining));
            }
        }
    }
}

fn ensure_deadline_open(deadline: ActivationDeadline) -> Result<(), NativeApplicationUnavailable> {
    deadline
        .remaining()
        .map(|_| ())
        .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::{
        NativeApplicationAck, NativeApplicationHost, NativeApplicationHostFactory,
        NativeApplicationLaunchOutcome, NativeApplicationSupervisor, NativeApplicationUnavailable,
        PrimaryActivationService, SecondaryActivationForwarder, SerializedPrimaryActivation,
        ensure_deadline_open,
    };
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex, mpsc};
    use std::thread;
    use std::time::Duration;
    use std::{panic::AssertUnwindSafe, panic::catch_unwind};

    use crate::activation::ActivationDeadline;

    fn deadline() -> ActivationDeadline {
        ActivationDeadline::after(Duration::from_secs(2)).expect("valid activation deadline")
    }

    #[derive(Clone)]
    struct RecordingFactory {
        starts: Arc<AtomicUsize>,
        host: Result<RecordingHost, NativeApplicationUnavailable>,
    }

    impl NativeApplicationHostFactory for RecordingFactory {
        type Host = RecordingHost;

        fn create_host(&self) -> Result<Self::Host, NativeApplicationUnavailable> {
            self.starts.fetch_add(1, Ordering::SeqCst);
            self.host.clone()
        }
    }

    #[derive(Clone)]
    struct RecordingHost {
        events: Arc<Mutex<Vec<&'static str>>>,
        activation_result: Result<NativeApplicationAck, NativeApplicationUnavailable>,
        active_actions: Arc<AtomicUsize>,
        maximum_active_actions: Arc<AtomicUsize>,
        activation_delay: Duration,
    }

    impl RecordingHost {
        fn available() -> Self {
            Self {
                events: Arc::new(Mutex::new(Vec::new())),
                activation_result: Ok(NativeApplicationAck::Focused),
                active_actions: Arc::new(AtomicUsize::new(0)),
                maximum_active_actions: Arc::new(AtomicUsize::new(0)),
                activation_delay: Duration::from_millis(5),
            }
        }

        fn returning(ack: NativeApplicationAck) -> Self {
            Self {
                activation_result: Ok(ack),
                ..Self::available()
            }
        }

        fn record_activation(&self) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
            let active = self.active_actions.fetch_add(1, Ordering::SeqCst) + 1;
            self.maximum_active_actions
                .fetch_max(active, Ordering::SeqCst);
            thread::sleep(self.activation_delay);
            if let Ok(mut events) = self.events.lock() {
                match self.activation_result {
                    Ok(NativeApplicationAck::Focused) => events.push("focus complete"),
                    Ok(NativeApplicationAck::Reopened) => events.push("reopen complete"),
                    Err(_) => events.push("activation failed"),
                }
            }
            self.active_actions.fetch_sub(1, Ordering::SeqCst);
            self.activation_result.clone()
        }
    }

    impl NativeApplicationHost for RecordingHost {
        fn activate(
            &mut self,
            _deadline: ActivationDeadline,
        ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
            self.record_activation()
        }
    }

    struct RecordingForwarder {
        calls: Arc<AtomicUsize>,
    }

    impl SecondaryActivationForwarder for RecordingForwarder {
        fn forward(&self) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(NativeApplicationAck::Focused)
        }
    }

    fn unavailable(message: &str) -> NativeApplicationUnavailable {
        NativeApplicationUnavailable::new(message)
    }

    fn supervisor(
        host: Result<RecordingHost, NativeApplicationUnavailable>,
    ) -> (
        NativeApplicationSupervisor<RecordingFactory>,
        Arc<AtomicUsize>,
    ) {
        let starts = Arc::new(AtomicUsize::new(0));
        (
            NativeApplicationSupervisor::new(RecordingFactory {
                starts: Arc::clone(&starts),
                host,
            }),
            starts,
        )
    }

    #[test]
    fn secondary_activation_forwards_without_constructing_primary_host() {
        let (supervisor, starts) = supervisor(Ok(RecordingHost::available()));
        let calls = Arc::new(AtomicUsize::new(0));
        let forwarder = RecordingForwarder {
            calls: Arc::clone(&calls),
        };

        let outcome = supervisor.forward_secondary(&forwarder);

        assert_eq!(outcome, Ok(NativeApplicationAck::Focused));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(starts.load(Ordering::SeqCst), 0);
        assert!(!supervisor.is_primary_ready());
    }

    #[test]
    fn repeated_primary_launch_constructs_at_most_one_host() {
        let (supervisor, starts) = supervisor(Ok(RecordingHost::available()));

        assert_eq!(
            supervisor.launch_primary(),
            NativeApplicationLaunchOutcome::Ready
        );
        assert_eq!(
            supervisor.launch_primary(),
            NativeApplicationLaunchOutcome::AlreadyReady
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn focused_ack_is_returned_only_after_focus_completes() {
        let host = RecordingHost::available();
        let events = Arc::clone(&host.events);
        let (supervisor, _) = supervisor(Ok(host));
        assert_eq!(
            supervisor.launch_primary(),
            NativeApplicationLaunchOutcome::Ready
        );

        let ack = supervisor.activate_primary(deadline());

        assert_eq!(ack, Ok(NativeApplicationAck::Focused));
        assert_eq!(
            events.lock().map(|events| events.clone()).ok(),
            Some(vec!["focus complete"])
        );
    }

    #[test]
    fn reopened_ack_is_returned_only_after_reopen_completes() {
        let host = RecordingHost::returning(NativeApplicationAck::Reopened);
        let events = Arc::clone(&host.events);
        let (supervisor, _) = supervisor(Ok(host));
        assert_eq!(
            supervisor.launch_primary(),
            NativeApplicationLaunchOutcome::Ready
        );

        let ack = supervisor.activate_primary(deadline());

        assert_eq!(ack, Ok(NativeApplicationAck::Reopened));
        assert_eq!(
            events.lock().map(|events| events.clone()).ok(),
            Some(vec!["reopen complete"])
        );
    }

    #[test]
    fn failed_host_action_returns_unavailable_instead_of_false_ack() {
        let host = RecordingHost {
            activation_result: Err(unavailable("host action failed")),
            ..RecordingHost::available()
        };
        let (supervisor, _) = supervisor(Ok(host));
        assert_eq!(
            supervisor.launch_primary(),
            NativeApplicationLaunchOutcome::Ready
        );

        assert_eq!(
            supervisor.activate_primary(deadline()),
            Err(unavailable("host action failed"))
        );
    }

    #[test]
    fn completed_host_action_after_deadline_never_returns_a_false_ack() {
        let host = RecordingHost {
            activation_delay: Duration::from_millis(30),
            ..RecordingHost::available()
        };
        let (supervisor, _) = supervisor(Ok(host));
        assert_eq!(
            supervisor.launch_primary(),
            NativeApplicationLaunchOutcome::Ready
        );
        let deadline = ActivationDeadline::after(Duration::from_millis(5))
            .expect("valid short activation deadline");

        let error = supervisor
            .activate_primary(deadline)
            .expect_err("late host result must be unavailable");

        assert_eq!(error.message(), "native activation total deadline elapsed");
    }

    #[test]
    fn startup_failure_is_sticky_and_never_reports_ready() {
        let (supervisor, starts) = supervisor(Err(unavailable("Core unavailable")));

        assert_eq!(
            supervisor.launch_primary(),
            NativeApplicationLaunchOutcome::Unavailable(unavailable("Core unavailable"))
        );
        assert!(!supervisor.is_primary_ready());
        assert_eq!(
            supervisor.launch_primary(),
            NativeApplicationLaunchOutcome::Unavailable(unavailable("Core unavailable"))
        );
        assert_eq!(
            supervisor.activate_primary(deadline()),
            Err(unavailable("Core unavailable"))
        );
        assert_eq!(starts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn concurrent_primary_launches_construct_only_one_host() {
        let (supervisor, starts) = supervisor(Ok(RecordingHost::available()));
        let supervisor = Arc::new(supervisor);
        let barrier = Arc::new(Barrier::new(3));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let supervisor = Arc::clone(&supervisor);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                supervisor.launch_primary()
            }));
        }
        barrier.wait();

        let outcomes: Vec<_> = workers
            .into_iter()
            .filter_map(|worker| worker.join().ok())
            .collect();

        assert_eq!(starts.load(Ordering::SeqCst), 1);
        assert!(outcomes.contains(&NativeApplicationLaunchOutcome::Ready));
        assert!(outcomes.contains(&NativeApplicationLaunchOutcome::AlreadyReady));
    }

    #[test]
    fn concurrent_activations_are_serialized() {
        let host = RecordingHost::available();
        let maximum_active_actions = Arc::clone(&host.maximum_active_actions);
        let (supervisor, _) = supervisor(Ok(host));
        assert_eq!(
            supervisor.launch_primary(),
            NativeApplicationLaunchOutcome::Ready
        );
        let supervisor = Arc::new(supervisor);
        let barrier = Arc::new(Barrier::new(3));
        let (sender, receiver) = mpsc::channel();
        for _ in 0..2 {
            let supervisor = Arc::clone(&supervisor);
            let barrier = Arc::clone(&barrier);
            let sender = sender.clone();
            thread::spawn(move || {
                barrier.wait();
                let result = supervisor.activate_primary(deadline());
                let _ = sender.send(result);
            });
        }
        drop(sender);
        barrier.wait();

        let results: Vec<_> = receiver.iter().collect();

        assert_eq!(results.len(), 2);
        assert!(
            results
                .iter()
                .all(|result| *result == Ok(NativeApplicationAck::Focused))
        );
        assert_eq!(maximum_active_actions.load(Ordering::SeqCst), 1);
    }

    struct BlockingTarget {
        started: Option<mpsc::Sender<()>>,
        release: Option<mpsc::Receiver<()>>,
    }

    fn activate_blocking_target(
        target: &mut BlockingTarget,
        deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        if let Some(started) = target.started.take() {
            let _ = started.send(());
        }
        if let Some(release) = target.release.take() {
            release
                .recv_timeout(Duration::from_secs(2))
                .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
        }
        ensure_deadline_open(deadline)?;
        Ok(NativeApplicationAck::Focused)
    }

    #[test]
    fn serialized_activation_lock_wait_honors_the_shared_deadline() {
        let (started_sender, started_receiver) = mpsc::channel();
        let (release_sender, release_receiver) = mpsc::channel();
        let service = Arc::new(SerializedPrimaryActivation::new(
            BlockingTarget {
                started: Some(started_sender),
                release: Some(release_receiver),
            },
            activate_blocking_target,
        ));
        let worker_service = Arc::clone(&service);
        let worker = thread::spawn(move || worker_service.activate_primary(deadline()));
        started_receiver
            .recv_timeout(Duration::from_secs(1))
            .expect("first activation acquired the serialized target");

        let short_deadline = ActivationDeadline::after(Duration::from_millis(10))
            .expect("valid short activation deadline");
        let contender_service = Arc::clone(&service);
        let (result_sender, result_receiver) = mpsc::channel();
        let contender = thread::spawn(move || {
            let _ = result_sender.send(contender_service.activate_primary(short_deadline));
        });
        let result = result_receiver.recv_timeout(Duration::from_secs(1));
        let _ = release_sender.send(());
        let worker_result = worker.join().expect("first activation thread completes");
        contender
            .join()
            .expect("second activation thread completes");
        let error = result
            .expect("second activation returned within the generous watchdog")
            .expect_err("second activation must not outlive its lock-wait deadline");

        assert_eq!(error.message(), "native activation total deadline elapsed");
        assert_eq!(worker_result, Ok(NativeApplicationAck::Focused));
    }

    struct PanicOnceTarget {
        calls: Arc<AtomicUsize>,
    }

    fn activate_panicking_target(
        target: &mut PanicOnceTarget,
        _deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        if target.calls.fetch_add(1, Ordering::SeqCst) == 0 {
            panic!("renderer mutated state and panicked");
        }
        Ok(NativeApplicationAck::Focused)
    }

    #[test]
    fn panicking_activation_target_is_never_invoked_again() {
        let calls = Arc::new(AtomicUsize::new(0));
        let service = SerializedPrimaryActivation::new(
            PanicOnceTarget {
                calls: Arc::clone(&calls),
            },
            activate_panicking_target,
        );

        let first = catch_unwind(AssertUnwindSafe(|| service.activate_primary(deadline())));
        assert!(first.is_err(), "fixture must reproduce the renderer panic");
        let second = service
            .activate_primary(deadline())
            .expect_err("a poisoned renderer must remain sticky-unavailable");

        assert_eq!(
            second.message(),
            "native activation target is unavailable after a prior panic"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
