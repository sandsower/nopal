//! Application-scoped resource ownership for one native Field session.

use std::fmt;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::time::{Duration, Instant};

use crate::recovery::{
    DurableRecoveryEntry, RecoveryDeadline, RecoveryJournalStore, RecoveryJournalUpdateOutcome,
};

/// The best-effort shutdown budget used when callers do not provide one.
pub const DEFAULT_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(30);

/// The application-level resource roles whose ownership must be explicit.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceKind {
    /// A Core-owned or application-owned Session connection/runtime.
    Session,
    /// A child or externally managed process.
    Process,
    /// An input or output pipe.
    Pipe,
    /// A listener or connected socket.
    Socket,
    /// A worker, watcher, or other background resource.
    BackgroundResource,
}

/// Whether native Field is responsible for closing a tracked resource.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceOwnership {
    /// Native Field acquired the resource and must close it on application shutdown.
    ApplicationOwned,
    /// Another authority owns the resource and native Field must never close it.
    Borrowed,
}

/// Stable diagnostic identity for one tracked resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceDescriptor {
    kind: ResourceKind,
    label: String,
}

impl ResourceDescriptor {
    /// Names one resource without granting ownership over it.
    pub fn new(kind: ResourceKind, label: impl Into<String>) -> Self {
        Self {
            kind,
            label: label.into(),
        }
    }

    /// Returns the resource role.
    pub fn kind(&self) -> ResourceKind {
        self.kind
    }

    /// Returns the human-readable resource label.
    pub fn label(&self) -> &str {
        &self.label
    }
}

/// A renderer-neutral application-owned resource cleanup boundary.
///
/// Implementations must not assume cleanup runs on the UI thread. They must
/// observe the shared deadline, bound every blocking operation by its remaining
/// time, and return [`ResourceCloseError::deadline_exceeded`] when cleanup
/// cannot finish in time. The registry deliberately does not spawn detached
/// cleanup threads: Rust cannot safely preempt an arbitrary stuck closer, so a
/// non-compliant implementation can still delay shutdown.
pub trait ApplicationResource: Send {
    /// Releases the resource once within the application-wide shutdown deadline.
    fn close(&mut self, deadline: ShutdownDeadline) -> Result<(), ResourceCloseError>;
}

/// One shared monotonic deadline for an entire application shutdown pass.
///
/// Every resource receives the same value. Implementations should query
/// [`Self::remaining`] immediately before each blocking operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShutdownDeadline {
    started_at: Instant,
    timeout: Duration,
}

impl ShutdownDeadline {
    fn from_timeout(timeout: Duration) -> Self {
        Self {
            started_at: Instant::now(),
            timeout,
        }
    }

    /// Returns the overall budget supplied for this shutdown pass.
    pub fn timeout(self) -> Duration {
        self.timeout
    }

    /// Returns the time still available to all remaining cleanup work.
    pub fn remaining(self) -> Duration {
        self.timeout.saturating_sub(self.started_at.elapsed())
    }

    /// Returns whether the overall shutdown budget has been consumed.
    pub fn is_expired(self) -> bool {
        self.started_at.elapsed() >= self.timeout
    }

    fn recovery_deadline(self) -> RecoveryDeadline {
        RecoveryDeadline::from_started_at(self.started_at, self.timeout)
    }
}

/// Machine-readable category for a resource cleanup error.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceCloseErrorKind {
    /// The resource reported an ordinary cleanup failure.
    ResourceFailure,
    /// Cleanup could not finish within the shared shutdown deadline.
    DeadlineExceeded,
    /// The resource's close implementation panicked.
    ClosePanicked,
    /// The owned resource's destructor panicked after close was attempted.
    DestructorPanicked,
    /// Cleanup succeeded, but its durable recovery entry could not be retired.
    RecoveryJournalRetirementFailed,
}

/// One non-panicking resource cleanup error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceCloseError {
    kind: ResourceCloseErrorKind,
    message: String,
    recovery_journal_failure: Option<RecoveryJournalFailure>,
}

impl ResourceCloseError {
    /// Creates a cleanup error suitable for aggregate shutdown reporting.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: ResourceCloseErrorKind::ResourceFailure,
            message: message.into(),
            recovery_journal_failure: None,
        }
    }

    /// Creates a typed diagnostic for cleanup that exhausted its shared budget.
    pub fn deadline_exceeded(message: impl Into<String>) -> Self {
        Self {
            kind: ResourceCloseErrorKind::DeadlineExceeded,
            message: message.into(),
            recovery_journal_failure: None,
        }
    }

    fn close_panicked() -> Self {
        Self {
            kind: ResourceCloseErrorKind::ClosePanicked,
            message: "resource cleanup panicked".to_owned(),
            recovery_journal_failure: None,
        }
    }

    fn destructor_panicked() -> Self {
        Self {
            kind: ResourceCloseErrorKind::DestructorPanicked,
            message: "resource destructor panicked after cleanup".to_owned(),
            recovery_journal_failure: None,
        }
    }

    fn recovery_retirement_failed(failure: RecoveryJournalFailure) -> Self {
        Self {
            kind: ResourceCloseErrorKind::RecoveryJournalRetirementFailed,
            message: failure.to_string(),
            recovery_journal_failure: Some(failure),
        }
    }

    /// Returns the machine-readable cleanup failure category.
    pub fn kind(&self) -> ResourceCloseErrorKind {
        self.kind
    }

    /// Returns the original cleanup diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns durable journal evidence when retirement persistence failed.
    pub fn recovery_journal_failure(&self) -> Option<&RecoveryJournalFailure> {
        self.recovery_journal_failure.as_ref()
    }
}

impl fmt::Display for ResourceCloseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ResourceCloseError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.recovery_journal_failure
            .as_ref()
            .map(|failure| failure as &(dyn std::error::Error + 'static))
    }
}

/// The durable journal mutation that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryJournalOperation {
    /// Persisting an acquired owned resource before management.
    Registration,
    /// Retiring an entry after complete ordinary cleanup.
    Retirement,
}

impl fmt::Display for RecoveryJournalOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Registration => formatter.write_str("registration"),
            Self::Retirement => formatter.write_str("retirement"),
        }
    }
}

/// Machine-readable reason a durable journal mutation did not complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryJournalFailureKind {
    /// Durable storage returned an I/O error.
    PersistenceIo,
    /// The shared lifecycle deadline expired during persistence.
    DeadlineExceeded,
    /// Unsafe or unsupported existing content was preserved unchanged.
    ExistingJournalPreserved,
    /// Registration would exceed the bounded entry count.
    CapacityExceeded,
    /// The resulting bounded wire document would be too large.
    EncodedJournalOversized,
    /// The store returned an outcome invalid for the requested mutation.
    UnexpectedOutcome,
}

/// One typed failure to register or retire durable recovery state.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryJournalFailure {
    operation: RecoveryJournalOperation,
    kind: RecoveryJournalFailureKind,
    message: String,
}

impl RecoveryJournalFailure {
    /// Returns the mutation that failed.
    pub fn operation(&self) -> RecoveryJournalOperation {
        self.operation
    }

    /// Returns the machine-readable failure category.
    pub fn kind(&self) -> RecoveryJournalFailureKind {
        self.kind
    }

    /// Returns the actionable persistence diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RecoveryJournalFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "recovery journal {} failed: {}",
            self.operation, self.message
        )
    }
}

impl std::error::Error for RecoveryJournalFailure {}

/// One owned-resource cleanup failure observed during shutdown.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceCloseFailure {
    descriptor: ResourceDescriptor,
    error: ResourceCloseError,
}

impl ResourceCloseFailure {
    /// Returns the exact resource that failed.
    pub fn descriptor(&self) -> &ResourceDescriptor {
        &self.descriptor
    }

    /// Returns the cleanup diagnostic for this resource.
    pub fn error(&self) -> &ResourceCloseError {
        &self.error
    }
}

/// Every cleanup failure observed during one deterministic shutdown pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ShutdownFailures {
    failures: Vec<ResourceCloseFailure>,
}

impl ShutdownFailures {
    /// Returns failures in cleanup-attempt order.
    pub fn failures(&self) -> &[ResourceCloseFailure] {
        &self.failures
    }
}

impl fmt::Display for ShutdownFailures {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "{} application-owned resource cleanup failure(s) observed",
            self.failures.len()
        )
    }
}

impl std::error::Error for ShutdownFailures {}

trait RecoveryJournalBackend: Send {
    fn register(
        &self,
        entry: DurableRecoveryEntry,
        deadline: RecoveryDeadline,
    ) -> io::Result<RecoveryJournalUpdateOutcome>;

    fn retire(
        &self,
        entry_id: &str,
        deadline: RecoveryDeadline,
    ) -> io::Result<RecoveryJournalUpdateOutcome>;
}

impl RecoveryJournalBackend for RecoveryJournalStore {
    fn register(
        &self,
        entry: DurableRecoveryEntry,
        deadline: RecoveryDeadline,
    ) -> io::Result<RecoveryJournalUpdateOutcome> {
        RecoveryJournalStore::register_with_deadline(self, entry, deadline)
    }

    fn retire(
        &self,
        entry_id: &str,
        deadline: RecoveryDeadline,
    ) -> io::Result<RecoveryJournalUpdateOutcome> {
        RecoveryJournalStore::retire_with_deadline(self, entry_id, deadline)
    }
}

struct TrackedRecoveryJournal {
    backend: Box<dyn RecoveryJournalBackend>,
    entry: DurableRecoveryEntry,
}

struct TrackedResource {
    descriptor: ResourceDescriptor,
    ownership: ResourceOwnership,
    owned: Option<Box<dyn ApplicationResource>>,
    recovery: Option<TrackedRecoveryJournal>,
}

/// An inactive owned resource with an exact durable cleanup recipe.
///
/// Product code must stage recoverable resources through this boundary. The
/// registry persists [`Self::recovery_entry`] before calling [`Self::activate`],
/// so a process exit can never leave an active resource without durable
/// recovery authority.
pub trait StagedRecoverableResource {
    /// The active resource managed after durable registration succeeds.
    type Resource: ApplicationResource + 'static;
    /// An actionable activation failure.
    type ActivationError: fmt::Display;

    /// Returns the exact cleanup recipe for the resource that activation creates.
    fn recovery_entry(&self) -> &DurableRecoveryEntry;

    /// Creates the live resource within the shared startup budget.
    ///
    /// An error may mean activation created only part of the external artifact.
    /// The registry therefore retains the durable entry for next-start recovery
    /// on every error and never assumes the implementation rolled back fully.
    fn activate(self, deadline: ShutdownDeadline) -> Result<Self::Resource, Self::ActivationError>;
}

/// The sole application-level cleanup authority for native Field resources.
///
/// Window close/reopen and presentation switches do not touch this registry.
/// Dropping an active registry performs best-effort cleanup of partially
/// acquired startup resources without allowing one failure to stop the rest.
/// Product code cannot register an already-active owned resource without first
/// establishing durable recovery authority:
///
/// ```compile_fail
/// use nopal_native_lifecycle::resources::{
///     ApplicationResource, ApplicationResources, ResourceCloseError,
///     ResourceDescriptor, ResourceKind, ShutdownDeadline,
/// };
///
/// struct ActiveSocket;
/// impl ApplicationResource for ActiveSocket {
///     fn close(&mut self, _deadline: ShutdownDeadline) -> Result<(), ResourceCloseError> {
///         Ok(())
///     }
/// }
///
/// let mut resources = ApplicationResources::new();
/// resources.register_owned(
///     ResourceDescriptor::new(ResourceKind::Socket, "unsafe live socket"),
///     ActiveSocket,
/// );
/// ```
pub struct ApplicationResources {
    resources: Vec<TrackedResource>,
    shutdown_result: Option<Result<(), ShutdownFailures>>,
}

impl ApplicationResources {
    /// Creates an empty active application registry.
    pub fn new() -> Self {
        Self {
            resources: Vec::new(),
            shutdown_result: None,
        }
    }

    /// Registers a test-only application-owned resource in acquisition order.
    ///
    /// Registration after shutdown is rejected and the supplied resource is
    /// still closed exactly once so a late acquisition cannot leak.
    #[cfg(test)]
    fn register_owned<R>(
        &mut self,
        descriptor: ResourceDescriptor,
        resource: R,
    ) -> Result<(), ResourceRegistrationError>
    where
        R: ApplicationResource + 'static,
    {
        if self.shutdown_result.is_some() {
            let cleanup_errors = cleanup_owned_resource(
                Box::new(resource),
                ShutdownDeadline::from_timeout(DEFAULT_SHUTDOWN_TIMEOUT),
            );
            return Err(ResourceRegistrationError {
                kind: ResourceRegistrationErrorKind::RegistryShutdown,
                cleanup_errors,
                recovery_journal_failure: None,
                activation_failure: None,
            });
        }

        self.resources.push(TrackedResource {
            descriptor,
            ownership: ResourceOwnership::ApplicationOwned,
            owned: Some(Box::new(resource)),
            recovery: None,
        });
        Ok(())
    }

    /// Durably registers, activates, and manages one recoverable owned resource.
    ///
    /// The staged resource remains inactive until its exact recovery entry is
    /// durable. Ordinary shutdown retires that entry only after both close and
    /// destruction finish successfully.
    pub fn register_recoverable_owned<S>(
        &mut self,
        descriptor: ResourceDescriptor,
        staged: S,
        journal: RecoveryJournalStore,
    ) -> Result<(), ResourceRegistrationError>
    where
        S: StagedRecoverableResource,
    {
        self.register_recoverable_owned_with_journal(descriptor, staged, Box::new(journal))
    }

    fn register_recoverable_owned_with_journal<S>(
        &mut self,
        descriptor: ResourceDescriptor,
        staged: S,
        journal: Box<dyn RecoveryJournalBackend>,
    ) -> Result<(), ResourceRegistrationError>
    where
        S: StagedRecoverableResource,
    {
        let deadline = ShutdownDeadline::from_timeout(DEFAULT_SHUTDOWN_TIMEOUT);
        let entry = staged.recovery_entry().clone();
        if self
            .resources
            .iter()
            .filter_map(|resource| resource.recovery.as_ref())
            .any(|recovery| recovery.entry.id() == entry.id())
        {
            return Err(ResourceRegistrationError {
                kind: ResourceRegistrationErrorKind::DuplicateRecoveryId,
                cleanup_errors: Vec::new(),
                recovery_journal_failure: None,
                activation_failure: None,
            });
        }
        if self.shutdown_result.is_some() {
            return Err(ResourceRegistrationError {
                kind: ResourceRegistrationErrorKind::RegistryShutdown,
                cleanup_errors: Vec::new(),
                recovery_journal_failure: None,
                activation_failure: None,
            });
        }

        if let Err(failure) = register_recovery_entry(
            journal.as_ref(),
            entry.clone(),
            deadline.recovery_deadline(),
        ) {
            return Err(ResourceRegistrationError {
                kind: ResourceRegistrationErrorKind::RecoveryJournalRegistrationFailed,
                cleanup_errors: Vec::new(),
                recovery_journal_failure: Some(failure),
                activation_failure: None,
            });
        }

        let owned: Box<dyn ApplicationResource> = match staged.activate(deadline) {
            Ok(resource) => Box::new(resource),
            Err(error) => {
                return Err(ResourceRegistrationError {
                    kind: ResourceRegistrationErrorKind::ActivationFailed,
                    cleanup_errors: Vec::new(),
                    recovery_journal_failure: None,
                    activation_failure: Some(error.to_string()),
                });
            }
        };

        self.resources.push(TrackedResource {
            descriptor,
            ownership: ResourceOwnership::ApplicationOwned,
            owned: Some(owned),
            recovery: Some(TrackedRecoveryJournal {
                backend: journal,
                entry,
            }),
        });
        Ok(())
    }

    /// Records a borrowed resource without accepting a cleanup capability.
    ///
    /// Borrowed registrations are diagnostic only. They are ignored after
    /// shutdown and can never be closed through this API.
    pub fn register_borrowed(&mut self, descriptor: ResourceDescriptor) {
        if self.shutdown_result.is_some() {
            return;
        }
        self.resources.push(TrackedResource {
            descriptor,
            ownership: ResourceOwnership::Borrowed,
            owned: None,
            recovery: None,
        });
    }

    /// Returns tracked resource descriptors and their explicit ownership.
    pub fn tracked(
        &self,
    ) -> impl ExactSizeIterator<Item = (&ResourceDescriptor, ResourceOwnership)> {
        self.resources
            .iter()
            .map(|resource| (&resource.descriptor, resource.ownership))
    }

    /// Returns whether the one shutdown pass has already run.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown_result.is_some()
    }

    /// Closes every application-owned resource once in reverse acquisition order.
    ///
    /// Borrowed resources are discarded without cleanup. Every owned entry is
    /// removed before its close attempt, so failures and repeated calls cannot
    /// cause a second close.
    pub fn shutdown(&mut self) -> Result<(), ShutdownFailures> {
        self.shutdown_with_timeout(DEFAULT_SHUTDOWN_TIMEOUT)
    }

    /// Closes owned resources within one shared best-effort shutdown budget.
    ///
    /// Every owned resource is still attempted once in reverse acquisition
    /// order after the budget expires. A compliant resource observes the expired
    /// deadline and returns promptly with a typed deadline error. Arbitrary
    /// resource code cannot be killed safely, so this method cannot enforce the
    /// bound against an implementation that ignores its deadline contract.
    pub fn shutdown_with_timeout(&mut self, timeout: Duration) -> Result<(), ShutdownFailures> {
        if let Some(result) = &self.shutdown_result {
            return result.clone();
        }

        let deadline = ShutdownDeadline::from_timeout(timeout);
        let mut failures = Vec::new();
        while let Some(mut tracked) = self.resources.pop() {
            if tracked.ownership == ResourceOwnership::Borrowed {
                continue;
            }
            let Some(mut owned) = tracked.owned.take() else {
                continue;
            };
            let mut cleanup_succeeded = true;
            if let Err(error) = close_without_unwind(owned.as_mut(), deadline) {
                cleanup_succeeded = false;
                failures.push(ResourceCloseFailure {
                    descriptor: tracked.descriptor.clone(),
                    error,
                });
            }
            if let Err(error) = drop_without_unwind(owned) {
                cleanup_succeeded = false;
                failures.push(ResourceCloseFailure {
                    descriptor: tracked.descriptor.clone(),
                    error,
                });
            }
            if cleanup_succeeded
                && let Some(recovery) = tracked.recovery
                && let Err(error) = retire_recovery_entry(
                    recovery.backend.as_ref(),
                    &recovery.entry,
                    deadline.recovery_deadline(),
                )
            {
                failures.push(ResourceCloseFailure {
                    descriptor: tracked.descriptor,
                    error: ResourceCloseError::recovery_retirement_failed(error),
                });
            }
        }

        let result = if failures.is_empty() {
            Ok(())
        } else {
            Err(ShutdownFailures { failures })
        };
        self.shutdown_result = Some(result.clone());
        result
    }
}

impl Default for ApplicationResources {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for ApplicationResources {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

fn close_without_unwind(
    resource: &mut dyn ApplicationResource,
    deadline: ShutdownDeadline,
) -> Result<(), ResourceCloseError> {
    match catch_unwind(AssertUnwindSafe(|| resource.close(deadline))) {
        Ok(Ok(())) if deadline.is_expired() => Err(ResourceCloseError::deadline_exceeded(
            "resource cleanup returned after the shared shutdown deadline",
        )),
        Ok(result) => result,
        Err(_) => Err(ResourceCloseError::close_panicked()),
    }
}

fn drop_without_unwind(resource: Box<dyn ApplicationResource>) -> Result<(), ResourceCloseError> {
    match catch_unwind(AssertUnwindSafe(|| drop(resource))) {
        Ok(()) => Ok(()),
        Err(_) => Err(ResourceCloseError::destructor_panicked()),
    }
}

#[cfg(test)]
fn cleanup_owned_resource(
    mut resource: Box<dyn ApplicationResource>,
    deadline: ShutdownDeadline,
) -> Vec<ResourceCloseError> {
    let mut errors = Vec::new();
    if let Err(error) = close_without_unwind(resource.as_mut(), deadline) {
        errors.push(error);
    }
    if let Err(error) = drop_without_unwind(resource) {
        errors.push(error);
    }
    errors
}

fn register_recovery_entry(
    journal: &dyn RecoveryJournalBackend,
    entry: DurableRecoveryEntry,
    deadline: RecoveryDeadline,
) -> Result<(), RecoveryJournalFailure> {
    map_recovery_update(
        RecoveryJournalOperation::Registration,
        journal.register(entry, deadline),
        deadline,
    )
}

fn retire_recovery_entry(
    journal: &dyn RecoveryJournalBackend,
    entry: &DurableRecoveryEntry,
    deadline: RecoveryDeadline,
) -> Result<(), RecoveryJournalFailure> {
    let retirement = map_recovery_update(
        RecoveryJournalOperation::Retirement,
        journal.retire(entry.id(), deadline),
        deadline,
    );
    let Err(mut retirement_failure) = retirement else {
        return Ok(());
    };

    if let Err(retention_failure) = register_recovery_entry(journal, entry.clone(), deadline) {
        retirement_failure.message.push_str(
            "; the stale entry also could not be reasserted for startup reconciliation: ",
        );
        retirement_failure
            .message
            .push_str(retention_failure.message());
    }
    Err(retirement_failure)
}

fn map_recovery_update(
    operation: RecoveryJournalOperation,
    result: io::Result<RecoveryJournalUpdateOutcome>,
    deadline: RecoveryDeadline,
) -> Result<(), RecoveryJournalFailure> {
    if deadline.is_expired() {
        return Err(RecoveryJournalFailure {
            operation,
            kind: RecoveryJournalFailureKind::DeadlineExceeded,
            message:
                "shared resource lifecycle deadline expired during recovery journal persistence"
                    .to_owned(),
        });
    }
    match result {
        Ok(RecoveryJournalUpdateOutcome::Written { .. }) => Ok(()),
        Ok(RecoveryJournalUpdateOutcome::EntryMissing)
            if operation == RecoveryJournalOperation::Retirement =>
        {
            Ok(())
        }
        Ok(RecoveryJournalUpdateOutcome::EntryMissing) => Err(RecoveryJournalFailure {
            operation,
            kind: RecoveryJournalFailureKind::UnexpectedOutcome,
            message: "registration reported a missing entry instead of durable persistence"
                .to_owned(),
        }),
        Ok(RecoveryJournalUpdateOutcome::Preserved(preserved)) => Err(RecoveryJournalFailure {
            operation,
            kind: RecoveryJournalFailureKind::ExistingJournalPreserved,
            message: preserved.diagnostic().to_owned(),
        }),
        Ok(RecoveryJournalUpdateOutcome::CapacityExceeded { max_entries }) => {
            Err(RecoveryJournalFailure {
                operation,
                kind: RecoveryJournalFailureKind::CapacityExceeded,
                message: format!("journal capacity of {max_entries} entries would be exceeded"),
            })
        }
        Ok(RecoveryJournalUpdateOutcome::EncodedJournalOversized {
            max_bytes,
            encoded_bytes,
        }) => Err(RecoveryJournalFailure {
            operation,
            kind: RecoveryJournalFailureKind::EncodedJournalOversized,
            message: format!(
                "encoded journal would be {encoded_bytes} bytes, exceeding the {max_bytes}-byte limit"
            ),
        }),
        Err(error) => Err(RecoveryJournalFailure {
            operation,
            kind: RecoveryJournalFailureKind::PersistenceIo,
            message: error.to_string(),
        }),
    }
}

/// Why an owned-resource registration was rejected.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ResourceRegistrationErrorKind {
    /// The registry had already completed its one shutdown pass.
    RegistryShutdown,
    /// The durable entry could not be persisted before management.
    RecoveryJournalRegistrationFailed,
    /// Another live resource already uses the same durable recovery identity.
    DuplicateRecoveryId,
    /// Durable registration succeeded, but staged activation failed.
    ActivationFailed,
}

/// An owned-resource registration rejected before management began.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ResourceRegistrationError {
    kind: ResourceRegistrationErrorKind,
    cleanup_errors: Vec<ResourceCloseError>,
    recovery_journal_failure: Option<RecoveryJournalFailure>,
    activation_failure: Option<String>,
}

impl ResourceRegistrationError {
    /// Returns why registration was rejected.
    pub fn kind(&self) -> ResourceRegistrationErrorKind {
        self.kind
    }

    /// Returns a cleanup failure if immediate release of the late resource failed.
    pub fn cleanup_error(&self) -> Option<&ResourceCloseError> {
        self.cleanup_errors.first()
    }

    /// Returns every failure observed while immediately releasing the resource.
    pub fn cleanup_errors(&self) -> &[ResourceCloseError] {
        &self.cleanup_errors
    }

    /// Returns durable journal evidence when registration or shutdown retirement failed.
    pub fn recovery_journal_failure(&self) -> Option<&RecoveryJournalFailure> {
        self.recovery_journal_failure.as_ref()
    }

    /// Returns the staged activation diagnostic, when activation failed.
    pub fn activation_failure(&self) -> Option<&str> {
        self.activation_failure.as_deref()
    }
}

impl fmt::Display for ResourceRegistrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.kind {
            ResourceRegistrationErrorKind::RegistryShutdown => {
                formatter.write_str("application resource registry is shut down")?;
            }
            ResourceRegistrationErrorKind::RecoveryJournalRegistrationFailed => {
                formatter.write_str("durable recovery registration failed")?;
            }
            ResourceRegistrationErrorKind::DuplicateRecoveryId => {
                formatter.write_str("durable recovery identity is already live")?;
            }
            ResourceRegistrationErrorKind::ActivationFailed => {
                formatter.write_str("staged recoverable resource activation failed")?;
            }
        }
        if let Some(failure) = &self.activation_failure {
            write!(formatter, "; {failure}")?;
        }
        if let Some(failure) = &self.recovery_journal_failure {
            write!(formatter, "; {failure}")?;
        }
        if let Some(error) = self.cleanup_errors.first() {
            write!(formatter, "; immediate resource cleanup failed: {error}")?;
        }
        Ok(())
    }
}

impl std::error::Error for ResourceRegistrationError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.recovery_journal_failure
            .as_ref()
            .map(|failure| failure as &(dyn std::error::Error + 'static))
            .or_else(|| {
                self.cleanup_errors
                    .first()
                    .map(|error| error as &(dyn std::error::Error + 'static))
            })
    }
}

/// One field of an exact Session identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionIdentityField {
    /// Core Plot identity.
    PlotId,
    /// Core Session identity.
    SessionId,
}

/// An invalid exact Session identity.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidSessionIdentity {
    field: SessionIdentityField,
}

impl InvalidSessionIdentity {
    /// Creates a typed invalid-identity diagnostic.
    pub fn new(field: SessionIdentityField) -> Self {
        Self { field }
    }

    /// Returns the blank field.
    pub fn field(&self) -> SessionIdentityField {
        self.field
    }
}

impl fmt::Display for InvalidSessionIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let field = match self.field {
            SessionIdentityField::PlotId => "plot_id",
            SessionIdentityField::SessionId => "session_id",
        };
        write!(
            formatter,
            "exact Session identity requires a non-blank {field}"
        )
    }
}

impl std::error::Error for InvalidSessionIdentity {}

/// One non-blank exact Core Plot and Session pair.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ExactSessionIdentity {
    plot_id: String,
    session_id: String,
}

impl ExactSessionIdentity {
    /// Validates and creates one exact Plot and Session identity.
    pub fn new(
        plot_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<Self, InvalidSessionIdentity> {
        let plot_id = plot_id.into();
        let session_id = session_id.into();
        if plot_id.trim().is_empty() {
            return Err(InvalidSessionIdentity::new(SessionIdentityField::PlotId));
        }
        if session_id.trim().is_empty() {
            return Err(InvalidSessionIdentity::new(SessionIdentityField::SessionId));
        }
        Ok(Self {
            plot_id,
            session_id,
        })
    }

    /// Returns the exact Core Plot identity.
    pub fn plot_id(&self) -> &str {
        &self.plot_id
    }

    /// Returns the exact Core Session identity.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

impl TryFrom<&crate::reconcile::ExactSessionSelection> for ExactSessionIdentity {
    type Error = InvalidSessionIdentity;

    fn try_from(selection: &crate::reconcile::ExactSessionSelection) -> Result<Self, Self::Error> {
        Self::new(selection.plot_id(), selection.session_id())
    }
}

/// The presentation attached to an already-existing exact Session.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentationMode {
    /// Rich structured activity output.
    StructuredOutput,
    /// Same-session terminal fallback.
    Terminal,
}

/// Whether the presentation window is currently visible.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowState {
    /// The application window is open.
    Open,
    /// The application remains resident with its window closed.
    Closed,
}

/// An honest reason rich structured output is currently unavailable.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuredOutputDegradation {
    diagnostic: String,
}

impl StructuredOutputDegradation {
    /// Returns the actionable diagnostic shown with Terminal fallback.
    pub fn diagnostic(&self) -> &str {
        &self.diagnostic
    }
}

/// A structured-output degradation without a visible diagnostic.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidDegradationDiagnostic;

impl fmt::Display for InvalidDegradationDiagnostic {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("structured output degradation requires a non-blank diagnostic")
    }
}

impl std::error::Error for InvalidDegradationDiagnostic {}

/// Window and presentation state for one immutable exact Session identity.
///
/// This type deliberately contains no resource factory or cleanup capability.
/// Switching presentation and closing/reopening a window therefore cannot
/// create or close Session resources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionPresentation {
    identity: ExactSessionIdentity,
    requested_mode: PresentationMode,
    window_state: WindowState,
    structured_output_degradation: Option<StructuredOutputDegradation>,
}

impl SessionPresentation {
    /// Starts with structured output in an open window.
    pub fn new(identity: ExactSessionIdentity) -> Self {
        Self {
            identity,
            requested_mode: PresentationMode::StructuredOutput,
            window_state: WindowState::Open,
            structured_output_degradation: None,
        }
    }

    /// Returns the immutable exact Session identity.
    pub fn identity(&self) -> &ExactSessionIdentity {
        &self.identity
    }

    /// Returns the effective same-session presentation.
    ///
    /// This compatibility accessor is equivalent to [`Self::effective_mode`].
    pub fn mode(&self) -> PresentationMode {
        self.effective_mode()
    }

    /// Returns the presentation currently shown after applying degradation.
    pub fn effective_mode(&self) -> PresentationMode {
        if self.structured_output_degradation.is_some() {
            PresentationMode::Terminal
        } else {
            self.requested_mode
        }
    }

    /// Returns the user's requested presentation independent of degradation.
    pub fn requested_mode(&self) -> PresentationMode {
        self.requested_mode
    }

    /// Returns the current window state.
    pub fn window_state(&self) -> WindowState {
        self.window_state
    }

    /// Switches the presentation without changing or creating a Session.
    pub fn switch_to(&mut self, mode: PresentationMode) {
        self.requested_mode = mode;
    }

    /// Degrades to same-session Terminal fallback with a visible diagnostic.
    pub fn structured_unavailable(
        &mut self,
        diagnostic: impl Into<String>,
    ) -> Result<(), InvalidDegradationDiagnostic> {
        let diagnostic = diagnostic.into();
        if diagnostic.trim().is_empty() {
            return Err(InvalidDegradationDiagnostic);
        }
        self.structured_output_degradation = Some(StructuredOutputDegradation { diagnostic });
        Ok(())
    }

    /// Returns the active structured-output failure shown beside Terminal fallback.
    pub fn structured_output_degradation(&self) -> Option<&StructuredOutputDegradation> {
        self.structured_output_degradation.as_ref()
    }

    /// Restores structured output for the exact same Session and window state.
    pub fn structured_restored(&mut self) {
        self.structured_output_degradation = None;
    }

    /// Closes only the presentation window, leaving application resources resident.
    pub fn close_window(&mut self) {
        self.window_state = WindowState::Closed;
    }

    /// Reopens only the presentation window for the exact same Session.
    pub fn reopen_window(&mut self) {
        self.window_state = WindowState::Open;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use crate::recovery::{
        DurableIdentity, DurableRecoveryEntry, DurableRecoveryRecipe, ExactRecoveryAdapter,
        FilesystemRecoveryRecipe, RecoveryAdapterError, RecoveryDisposition,
        RecoveryJournalReadOutcome, RecoveryJournalUpdateOutcome, RecoveryReconcileOutcome,
        VerifiedProcessRecoveryRecipe,
    };
    use tempfile::tempdir;

    #[derive(Clone)]
    enum CloseBehavior {
        Succeed,
        Fail(&'static str),
        Panic,
    }

    struct RecordingResource {
        name: &'static str,
        behavior: CloseBehavior,
        closes: Arc<Mutex<Vec<&'static str>>>,
    }

    impl RecordingResource {
        fn new(
            name: &'static str,
            behavior: CloseBehavior,
            closes: Arc<Mutex<Vec<&'static str>>>,
        ) -> Self {
            Self {
                name,
                behavior,
                closes,
            }
        }
    }

    impl ApplicationResource for RecordingResource {
        fn close(&mut self, _deadline: ShutdownDeadline) -> Result<(), ResourceCloseError> {
            self.closes.lock().unwrap().push(self.name);
            match self.behavior {
                CloseBehavior::Succeed => Ok(()),
                CloseBehavior::Fail(message) => Err(ResourceCloseError::new(message)),
                CloseBehavior::Panic => panic!("closer panicked"),
            }
        }
    }

    struct PanickingDropResource {
        inner: RecordingResource,
    }

    impl ApplicationResource for PanickingDropResource {
        fn close(&mut self, deadline: ShutdownDeadline) -> Result<(), ResourceCloseError> {
            self.inner.close(deadline)
        }
    }

    impl Drop for PanickingDropResource {
        fn drop(&mut self) {
            panic!("resource destructor panicked");
        }
    }

    struct BudgetAwareResource {
        name: &'static str,
        work: Duration,
        closes: Arc<Mutex<Vec<&'static str>>>,
        deadlines: Arc<Mutex<Vec<ShutdownDeadline>>>,
    }

    impl ApplicationResource for BudgetAwareResource {
        fn close(&mut self, deadline: ShutdownDeadline) -> Result<(), ResourceCloseError> {
            self.closes.lock().unwrap().push(self.name);
            self.deadlines.lock().unwrap().push(deadline);
            let remaining = deadline.remaining();
            if self.work > remaining {
                std::thread::sleep(remaining);
                return Err(ResourceCloseError::deadline_exceeded(
                    "resource could not close within the shared shutdown budget",
                ));
            }
            std::thread::sleep(self.work);
            Ok(())
        }
    }

    #[derive(Default)]
    struct FakeRecoveryJournalState {
        entries: Vec<DurableRecoveryEntry>,
        events: Vec<String>,
        fail_registration: bool,
        fail_retirement: bool,
        deadlines: Vec<RecoveryDeadline>,
    }

    struct FakeRecoveryJournal {
        state: Arc<Mutex<FakeRecoveryJournalState>>,
    }

    impl FakeRecoveryJournal {
        fn new(state: &Arc<Mutex<FakeRecoveryJournalState>>) -> Self {
            Self {
                state: Arc::clone(state),
            }
        }
    }

    impl RecoveryJournalBackend for FakeRecoveryJournal {
        fn register(
            &self,
            entry: DurableRecoveryEntry,
            _deadline: RecoveryDeadline,
        ) -> io::Result<RecoveryJournalUpdateOutcome> {
            let mut state = self.state.lock().unwrap();
            state.deadlines.push(_deadline);
            state.events.push(format!("register:{}", entry.id()));
            if state.fail_registration {
                return Err(io::Error::other(
                    "simulated registration persistence failure",
                ));
            }
            if let Some(existing) = state
                .entries
                .iter_mut()
                .find(|existing| existing.id() == entry.id())
            {
                *existing = entry;
            } else {
                state.entries.push(entry);
            }
            Ok(RecoveryJournalUpdateOutcome::Written {
                entry_count: state.entries.len(),
            })
        }

        fn retire(
            &self,
            entry_id: &str,
            _deadline: RecoveryDeadline,
        ) -> io::Result<RecoveryJournalUpdateOutcome> {
            let mut state = self.state.lock().unwrap();
            state.deadlines.push(_deadline);
            state.events.push(format!("retire:{entry_id}"));
            if state.fail_retirement {
                state.entries.retain(|entry| entry.id() != entry_id);
                return Err(io::Error::other("simulated retirement persistence failure"));
            }
            let original_len = state.entries.len();
            state.entries.retain(|entry| entry.id() != entry_id);
            if state.entries.len() == original_len {
                Ok(RecoveryJournalUpdateOutcome::EntryMissing)
            } else {
                Ok(RecoveryJournalUpdateOutcome::Written {
                    entry_count: state.entries.len(),
                })
            }
        }
    }

    fn durable_entry(id: &str, path: &Path) -> DurableRecoveryEntry {
        DurableRecoveryEntry::new(
            id,
            format!("recoverable {id}"),
            ResourceOwnership::ApplicationOwned,
            DurableRecoveryRecipe::Filesystem(
                FilesystemRecoveryRecipe::new(
                    path,
                    DurableIdentity::new("test.identity", format!("identity-{id}")).unwrap(),
                )
                .unwrap(),
            ),
        )
        .unwrap()
    }

    struct TestStage<R> {
        entry: DurableRecoveryEntry,
        resource: Result<R, &'static str>,
    }

    impl<R> StagedRecoverableResource for TestStage<R>
    where
        R: ApplicationResource + 'static,
    {
        type Resource = R;
        type ActivationError = &'static str;

        fn recovery_entry(&self) -> &DurableRecoveryEntry {
            &self.entry
        }

        fn activate(
            self,
            _deadline: ShutdownDeadline,
        ) -> Result<Self::Resource, Self::ActivationError> {
            self.resource
        }
    }

    fn staged<R>(entry: DurableRecoveryEntry, resource: R) -> TestStage<R>
    where
        R: ApplicationResource + 'static,
    {
        TestStage {
            entry,
            resource: Ok(resource),
        }
    }

    struct SequencedStage<R> {
        entry: DurableRecoveryEntry,
        resource: Result<R, &'static str>,
        state: Arc<Mutex<FakeRecoveryJournalState>>,
    }

    impl<R> StagedRecoverableResource for SequencedStage<R>
    where
        R: ApplicationResource + 'static,
    {
        type Resource = R;
        type ActivationError = &'static str;

        fn recovery_entry(&self) -> &DurableRecoveryEntry {
            &self.entry
        }

        fn activate(
            self,
            _deadline: ShutdownDeadline,
        ) -> Result<Self::Resource, Self::ActivationError> {
            self.state
                .lock()
                .unwrap()
                .events
                .push(format!("activate:{}", self.entry.id()));
            self.resource
        }
    }

    struct FailAfterCreateStage {
        entry: DurableRecoveryEntry,
        path: std::path::PathBuf,
    }

    impl StagedRecoverableResource for FailAfterCreateStage {
        type Resource = RecordingResource;
        type ActivationError = &'static str;

        fn recovery_entry(&self) -> &DurableRecoveryEntry {
            &self.entry
        }

        fn activate(
            self,
            _deadline: ShutdownDeadline,
        ) -> Result<Self::Resource, Self::ActivationError> {
            std::fs::write(&self.path, b"partial-live").unwrap();
            Err("failure after create")
        }
    }

    struct ExactTestFileAdapter;

    impl ExactRecoveryAdapter for ExactTestFileAdapter {
        fn recover_filesystem_exact(
            &mut self,
            recipe: &FilesystemRecoveryRecipe,
            deadline: RecoveryDeadline,
        ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
            if deadline.is_expired() {
                return Err(RecoveryAdapterError::deadline_exceeded(
                    "test file recovery deadline expired",
                ));
            }
            let observed = match std::fs::read_to_string(recipe.path()) {
                Ok(observed) => observed,
                Err(error) if error.kind() == io::ErrorKind::NotFound => {
                    return Ok(RecoveryDisposition::AlreadyAbsent);
                }
                Err(error) => return Err(RecoveryAdapterError::new(error.to_string())),
            };
            if observed != recipe.identity().value() {
                return Ok(RecoveryDisposition::IdentityMismatch {
                    observed_identity: Some(observed),
                });
            }
            std::fs::remove_file(recipe.path())
                .map_err(|error| RecoveryAdapterError::new(error.to_string()))?;
            Ok(RecoveryDisposition::Recovered)
        }

        fn recover_process_exact(
            &mut self,
            _recipe: &VerifiedProcessRecoveryRecipe,
            _deadline: RecoveryDeadline,
        ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
            Err(RecoveryAdapterError::new(
                "test adapter does not recover processes",
            ))
        }
    }

    fn descriptor(kind: ResourceKind, label: &'static str) -> ResourceDescriptor {
        ResourceDescriptor::new(kind, label)
    }

    #[test]
    fn shutdown_closes_owned_resources_once_in_reverse_acquisition_order() {
        let closes = Arc::new(Mutex::new(Vec::new()));
        let mut resources = ApplicationResources::new();

        for (kind, name) in [
            (ResourceKind::Session, "session"),
            (ResourceKind::Process, "process"),
            (ResourceKind::Pipe, "pipe"),
            (ResourceKind::Socket, "socket"),
            (ResourceKind::BackgroundResource, "background"),
        ] {
            resources
                .register_owned(
                    descriptor(kind, name),
                    RecordingResource::new(name, CloseBehavior::Succeed, Arc::clone(&closes)),
                )
                .unwrap();
        }

        assert_eq!(resources.shutdown(), Ok(()));
        assert_eq!(resources.shutdown(), Ok(()));
        assert_eq!(
            *closes.lock().unwrap(),
            vec!["background", "socket", "pipe", "process", "session"]
        );
    }

    #[test]
    fn shutdown_never_closes_borrowed_resources() {
        let closes = Arc::new(Mutex::new(Vec::new()));
        let mut resources = ApplicationResources::new();
        resources.register_borrowed(descriptor(ResourceKind::Session, "core-session"));
        resources.register_borrowed(descriptor(ResourceKind::Process, "external-process"));
        resources
            .register_owned(
                descriptor(ResourceKind::Socket, "owned-socket"),
                RecordingResource::new("owned-socket", CloseBehavior::Succeed, Arc::clone(&closes)),
            )
            .unwrap();

        assert_eq!(resources.shutdown(), Ok(()));
        assert_eq!(*closes.lock().unwrap(), vec!["owned-socket"]);
    }

    #[test]
    fn shutdown_continues_after_failures_and_reports_every_failure() {
        let closes = Arc::new(Mutex::new(Vec::new()));
        let mut resources = ApplicationResources::new();
        for (kind, name, behavior) in [
            (
                ResourceKind::Process,
                "process",
                CloseBehavior::Fail("process refused"),
            ),
            (ResourceKind::Pipe, "pipe", CloseBehavior::Succeed),
            (ResourceKind::Socket, "socket", CloseBehavior::Panic),
            (
                ResourceKind::BackgroundResource,
                "background",
                CloseBehavior::Fail("join failed"),
            ),
        ] {
            resources
                .register_owned(
                    descriptor(kind, name),
                    RecordingResource::new(name, behavior, Arc::clone(&closes)),
                )
                .unwrap();
        }

        let failures = resources.shutdown().unwrap_err();

        assert_eq!(
            *closes.lock().unwrap(),
            vec!["background", "socket", "pipe", "process"]
        );
        assert_eq!(failures.failures().len(), 3);
        assert_eq!(failures.failures()[0].descriptor().label(), "background");
        assert_eq!(failures.failures()[0].error().message(), "join failed");
        assert_eq!(failures.failures()[1].descriptor().label(), "socket");
        assert_eq!(
            failures.failures()[1].error().message(),
            "resource cleanup panicked"
        );
        assert_eq!(failures.failures()[2].descriptor().label(), "process");
        assert_eq!(failures.failures()[2].error().message(), "process refused");
        assert_eq!(resources.shutdown(), Err(failures));
        assert_eq!(
            *closes.lock().unwrap(),
            vec!["background", "socket", "pipe", "process"]
        );
    }

    #[test]
    fn successful_close_with_panicking_destructor_is_reported_without_stopping_cleanup() {
        let closes = Arc::new(Mutex::new(Vec::new()));
        let result = std::panic::catch_unwind({
            let closes = Arc::clone(&closes);
            move || {
                let mut resources = ApplicationResources::new();
                resources
                    .register_owned(
                        descriptor(ResourceKind::Session, "remaining"),
                        RecordingResource::new(
                            "remaining",
                            CloseBehavior::Succeed,
                            Arc::clone(&closes),
                        ),
                    )
                    .unwrap();
                resources
                    .register_owned(
                        descriptor(ResourceKind::Socket, "drop-panics"),
                        PanickingDropResource {
                            inner: RecordingResource::new(
                                "drop-panics",
                                CloseBehavior::Succeed,
                                Arc::clone(&closes),
                            ),
                        },
                    )
                    .unwrap();

                resources.shutdown().unwrap_err()
            }
        });

        let failures = result.expect("resource destruction must not unwind shutdown");
        assert_eq!(*closes.lock().unwrap(), vec!["drop-panics", "remaining"]);
        assert_eq!(failures.failures().len(), 1);
        assert_eq!(
            failures.failures()[0].error().kind(),
            ResourceCloseErrorKind::DestructorPanicked
        );
    }

    #[test]
    fn close_and_destructor_panics_are_both_reported_without_escaping() {
        let closes = Arc::new(Mutex::new(Vec::new()));
        let result = std::panic::catch_unwind({
            let closes = Arc::clone(&closes);
            move || {
                let mut resources = ApplicationResources::new();
                resources
                    .register_owned(
                        descriptor(ResourceKind::Pipe, "remaining"),
                        RecordingResource::new(
                            "remaining",
                            CloseBehavior::Succeed,
                            Arc::clone(&closes),
                        ),
                    )
                    .unwrap();
                resources
                    .register_owned(
                        descriptor(ResourceKind::Process, "both-panic"),
                        PanickingDropResource {
                            inner: RecordingResource::new(
                                "both-panic",
                                CloseBehavior::Panic,
                                Arc::clone(&closes),
                            ),
                        },
                    )
                    .unwrap();

                resources.shutdown().unwrap_err()
            }
        });

        let failures = result.expect("close and destructor panics must be contained");
        assert_eq!(*closes.lock().unwrap(), vec!["both-panic", "remaining"]);
        assert_eq!(failures.failures().len(), 2);
        assert_eq!(
            failures.failures()[0].error().kind(),
            ResourceCloseErrorKind::ClosePanicked
        );
        assert_eq!(
            failures.failures()[1].error().kind(),
            ResourceCloseErrorKind::DestructorPanicked
        );
    }

    #[test]
    fn shutdown_propagates_one_shared_budget_and_attempts_remaining_cleanup() {
        let closes = Arc::new(Mutex::new(Vec::new()));
        let deadlines = Arc::new(Mutex::new(Vec::new()));
        let mut resources = ApplicationResources::new();
        for (name, work) in [
            ("remaining", Duration::ZERO),
            ("consumes-budget", Duration::from_secs(1)),
        ] {
            resources
                .register_owned(
                    descriptor(ResourceKind::BackgroundResource, name),
                    BudgetAwareResource {
                        name,
                        work,
                        closes: Arc::clone(&closes),
                        deadlines: Arc::clone(&deadlines),
                    },
                )
                .unwrap();
        }

        let failures = resources
            .shutdown_with_timeout(Duration::from_millis(20))
            .unwrap_err();

        assert_eq!(
            *closes.lock().unwrap(),
            vec!["consumes-budget", "remaining"]
        );
        let observed_deadlines = deadlines.lock().unwrap();
        assert_eq!(observed_deadlines.len(), 2);
        assert_eq!(observed_deadlines[0], observed_deadlines[1]);
        assert_eq!(observed_deadlines[0].timeout(), Duration::from_millis(20));
        assert!(
            failures
                .failures()
                .iter()
                .any(|failure| failure.error().kind() == ResourceCloseErrorKind::DeadlineExceeded)
        );
        assert_eq!(resources.shutdown(), Err(failures));
    }

    #[test]
    fn recoverable_owned_resource_is_registered_before_management_and_retired_after_cleanup() {
        let root = tempdir().unwrap();
        let store = RecoveryJournalStore::new(root.path().join("owned-resources.json"));
        let entry = durable_entry("socket", &root.path().join("native.sock"));
        let closes = Arc::new(Mutex::new(Vec::new()));
        let mut resources = ApplicationResources::new();

        resources
            .register_recoverable_owned(
                descriptor(ResourceKind::Socket, "native socket"),
                staged(
                    entry.clone(),
                    RecordingResource::new(
                        "native socket",
                        CloseBehavior::Succeed,
                        Arc::clone(&closes),
                    ),
                ),
                store.clone(),
            )
            .unwrap();

        let RecoveryJournalReadOutcome::Ready(journal) = store.read() else {
            panic!("resource must be durable before registration returns");
        };
        assert_eq!(journal.entries(), &[entry]);
        assert_eq!(resources.tracked().count(), 1);

        assert_eq!(resources.shutdown(), Ok(()));
        assert_eq!(*closes.lock().unwrap(), vec!["native socket"]);
        assert_eq!(store.read(), RecoveryJournalReadOutcome::Missing);
    }

    #[test]
    fn staged_recoverable_resource_activates_only_after_durable_registration() {
        let root = tempdir().unwrap();
        let state = Arc::new(Mutex::new(FakeRecoveryJournalState::default()));
        let closes = Arc::new(Mutex::new(Vec::new()));
        let entry = durable_entry("worker", &root.path().join("worker.sock"));
        let mut resources = ApplicationResources::new();

        resources
            .register_recoverable_owned_with_journal(
                descriptor(ResourceKind::Process, "worker"),
                SequencedStage {
                    entry,
                    resource: Ok(RecordingResource::new(
                        "worker",
                        CloseBehavior::Succeed,
                        Arc::clone(&closes),
                    )),
                    state: Arc::clone(&state),
                },
                Box::new(FakeRecoveryJournal::new(&state)),
            )
            .unwrap();

        assert_eq!(
            state.lock().unwrap().events,
            vec!["register:worker", "activate:worker"]
        );
        resources.shutdown().unwrap();
    }

    #[test]
    fn duplicate_live_recovery_id_is_rejected_before_persistence_or_activation() {
        let root = tempdir().unwrap();
        let state = Arc::new(Mutex::new(FakeRecoveryJournalState::default()));
        let closes = Arc::new(Mutex::new(Vec::new()));
        let entry = durable_entry("socket", &root.path().join("socket.sock"));
        let mut resources = ApplicationResources::new();
        resources
            .register_recoverable_owned_with_journal(
                descriptor(ResourceKind::Socket, "first"),
                SequencedStage {
                    entry: entry.clone(),
                    resource: Ok(RecordingResource::new(
                        "first",
                        CloseBehavior::Succeed,
                        Arc::clone(&closes),
                    )),
                    state: Arc::clone(&state),
                },
                Box::new(FakeRecoveryJournal::new(&state)),
            )
            .unwrap();

        let error = resources
            .register_recoverable_owned_with_journal(
                descriptor(ResourceKind::Socket, "duplicate"),
                SequencedStage {
                    entry,
                    resource: Ok(RecordingResource::new(
                        "duplicate",
                        CloseBehavior::Succeed,
                        Arc::clone(&closes),
                    )),
                    state: Arc::clone(&state),
                },
                Box::new(FakeRecoveryJournal::new(&state)),
            )
            .unwrap_err();

        assert_eq!(
            error.kind(),
            ResourceRegistrationErrorKind::DuplicateRecoveryId
        );
        assert_eq!(
            state.lock().unwrap().events,
            vec!["register:socket", "activate:socket"]
        );
        assert_eq!(resources.tracked().count(), 1);
        resources.shutdown().unwrap();
        assert_eq!(*closes.lock().unwrap(), vec!["first"]);
    }

    #[test]
    fn activation_failure_retains_durable_entry_and_never_manages_a_resource() {
        let root = tempdir().unwrap();
        let state = Arc::new(Mutex::new(FakeRecoveryJournalState::default()));
        let entry = durable_entry("socket", &root.path().join("socket.sock"));
        let mut resources = ApplicationResources::new();

        let error = resources
            .register_recoverable_owned_with_journal(
                descriptor(ResourceKind::Socket, "socket"),
                SequencedStage::<RecordingResource> {
                    entry: entry.clone(),
                    resource: Err("bind refused"),
                    state: Arc::clone(&state),
                },
                Box::new(FakeRecoveryJournal::new(&state)),
            )
            .unwrap_err();

        assert_eq!(
            error.kind(),
            ResourceRegistrationErrorKind::ActivationFailed
        );
        assert_eq!(error.activation_failure(), Some("bind refused"));
        assert_eq!(
            state.lock().unwrap().events,
            vec!["register:socket", "activate:socket"]
        );
        assert_eq!(state.lock().unwrap().entries, vec![entry]);
        assert_eq!(resources.tracked().count(), 0);
    }

    #[test]
    fn activation_failure_after_create_is_cleaned_by_next_reconciliation() {
        let root = tempdir().unwrap();
        let target = root.path().join("socket.sock");
        let journal_path = root.path().join("owned-resources.json");
        let store = RecoveryJournalStore::new(&journal_path);
        let entry = DurableRecoveryEntry::new(
            "socket",
            "recover partial socket activation",
            ResourceOwnership::ApplicationOwned,
            DurableRecoveryRecipe::Filesystem(
                FilesystemRecoveryRecipe::new(
                    &target,
                    DurableIdentity::new("test.file.contents", "partial-live").unwrap(),
                )
                .unwrap(),
            ),
        )
        .unwrap();
        let mut resources = ApplicationResources::new();

        let error = resources
            .register_recoverable_owned(
                descriptor(ResourceKind::Socket, "socket"),
                FailAfterCreateStage {
                    entry: entry.clone(),
                    path: target.clone(),
                },
                store.clone(),
            )
            .unwrap_err();

        assert_eq!(
            error.kind(),
            ResourceRegistrationErrorKind::ActivationFailed
        );
        assert_eq!(error.activation_failure(), Some("failure after create"));
        assert!(target.exists());
        let RecoveryJournalReadOutcome::Ready(journal) = store.read() else {
            panic!("failed activation must remain durable");
        };
        assert_eq!(journal.entries(), &[entry]);
        assert_eq!(resources.tracked().count(), 0);

        let mut adapter = ExactTestFileAdapter;
        let RecoveryReconcileOutcome::Completed(report) = store.reconcile(&mut adapter).unwrap()
        else {
            panic!("next startup must reconcile the partial activation");
        };
        assert_eq!(report.remaining_entries(), 0);
        assert!(!target.exists());
        assert_eq!(store.read(), RecoveryJournalReadOutcome::Missing);
    }

    #[test]
    fn recoverable_close_and_destructor_failures_retain_both_durable_entries() {
        let root = tempdir().unwrap();
        let store = RecoveryJournalStore::new(root.path().join("owned-resources.json"));
        let close_entry = durable_entry("close-fails", &root.path().join("close.sock"));
        let drop_entry = durable_entry("drop-fails", &root.path().join("drop.sock"));
        let closes = Arc::new(Mutex::new(Vec::new()));
        let mut resources = ApplicationResources::new();
        resources
            .register_recoverable_owned(
                descriptor(ResourceKind::Socket, "close-fails"),
                staged(
                    close_entry.clone(),
                    RecordingResource::new(
                        "close-fails",
                        CloseBehavior::Fail("close refused"),
                        Arc::clone(&closes),
                    ),
                ),
                store.clone(),
            )
            .unwrap();
        resources
            .register_recoverable_owned(
                descriptor(ResourceKind::Socket, "drop-fails"),
                staged(
                    drop_entry.clone(),
                    PanickingDropResource {
                        inner: RecordingResource::new(
                            "drop-fails",
                            CloseBehavior::Succeed,
                            Arc::clone(&closes),
                        ),
                    },
                ),
                store.clone(),
            )
            .unwrap();

        let failures = resources.shutdown().unwrap_err();

        assert_eq!(*closes.lock().unwrap(), vec!["drop-fails", "close-fails"]);
        assert_eq!(failures.failures().len(), 2);
        let RecoveryJournalReadOutcome::Ready(journal) = store.read() else {
            panic!("failed cleanup must retain durable entries");
        };
        assert_eq!(journal.entries(), &[close_entry, drop_entry]);
    }

    #[test]
    fn durable_registration_failure_never_activates_or_manages_resource() {
        let root = tempdir().unwrap();
        let state = Arc::new(Mutex::new(FakeRecoveryJournalState {
            fail_registration: true,
            ..FakeRecoveryJournalState::default()
        }));
        let closes = Arc::new(Mutex::new(Vec::new()));
        let mut resources = ApplicationResources::new();

        let error = resources
            .register_recoverable_owned_with_journal(
                descriptor(ResourceKind::Process, "worker"),
                staged(
                    durable_entry("worker", &root.path().join("worker.sock")),
                    RecordingResource::new("worker", CloseBehavior::Succeed, Arc::clone(&closes)),
                ),
                Box::new(FakeRecoveryJournal::new(&state)),
            )
            .unwrap_err();

        assert_eq!(
            error.kind(),
            ResourceRegistrationErrorKind::RecoveryJournalRegistrationFailed
        );
        assert_eq!(
            error.recovery_journal_failure().unwrap().operation(),
            RecoveryJournalOperation::Registration
        );
        assert!(closes.lock().unwrap().is_empty());
        assert_eq!(resources.tracked().count(), 0);
        assert!(state.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn retirement_failure_is_typed_retained_and_idempotent() {
        let root = tempdir().unwrap();
        let state = Arc::new(Mutex::new(FakeRecoveryJournalState {
            fail_retirement: true,
            ..FakeRecoveryJournalState::default()
        }));
        let closes = Arc::new(Mutex::new(Vec::new()));
        let mut resources = ApplicationResources::new();
        resources
            .register_recoverable_owned_with_journal(
                descriptor(ResourceKind::Socket, "socket"),
                staged(
                    durable_entry("socket", &root.path().join("socket.sock")),
                    RecordingResource::new("socket", CloseBehavior::Succeed, Arc::clone(&closes)),
                ),
                Box::new(FakeRecoveryJournal::new(&state)),
            )
            .unwrap();

        let failures = resources
            .shutdown_with_timeout(Duration::from_millis(50))
            .unwrap_err();
        assert_eq!(
            failures.failures()[0].error().kind(),
            ResourceCloseErrorKind::RecoveryJournalRetirementFailed
        );
        assert_eq!(
            failures.failures()[0]
                .error()
                .recovery_journal_failure()
                .unwrap()
                .operation(),
            RecoveryJournalOperation::Retirement
        );
        assert_eq!(state.lock().unwrap().entries.len(), 1);

        assert_eq!(resources.shutdown(), Err(failures));
        assert_eq!(*closes.lock().unwrap(), vec!["socket"]);
        assert_eq!(
            state.lock().unwrap().events,
            vec!["register:socket", "retire:socket", "register:socket"]
        );
        let state = state.lock().unwrap();
        assert_eq!(state.deadlines.len(), 3);
        assert_eq!(state.deadlines[1], state.deadlines[2]);
        assert_eq!(state.deadlines[1].timeout(), Duration::from_millis(50));
    }

    #[test]
    fn recoverable_shutdown_retires_in_reverse_acquisition_order() {
        let root = tempdir().unwrap();
        let state = Arc::new(Mutex::new(FakeRecoveryJournalState::default()));
        let closes = Arc::new(Mutex::new(Vec::new()));
        let mut resources = ApplicationResources::new();
        for id in ["first", "second"] {
            resources
                .register_recoverable_owned_with_journal(
                    descriptor(ResourceKind::Socket, id),
                    staged(
                        durable_entry(id, &root.path().join(format!("{id}.sock"))),
                        RecordingResource::new(id, CloseBehavior::Succeed, Arc::clone(&closes)),
                    ),
                    Box::new(FakeRecoveryJournal::new(&state)),
                )
                .unwrap();
        }

        resources.shutdown().unwrap();

        assert_eq!(*closes.lock().unwrap(), vec!["second", "first"]);
        assert_eq!(
            state.lock().unwrap().events,
            vec![
                "register:first",
                "register:second",
                "retire:second",
                "retire:first"
            ]
        );
    }

    #[test]
    fn late_recoverable_registration_never_persists_or_activates() {
        let root = tempdir().unwrap();
        let successful_state = Arc::new(Mutex::new(FakeRecoveryJournalState::default()));
        let successful_closes = Arc::new(Mutex::new(Vec::new()));
        let mut successful = ApplicationResources::new();
        successful.shutdown().unwrap();

        let error = successful
            .register_recoverable_owned_with_journal(
                descriptor(ResourceKind::Socket, "late-success"),
                staged(
                    durable_entry("late-success", &root.path().join("late-success.sock")),
                    RecordingResource::new(
                        "late-success",
                        CloseBehavior::Succeed,
                        Arc::clone(&successful_closes),
                    ),
                ),
                Box::new(FakeRecoveryJournal::new(&successful_state)),
            )
            .unwrap_err();

        assert_eq!(
            error.kind(),
            ResourceRegistrationErrorKind::RegistryShutdown
        );
        assert!(error.cleanup_errors().is_empty());
        assert!(successful_closes.lock().unwrap().is_empty());
        assert!(successful_state.lock().unwrap().events.is_empty());
        assert!(successful_state.lock().unwrap().entries.is_empty());

        let failed_state = Arc::new(Mutex::new(FakeRecoveryJournalState::default()));
        let failed_closes = Arc::new(Mutex::new(Vec::new()));
        let mut failed = ApplicationResources::new();
        failed.shutdown().unwrap();
        let error = failed
            .register_recoverable_owned_with_journal(
                descriptor(ResourceKind::Socket, "late-failure"),
                staged(
                    durable_entry("late-failure", &root.path().join("late-failure.sock")),
                    RecordingResource::new(
                        "late-failure",
                        CloseBehavior::Fail("still busy"),
                        Arc::clone(&failed_closes),
                    ),
                ),
                Box::new(FakeRecoveryJournal::new(&failed_state)),
            )
            .unwrap_err();

        assert_eq!(
            error.kind(),
            ResourceRegistrationErrorKind::RegistryShutdown
        );
        assert!(error.cleanup_errors().is_empty());
        assert!(failed_closes.lock().unwrap().is_empty());
        assert!(failed_state.lock().unwrap().events.is_empty());
        assert!(failed_state.lock().unwrap().entries.is_empty());
    }

    #[test]
    fn drop_is_best_effort_continues_after_panic_and_does_not_unwind() {
        let closes = Arc::new(Mutex::new(Vec::new()));
        let drop_result = std::panic::catch_unwind({
            let closes = Arc::clone(&closes);
            move || {
                let mut resources = ApplicationResources::new();
                resources
                    .register_owned(
                        descriptor(ResourceKind::Process, "last"),
                        RecordingResource::new("last", CloseBehavior::Succeed, Arc::clone(&closes)),
                    )
                    .unwrap();
                resources
                    .register_owned(
                        descriptor(ResourceKind::Socket, "panics"),
                        RecordingResource::new("panics", CloseBehavior::Panic, Arc::clone(&closes)),
                    )
                    .unwrap();
            }
        });

        assert!(drop_result.is_ok());
        assert_eq!(*closes.lock().unwrap(), vec!["panics", "last"]);
    }

    #[test]
    fn partial_startup_failure_releases_every_resource_already_acquired() {
        let closes = Arc::new(Mutex::new(Vec::new()));
        let startup_result: Result<(), &'static str> = {
            let mut resources = ApplicationResources::new();
            resources
                .register_owned(
                    descriptor(ResourceKind::Socket, "listener"),
                    RecordingResource::new("listener", CloseBehavior::Succeed, Arc::clone(&closes)),
                )
                .unwrap();
            resources
                .register_owned(
                    descriptor(ResourceKind::Process, "pi"),
                    RecordingResource::new("pi", CloseBehavior::Succeed, Arc::clone(&closes)),
                )
                .unwrap();
            Err("window construction failed")
        };

        assert_eq!(startup_result, Err("window construction failed"));
        assert_eq!(*closes.lock().unwrap(), vec!["pi", "listener"]);
    }

    #[test]
    fn closing_and_reopening_window_preserves_session_resources() {
        let identity = ExactSessionIdentity::new("plot-7", "session-9").unwrap();
        let mut presentation = SessionPresentation::new(identity.clone());
        let closes = Arc::new(Mutex::new(Vec::new()));
        let mut resources = ApplicationResources::new();
        resources
            .register_owned(
                descriptor(ResourceKind::Session, "session-9"),
                RecordingResource::new("session-9", CloseBehavior::Succeed, Arc::clone(&closes)),
            )
            .unwrap();

        presentation.close_window();
        assert_eq!(presentation.identity(), &identity);
        assert_eq!(presentation.window_state(), WindowState::Closed);
        assert!(closes.lock().unwrap().is_empty());

        presentation.reopen_window();
        assert_eq!(presentation.identity(), &identity);
        assert_eq!(presentation.window_state(), WindowState::Open);
        assert!(closes.lock().unwrap().is_empty());

        resources.shutdown().unwrap();
        assert_eq!(*closes.lock().unwrap(), vec!["session-9"]);
    }

    #[test]
    fn switching_output_and_terminal_preserves_exact_session_identity() {
        let identity = ExactSessionIdentity::new("plot-7", "session-9").unwrap();
        let mut presentation = SessionPresentation::new(identity.clone());
        let mut resources = ApplicationResources::new();
        resources.register_borrowed(descriptor(ResourceKind::Session, "session-9"));

        assert_eq!(presentation.mode(), PresentationMode::StructuredOutput);
        presentation.switch_to(PresentationMode::Terminal);
        assert_eq!(presentation.identity(), &identity);
        assert_eq!(presentation.mode(), PresentationMode::Terminal);
        assert_eq!(resources.tracked().count(), 1);
        presentation.switch_to(PresentationMode::StructuredOutput);
        assert_eq!(presentation.identity(), &identity);
        assert_eq!(presentation.mode(), PresentationMode::StructuredOutput);
        assert_eq!(resources.tracked().count(), 1);
    }

    #[test]
    fn exact_session_identity_rejects_blank_plot_or_session_ids() {
        assert_eq!(
            ExactSessionIdentity::new("  ", "session-9"),
            Err(InvalidSessionIdentity::new(SessionIdentityField::PlotId))
        );
        assert_eq!(
            ExactSessionIdentity::new("plot-7", "\n\t"),
            Err(InvalidSessionIdentity::new(SessionIdentityField::SessionId))
        );
    }

    #[test]
    fn exact_session_identity_validates_reconciled_selection_at_binding_seam() {
        let selection = crate::reconcile::ExactSessionSelection::new("plot-7", "session-9");
        let identity = ExactSessionIdentity::try_from(&selection).unwrap();

        assert_eq!(identity.plot_id(), selection.plot_id());
        assert_eq!(identity.session_id(), selection.session_id());

        let invalid = crate::reconcile::ExactSessionSelection::new("plot-7", " ");
        assert_eq!(
            ExactSessionIdentity::try_from(&invalid),
            Err(InvalidSessionIdentity::new(SessionIdentityField::SessionId))
        );
    }

    #[test]
    fn application_resource_registry_can_be_owned_off_the_ui_thread() {
        fn assert_send<T: Send>() {}

        assert_send::<ApplicationResources>();
    }

    #[test]
    fn structured_output_degrades_honestly_to_same_session_terminal_and_recovers() {
        let identity = ExactSessionIdentity::new("plot-7", "session-9").unwrap();
        let mut presentation = SessionPresentation::new(identity.clone());
        presentation.close_window();

        presentation
            .structured_unavailable("Core activity feed disconnected; retry connection")
            .unwrap();

        assert_eq!(presentation.identity(), &identity);
        assert_eq!(presentation.mode(), PresentationMode::Terminal);
        assert_eq!(presentation.window_state(), WindowState::Closed);
        assert_eq!(
            presentation
                .structured_output_degradation()
                .unwrap()
                .diagnostic(),
            "Core activity feed disconnected; retry connection"
        );

        presentation.structured_restored();

        assert_eq!(presentation.identity(), &identity);
        assert_eq!(presentation.mode(), PresentationMode::StructuredOutput);
        assert_eq!(presentation.window_state(), WindowState::Closed);
        assert!(presentation.structured_output_degradation().is_none());
    }

    #[test]
    fn explicit_terminal_intent_survives_structured_outage_and_recovery() {
        let identity = ExactSessionIdentity::new("plot-7", "session-9").unwrap();
        let mut presentation = SessionPresentation::new(identity);
        presentation.switch_to(PresentationMode::Terminal);

        presentation
            .structured_unavailable("feed unavailable")
            .unwrap();
        assert_eq!(presentation.requested_mode(), PresentationMode::Terminal);
        assert_eq!(presentation.mode(), PresentationMode::Terminal);

        presentation.structured_restored();
        assert_eq!(presentation.requested_mode(), PresentationMode::Terminal);
        assert_eq!(presentation.mode(), PresentationMode::Terminal);
    }

    #[test]
    fn output_requested_during_outage_restores_only_after_recovery() {
        let identity = ExactSessionIdentity::new("plot-7", "session-9").unwrap();
        let mut presentation = SessionPresentation::new(identity);

        presentation
            .structured_unavailable("feed unavailable")
            .unwrap();
        presentation.switch_to(PresentationMode::Terminal);
        presentation.switch_to(PresentationMode::StructuredOutput);

        assert_eq!(
            presentation.requested_mode(),
            PresentationMode::StructuredOutput
        );
        assert_eq!(presentation.mode(), PresentationMode::Terminal);

        presentation.structured_restored();
        assert_eq!(presentation.mode(), PresentationMode::StructuredOutput);
    }

    #[test]
    fn requested_and_effective_presentation_follow_the_outage_transition_matrix() {
        for (initial_request, request_during_outage, expected_request) in [
            (
                PresentationMode::StructuredOutput,
                None,
                PresentationMode::StructuredOutput,
            ),
            (PresentationMode::Terminal, None, PresentationMode::Terminal),
            (
                PresentationMode::StructuredOutput,
                Some(PresentationMode::Terminal),
                PresentationMode::Terminal,
            ),
            (
                PresentationMode::Terminal,
                Some(PresentationMode::StructuredOutput),
                PresentationMode::StructuredOutput,
            ),
        ] {
            let identity = ExactSessionIdentity::new("plot-7", "session-9").unwrap();
            let mut presentation = SessionPresentation::new(identity);
            presentation.switch_to(initial_request);
            presentation
                .structured_unavailable("feed unavailable")
                .unwrap();
            if let Some(request) = request_during_outage {
                presentation.switch_to(request);
            }

            assert_eq!(presentation.requested_mode(), expected_request);
            assert_eq!(presentation.effective_mode(), PresentationMode::Terminal);

            presentation.structured_restored();
            assert_eq!(presentation.requested_mode(), expected_request);
            assert_eq!(presentation.effective_mode(), expected_request);
        }
    }

    #[test]
    fn structured_output_degradation_requires_a_visible_diagnostic() {
        let identity = ExactSessionIdentity::new("plot-7", "session-9").unwrap();
        let mut presentation = SessionPresentation::new(identity);

        assert_eq!(
            presentation.structured_unavailable(" \n "),
            Err(InvalidDegradationDiagnostic)
        );
        assert_eq!(presentation.mode(), PresentationMode::StructuredOutput);
        assert!(presentation.structured_output_degradation().is_none());
    }
}
