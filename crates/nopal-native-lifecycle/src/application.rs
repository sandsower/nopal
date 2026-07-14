//! Renderer-neutral composition of native Field startup.
//!
//! This module is deliberately above instance acquisition, restore storage,
//! Core reconciliation, and renderer construction. It keeps those boundaries
//! injectable while enforcing their startup order in one place.

use std::fmt;
use std::io;
use std::time::Duration;

use nopal_feed_client::field::FieldSnapshot;

use crate::activation::ActivationDeadline;
use crate::current_field::CurrentCoreFieldAuthority;
use crate::instance::{InstanceAcquisition, InstancePlatform};
use crate::preferences::{
    PreservedPreference, RestorePreferenceReadOutcome, RestorePreferenceStore,
    RestorePreferenceUpdate, RestorePreferenceWriteOutcome,
};
use crate::reconcile::{ExactSessionSelection, RestoreResolution, reconcile_restore};
use crate::recovery::{
    ExactRecoveryAdapter, PreservedRecoveryJournal, RecoveryJournalStore, RecoveryPassReport,
    RecoveryPersistenceFailure, RecoveryReconcileOutcome,
};
use crate::state_root::NativeInstanceScope;
use crate::supervisor::{
    NativeApplicationAck, NativeApplicationHost, NativeApplicationUnavailable,
    SecondaryActivationForwarder,
};

/// The startup boundary that made a native launch unavailable.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeApplicationStartStage {
    /// The operating-system singleton authority could not be acquired or joined.
    InstanceAcquisition,
    /// A secondary could not complete activation through the existing primary.
    SecondaryActivation,
    /// Prior application-owned resources could not be safely reconciled.
    OwnedResourceRecovery,
    /// Restore intent could not be read or safely interpreted.
    RestorePreference,
    /// Core could not provide one immutable Field snapshot.
    CoreSnapshot,
    /// The renderer adapter could not construct the primary host.
    HostConstruction,
}

impl fmt::Display for NativeApplicationStartStage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::InstanceAcquisition => "instance acquisition",
            Self::SecondaryActivation => "secondary activation",
            Self::OwnedResourceRecovery => "owned-resource recovery",
            Self::RestorePreference => "restore preference",
            Self::CoreSnapshot => "Core Field snapshot",
            Self::HostConstruction => "native host construction",
        };
        formatter.write_str(label)
    }
}

/// One typed, stage-specific native startup failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeApplicationStartError {
    stage: NativeApplicationStartStage,
    message: String,
}

impl NativeApplicationStartError {
    fn new(stage: NativeApplicationStartStage, message: impl Into<String>) -> Self {
        Self {
            stage,
            message: message.into(),
        }
    }

    /// Returns the boundary that failed.
    pub const fn stage(&self) -> NativeApplicationStartStage {
        self.stage
    }

    /// Returns the boundary's actionable diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for NativeApplicationStartError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} failed: {}", self.stage, self.message)
    }
}

impl std::error::Error for NativeApplicationStartError {}

/// Successful evidence from primary-only owned-resource startup recovery.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OwnedResourceRecoveryReport {
    /// No prior application-owned resources required cleanup.
    Empty,
    /// Every discovered entry was reconciled and durably retired.
    Reconciled(RecoveryPassReport),
}

/// Injectable primary-only boundary for durable owned-resource recovery.
pub trait PrimaryStartupRecovery {
    type Error: fmt::Display;

    /// Reconciles the journal belonging to the exact acquired instance scope.
    fn reconcile_for_scope(
        &mut self,
        scope: &NativeInstanceScope,
    ) -> Result<RecoveryReconcileOutcome, Self::Error>;
}

const OWNED_RESOURCE_RECOVERY_JOURNAL_FILE: &str = "owned-resources.json";

/// Production scope-derived recovery coordinator over the durable journal.
pub struct ScopedOwnedResourceRecovery<A> {
    adapter: A,
}

impl<A> ScopedOwnedResourceRecovery<A> {
    /// Creates a coordinator with the platform's exact-identity cleanup adapter.
    pub fn new(adapter: A) -> Self {
        Self { adapter }
    }

    /// Returns the exact journal path for a native instance scope.
    pub fn journal_path(scope: &NativeInstanceScope) -> std::path::PathBuf {
        scope
            .state_paths()
            .state_directory()
            .join(OWNED_RESOURCE_RECOVERY_JOURNAL_FILE)
    }

    /// Returns the adapter for inspection or further platform-specific setup.
    pub fn adapter_mut(&mut self) -> &mut A {
        &mut self.adapter
    }

    /// Returns ownership of the platform adapter.
    pub fn into_adapter(self) -> A {
        self.adapter
    }
}

impl<A> PrimaryStartupRecovery for ScopedOwnedResourceRecovery<A>
where
    A: ExactRecoveryAdapter,
{
    type Error = RecoveryPersistenceFailure;

    fn reconcile_for_scope(
        &mut self,
        scope: &NativeInstanceScope,
    ) -> Result<RecoveryReconcileOutcome, Self::Error> {
        RecoveryJournalStore::new(Self::journal_path(scope)).reconcile(&mut self.adapter)
    }
}

/// Preserved preference content that could not safely supply restore intent.
///
/// These notices are not startup failures. The primary ignores the unsafe
/// intent, reconciles deterministic fallback from Core, and gives the renderer
/// an actionable notice to show or log.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RestorePreferenceNotice {
    /// Existing content was not exact valid v1 JSON.
    Malformed {
        /// Diagnostic from the bounded preference decoder.
        message: String,
    },
    /// Existing content belongs to a newer unsupported contract.
    FutureVersion {
        /// Exact unsupported kind value.
        version: String,
    },
    /// Existing content exceeded the bounded preference wire size.
    Oversized {
        /// Maximum document size accepted by this build.
        max_bytes: usize,
        /// Size reported by the file or bounded read.
        observed_bytes: Option<u64>,
        /// Actionable diagnostic from the bounded preference reader.
        message: String,
    },
    /// Existing content could not be opened, inspected, or read safely.
    Unreadable {
        /// Actionable diagnostic from the bounded preference reader.
        message: String,
    },
}

/// Scope-aware restore source used only after this process becomes primary.
pub trait NativeRestorePreferenceSource {
    /// Reads the bounded preference document belonging to the exact instance scope.
    fn read_for_scope(
        &self,
        scope: &NativeInstanceScope,
    ) -> io::Result<RestorePreferenceReadOutcome>;
}

/// Production restore source backed by the scope's versioned preference file.
#[derive(Clone, Copy, Debug, Default)]
pub struct ScopedRestorePreferenceSource;

impl NativeRestorePreferenceSource for ScopedRestorePreferenceSource {
    fn read_for_scope(
        &self,
        scope: &NativeInstanceScope,
    ) -> io::Result<RestorePreferenceReadOutcome> {
        RestorePreferenceStore::new(scope.state_paths().restore_preference()).read()
    }
}

/// Core boundary that supplies one immutable Field snapshot to a primary.
pub trait CoreFieldSnapshotSource {
    /// Loads the sole snapshot used for restore reconciliation and host startup.
    fn load_field_snapshot(&self) -> Result<FieldSnapshot, NativeApplicationUnavailable>;
}

/// Typed result of a renderer-requested native selection persistence update.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeSelectionWriteOutcome {
    /// A complete new preference document was atomically installed.
    Written,
    /// No preference document was written.
    NotWritten(NativeSelectionNotWrittenReason),
}

/// The exact reason a native selection persistence request did not write.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum NativeSelectionNotWrittenReason {
    /// The requested pair was not an exact fact in the startup Core snapshot.
    NotExact {
        /// Core-derived reconciliation evidence explaining the rejection.
        resolution: RestoreResolution,
    },
    /// The preference store rejected an invalid selection identity.
    UnknownSelection,
    /// The encoded preference document exceeded the bounded wire size.
    Oversized {
        /// The maximum document size accepted by this build.
        max_bytes: usize,
        /// The number of bytes the requested document would require.
        encoded_bytes: usize,
    },
    /// Existing unsafe content was preserved without mutation.
    PreservedUnreadable(PreservedPreference),
}

fn map_store_write_outcome(outcome: RestorePreferenceWriteOutcome) -> NativeSelectionWriteOutcome {
    match outcome {
        RestorePreferenceWriteOutcome::Written => NativeSelectionWriteOutcome::Written,
        RestorePreferenceWriteOutcome::RejectedUnknownSelection => {
            NativeSelectionWriteOutcome::NotWritten(
                NativeSelectionNotWrittenReason::UnknownSelection,
            )
        }
        RestorePreferenceWriteOutcome::RejectedOversized {
            max_bytes,
            encoded_bytes,
        } => NativeSelectionWriteOutcome::NotWritten(NativeSelectionNotWrittenReason::Oversized {
            max_bytes,
            encoded_bytes,
        }),
        RestorePreferenceWriteOutcome::PreservedUnreadable(preserved) => {
            NativeSelectionWriteOutcome::NotWritten(
                NativeSelectionNotWrittenReason::PreservedUnreadable(preserved),
            )
        }
    }
}

/// Scope-bound persistence for renderer selection intent.
///
/// The handle retains the immutable Core snapshot used to construct the host.
/// A renderer can therefore persist only a Plot and Session pair that reconciles
/// exactly against Core facts, or explicitly clear intent. The preference remains
/// desktop intent and is reconciled again against a fresh Core snapshot at every
/// restart.
#[derive(Clone, Debug)]
pub struct NativeSelectionPersistence {
    store: RestorePreferenceStore,
    field: FieldSnapshot,
}

impl NativeSelectionPersistence {
    pub(crate) fn for_scope(scope: &NativeInstanceScope, field: &FieldSnapshot) -> Self {
        Self::for_restore_path(scope.state_paths().restore_preference(), field)
    }

    /// Creates persistence for an exact native restore-preference path.
    ///
    /// Native application startup normally derives this path from its acquired
    /// instance scope. Renderer-neutral coordinators and embedders may use this
    /// constructor when that scope has already been resolved by their host.
    pub fn for_restore_path(path: impl Into<std::path::PathBuf>, field: &FieldSnapshot) -> Self {
        Self {
            store: RestorePreferenceStore::new(path.into()),
            field: field.clone(),
        }
    }

    pub(crate) fn replace_field(&mut self, field: FieldSnapshot) {
        self.field = field;
    }

    /// Returns the exact scope-derived preference path.
    pub fn path(&self) -> &std::path::Path {
        self.store.path()
    }

    /// Persists a pair only when it is an exact fact in the startup Core snapshot.
    pub fn select(
        &self,
        selection: &ExactSessionSelection,
    ) -> io::Result<NativeSelectionWriteOutcome> {
        match reconcile_restore(&self.field, Some(selection)) {
            RestoreResolution::Exact(exact) => self
                .store
                .write(&RestorePreferenceUpdate::select(exact))
                .map(map_store_write_outcome),
            resolution => Ok(NativeSelectionWriteOutcome::NotWritten(
                NativeSelectionNotWrittenReason::NotExact { resolution },
            )),
        }
    }

    /// Explicitly clears prior native selection intent.
    pub fn clear(&self) -> io::Result<NativeSelectionWriteOutcome> {
        self.store
            .write(&RestorePreferenceUpdate::ClearSelection)
            .map(map_store_write_outcome)
    }
}

/// Renderer seam that constructs a host from exact Core facts and restore resolution.
pub trait ResolvedNativeApplicationHostFactory {
    type Host: NativeApplicationHost;

    /// Constructs the one primary host after all renderer-neutral startup work succeeds.
    fn create_host(
        &self,
        field: &FieldSnapshot,
        restore: &RestoreResolution,
        recovery_report: &OwnedResourceRecoveryReport,
        preference_notice: Option<&RestorePreferenceNotice>,
        current_field: CurrentCoreFieldAuthority,
    ) -> Result<Self::Host, NativeApplicationUnavailable>;
}

/// A successfully composed primary application.
///
/// Field order is intentional because Rust drops fields in declaration order.
/// The renderer host is declared first and the singleton lease last so every
/// host-owned runtime binding finishes cleanup before singleton authority is
/// released. Do not reorder these fields without preserving that invariant.
/// The application intentionally has no decomposition API that can release the
/// lease before dropping the host:
///
/// ```compile_fail
/// use nopal_native_lifecycle::application::NativePrimaryApplication;
/// use nopal_native_lifecycle::supervisor::NativeApplicationHost;
///
/// fn cannot_dismantle<L, H: NativeApplicationHost>(application: NativePrimaryApplication<L, H>) {
///     let _ = application.into_parts();
/// }
/// ```
pub struct NativePrimaryApplication<L, H> {
    host: H,
    field: FieldSnapshot,
    restore: RestoreResolution,
    recovery_report: OwnedResourceRecoveryReport,
    preference_notice: Option<RestorePreferenceNotice>,
    lease: L,
}

impl<L, H> NativePrimaryApplication<L, H>
where
    H: NativeApplicationHost,
{
    /// Keeps the operating-system primary lease observable without transferring it.
    pub fn lease(&self) -> &L {
        &self.lease
    }

    /// Returns the immutable Core snapshot used for this startup.
    pub fn field(&self) -> &FieldSnapshot {
        &self.field
    }

    /// Returns the exact or deterministic restore decision given to the host.
    pub fn restore_resolution(&self) -> &RestoreResolution {
        &self.restore
    }

    /// Returns successful primary-only owned-resource recovery evidence.
    pub fn recovery_report(&self) -> &OwnedResourceRecoveryReport {
        &self.recovery_report
    }

    /// Returns preserved unsafe preference content the host may show or log.
    pub fn preference_notice(&self) -> Option<&RestorePreferenceNotice> {
        self.preference_notice.as_ref()
    }

    /// Delegates one focus or reopen decision to the renderer-specific host.
    pub fn activate(
        &mut self,
        deadline: ActivationDeadline,
    ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
        self.host.activate(deadline)
    }
}

/// The complete result of one native process startup attempt.
pub enum NativeApplicationStart<L, H> {
    /// This process owns the singleton lease and constructed the sole host.
    Primary(Box<NativePrimaryApplication<L, H>>),
    /// This process activated the existing primary and must now exit.
    Secondary {
        /// The completed focus or reopen action acknowledged by the primary.
        acknowledgement: NativeApplicationAck,
    },
}

/// Acquires the singleton role and composes only the work permitted for that role.
///
/// `platform` must have been constructed for `scope`. A secondary performs only
/// acquisition and activation forwarding. It never reads restore intent, loads
/// Core, reconciles selection, or constructs a renderer host. A primary holds its
/// lease while it reads bounded intent, loads exactly one Core snapshot, reconciles
/// against that snapshot, and constructs exactly one host.
pub fn start_native_application<P, O, R, C, F>(
    scope: &NativeInstanceScope,
    platform: &P,
    owned_resource_recovery: &mut O,
    restore_source: &R,
    core_source: &C,
    host_factory: &F,
    secondary_connect_timeout: Duration,
) -> Result<NativeApplicationStart<P::Primary, F::Host>, NativeApplicationStartError>
where
    P: InstancePlatform,
    P::Secondary: SecondaryActivationForwarder,
    O: PrimaryStartupRecovery + ?Sized,
    R: NativeRestorePreferenceSource + ?Sized,
    C: CoreFieldSnapshotSource + ?Sized,
    F: ResolvedNativeApplicationHostFactory + ?Sized,
{
    let acquisition = platform
        .acquire(secondary_connect_timeout)
        .map_err(|error| {
            NativeApplicationStartError::new(
                NativeApplicationStartStage::InstanceAcquisition,
                error.to_string(),
            )
        })?;

    let primary_lease = match acquisition {
        InstanceAcquisition::Secondary(secondary) => {
            let acknowledgement = secondary.forward().map_err(|error| {
                NativeApplicationStartError::new(
                    NativeApplicationStartStage::SecondaryActivation,
                    error.to_string(),
                )
            })?;
            return Ok(NativeApplicationStart::Secondary { acknowledgement });
        }
        InstanceAcquisition::Primary(lease) => lease,
    };

    let recovery_report = reconcile_owned_resources(owned_resource_recovery, scope)?;
    let preference = restore_source.read_for_scope(scope).map_err(|error| {
        NativeApplicationStartError::new(
            NativeApplicationStartStage::RestorePreference,
            error.to_string(),
        )
    })?;
    let (intent, preference_notice) = preference_intent(preference);
    let field = core_source.load_field_snapshot().map_err(|error| {
        NativeApplicationStartError::new(
            NativeApplicationStartStage::CoreSnapshot,
            error.to_string(),
        )
    })?;
    let restore = reconcile_restore(&field, intent.as_ref());
    let selection_persistence = NativeSelectionPersistence::for_scope(scope, &field);
    let current_field =
        CurrentCoreFieldAuthority::from_startup(field.clone(), selection_persistence);
    let host = host_factory
        .create_host(
            &field,
            &restore,
            &recovery_report,
            preference_notice.as_ref(),
            current_field,
        )
        .map_err(|error| {
            NativeApplicationStartError::new(
                NativeApplicationStartStage::HostConstruction,
                error.to_string(),
            )
        })?;

    Ok(NativeApplicationStart::Primary(Box::new(
        NativePrimaryApplication {
            host,
            field,
            restore,
            recovery_report,
            preference_notice,
            lease: primary_lease,
        },
    )))
}

fn reconcile_owned_resources<O>(
    recovery: &mut O,
    scope: &NativeInstanceScope,
) -> Result<OwnedResourceRecoveryReport, NativeApplicationStartError>
where
    O: PrimaryStartupRecovery + ?Sized,
{
    let outcome = recovery.reconcile_for_scope(scope).map_err(|error| {
        NativeApplicationStartError::new(
            NativeApplicationStartStage::OwnedResourceRecovery,
            error.to_string(),
        )
    })?;
    match outcome {
        RecoveryReconcileOutcome::Empty => Ok(OwnedResourceRecoveryReport::Empty),
        RecoveryReconcileOutcome::Completed(report) if report.remaining_entries() == 0 => {
            Ok(OwnedResourceRecoveryReport::Reconciled(report))
        }
        RecoveryReconcileOutcome::Completed(report) => Err(NativeApplicationStartError::new(
            NativeApplicationStartStage::OwnedResourceRecovery,
            format!(
                "{} application-owned recovery entry or entries remain unresolved after {} attempt(s); inspect the recovery journal before restarting native Field",
                report.remaining_entries(),
                report.attempts().len(),
            ),
        )),
        RecoveryReconcileOutcome::Blocked(preserved) => Err(blocked_recovery_error(&preserved)),
    }
}

fn blocked_recovery_error(preserved: &PreservedRecoveryJournal) -> NativeApplicationStartError {
    NativeApplicationStartError::new(
        NativeApplicationStartStage::OwnedResourceRecovery,
        preserved.diagnostic(),
    )
}

fn preference_intent(
    outcome: RestorePreferenceReadOutcome,
) -> (
    Option<ExactSessionSelection>,
    Option<RestorePreferenceNotice>,
) {
    match outcome {
        RestorePreferenceReadOutcome::Missing => (None, None),
        RestorePreferenceReadOutcome::Ready(preference) => (preference.selection, None),
        RestorePreferenceReadOutcome::Malformed { message } => {
            (None, Some(RestorePreferenceNotice::Malformed { message }))
        }
        RestorePreferenceReadOutcome::FutureVersion { version } => (
            None,
            Some(RestorePreferenceNotice::FutureVersion { version }),
        ),
        RestorePreferenceReadOutcome::Oversized {
            max_bytes,
            observed_bytes,
            message,
        } => (
            None,
            Some(RestorePreferenceNotice::Oversized {
                max_bytes,
                observed_bytes,
                message,
            }),
        ),
        RestorePreferenceReadOutcome::Unreadable { message } => {
            (None, Some(RestorePreferenceNotice::Unreadable { message }))
        }
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};
    use std::fs;
    use std::io;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use nopal_feed_client::field::FieldSnapshot;

    use super::{
        CoreFieldSnapshotSource, NativeApplicationStart, NativeApplicationStartStage,
        NativeRestorePreferenceSource, NativeSelectionNotWrittenReason, NativeSelectionPersistence,
        NativeSelectionWriteOutcome, OwnedResourceRecoveryReport, PrimaryStartupRecovery,
        ResolvedNativeApplicationHostFactory, RestorePreferenceNotice, ScopedOwnedResourceRecovery,
        ScopedRestorePreferenceSource, map_store_write_outcome, start_native_application,
    };
    use crate::activation::ActivationDeadline;
    use crate::current_field::CurrentCoreFieldAuthority;
    use crate::instance::{InstanceAcquisition, InstancePlatform};
    use crate::preferences::{
        MAX_RESTORE_PREFERENCE_BYTES, PreservedPreference, RestorePreference,
        RestorePreferenceReadOutcome, RestorePreferenceStore, RestorePreferenceUpdate,
        RestorePreferenceWriteOutcome,
    };
    use crate::reconcile::{
        ExactSessionSelection, RestoreFallbackReason, RestoreResolution, RestoreSelection,
    };
    use crate::recovery::{
        DurableIdentity, DurableRecoveryEntry, DurableRecoveryRecipe, ExactRecoveryAdapter,
        FilesystemRecoveryRecipe, PreservedRecoveryJournal, RecoveryAdapterError, RecoveryDeadline,
        RecoveryDisposition, RecoveryJournalStore, RecoveryJournalUpdateOutcome,
        RecoveryReconcileOutcome, VerifiedProcessRecoveryRecipe,
    };
    use crate::resources::ResourceOwnership;
    use crate::state_root::{CanonicalStateRoot, NativeInstanceScope, ReleaseChannel};
    use crate::supervisor::{
        NativeApplicationAck, NativeApplicationHost, NativeApplicationUnavailable,
        SecondaryActivationForwarder,
    };

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct FakeLease(&'static str);

    struct FakeSecondary {
        calls: Arc<AtomicUsize>,
        result: Result<NativeApplicationAck, NativeApplicationUnavailable>,
    }

    impl SecondaryActivationForwarder for FakeSecondary {
        fn forward(&self) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    struct FakePlatform {
        acquisition: RefCell<Option<io::Result<InstanceAcquisition<FakeLease, FakeSecondary>>>>,
        calls: Cell<usize>,
    }

    impl FakePlatform {
        fn primary() -> Self {
            Self {
                acquisition: RefCell::new(Some(Ok(InstanceAcquisition::Primary(FakeLease(
                    "primary",
                ))))),
                calls: Cell::new(0),
            }
        }

        fn secondary(
            calls: Arc<AtomicUsize>,
            result: Result<NativeApplicationAck, NativeApplicationUnavailable>,
        ) -> Self {
            Self {
                acquisition: RefCell::new(Some(Ok(InstanceAcquisition::Secondary(
                    FakeSecondary { calls, result },
                )))),
                calls: Cell::new(0),
            }
        }
    }

    impl InstancePlatform for FakePlatform {
        type Primary = FakeLease;
        type Secondary = FakeSecondary;

        fn acquire(
            &self,
            _secondary_connect_timeout: Duration,
        ) -> io::Result<InstanceAcquisition<Self::Primary, Self::Secondary>> {
            self.calls.set(self.calls.get() + 1);
            self.acquisition
                .borrow_mut()
                .take()
                .expect("fake platform must be acquired exactly once")
        }
    }

    struct FakeRecovery {
        calls: Cell<usize>,
        result: RefCell<Option<io::Result<RecoveryReconcileOutcome>>>,
    }

    impl FakeRecovery {
        fn empty() -> Self {
            Self {
                calls: Cell::new(0),
                result: RefCell::new(Some(Ok(RecoveryReconcileOutcome::Empty))),
            }
        }

        fn returning(outcome: RecoveryReconcileOutcome) -> Self {
            Self {
                calls: Cell::new(0),
                result: RefCell::new(Some(Ok(outcome))),
            }
        }

        fn failing(kind: io::ErrorKind, message: &'static str) -> Self {
            Self {
                calls: Cell::new(0),
                result: RefCell::new(Some(Err(io::Error::new(kind, message)))),
            }
        }
    }

    impl PrimaryStartupRecovery for FakeRecovery {
        type Error = io::Error;

        fn reconcile_for_scope(
            &mut self,
            _scope: &NativeInstanceScope,
        ) -> Result<RecoveryReconcileOutcome, Self::Error> {
            self.calls.set(self.calls.get() + 1);
            self.result
                .borrow_mut()
                .take()
                .expect("fake recovery must run exactly once")
        }
    }

    struct ScriptedRecoveryAdapter {
        calls: Arc<AtomicUsize>,
        result: Result<RecoveryDisposition, RecoveryAdapterError>,
    }

    impl ExactRecoveryAdapter for ScriptedRecoveryAdapter {
        fn recover_filesystem_exact(
            &mut self,
            _recipe: &FilesystemRecoveryRecipe,
            _deadline: RecoveryDeadline,
        ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }

        fn recover_process_exact(
            &mut self,
            _recipe: &VerifiedProcessRecoveryRecipe,
            _deadline: RecoveryDeadline,
        ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.result.clone()
        }
    }

    struct FakePreferenceSource {
        calls: Cell<usize>,
        result: RefCell<Option<io::Result<RestorePreferenceReadOutcome>>>,
    }

    impl FakePreferenceSource {
        fn returning(outcome: RestorePreferenceReadOutcome) -> Self {
            Self {
                calls: Cell::new(0),
                result: RefCell::new(Some(Ok(outcome))),
            }
        }

        fn failing(kind: io::ErrorKind, message: &'static str) -> Self {
            Self {
                calls: Cell::new(0),
                result: RefCell::new(Some(Err(io::Error::new(kind, message)))),
            }
        }
    }

    impl NativeRestorePreferenceSource for FakePreferenceSource {
        fn read_for_scope(
            &self,
            _scope: &NativeInstanceScope,
        ) -> io::Result<RestorePreferenceReadOutcome> {
            self.calls.set(self.calls.get() + 1);
            self.result
                .borrow_mut()
                .take()
                .expect("fake preference source must be read exactly once")
        }
    }

    struct FakeCoreSource {
        calls: Cell<usize>,
        result: RefCell<Option<Result<FieldSnapshot, NativeApplicationUnavailable>>>,
    }

    impl FakeCoreSource {
        fn returning(field: FieldSnapshot) -> Self {
            Self {
                calls: Cell::new(0),
                result: RefCell::new(Some(Ok(field))),
            }
        }

        fn failing(message: &'static str) -> Self {
            Self {
                calls: Cell::new(0),
                result: RefCell::new(Some(Err(unavailable(message)))),
            }
        }
    }

    impl CoreFieldSnapshotSource for FakeCoreSource {
        fn load_field_snapshot(&self) -> Result<FieldSnapshot, NativeApplicationUnavailable> {
            self.calls.set(self.calls.get() + 1);
            self.result
                .borrow_mut()
                .take()
                .expect("fake Core source must be loaded exactly once")
        }
    }

    struct FakeHost {
        activations: Arc<AtomicUsize>,
        current_field: CurrentCoreFieldAuthority,
    }

    impl FakeHost {
        fn selection_persistence(&self) -> &NativeSelectionPersistence {
            self.current_field.persistence()
        }
    }

    impl NativeApplicationHost for FakeHost {
        fn activate(
            &mut self,
            _deadline: ActivationDeadline,
        ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
            self.activations.fetch_add(1, Ordering::SeqCst);
            Ok(NativeApplicationAck::Focused)
        }
    }

    type HostStartRecord = (
        FieldSnapshot,
        RestoreResolution,
        OwnedResourceRecoveryReport,
        Option<RestorePreferenceNotice>,
    );

    struct FakeHostFactory {
        calls: Cell<usize>,
        received: Arc<Mutex<Vec<HostStartRecord>>>,
        activations: Arc<AtomicUsize>,
        result: RefCell<Option<Result<(), NativeApplicationUnavailable>>>,
    }

    impl FakeHostFactory {
        fn available() -> Self {
            Self {
                calls: Cell::new(0),
                received: Arc::new(Mutex::new(Vec::new())),
                activations: Arc::new(AtomicUsize::new(0)),
                result: RefCell::new(Some(Ok(()))),
            }
        }

        fn failing(message: &'static str) -> Self {
            Self {
                result: RefCell::new(Some(Err(unavailable(message)))),
                ..Self::available()
            }
        }
    }

    impl ResolvedNativeApplicationHostFactory for FakeHostFactory {
        type Host = FakeHost;

        fn create_host(
            &self,
            field: &FieldSnapshot,
            restore: &RestoreResolution,
            recovery_report: &OwnedResourceRecoveryReport,
            preference_notice: Option<&RestorePreferenceNotice>,
            current_field: CurrentCoreFieldAuthority,
        ) -> Result<Self::Host, NativeApplicationUnavailable> {
            self.calls.set(self.calls.get() + 1);
            self.received
                .lock()
                .expect("recording lock should remain available")
                .push((
                    field.clone(),
                    restore.clone(),
                    recovery_report.clone(),
                    preference_notice.cloned(),
                ));
            self.result
                .borrow_mut()
                .take()
                .expect("fake host factory must be called exactly once")
                .map(|()| FakeHost {
                    activations: Arc::clone(&self.activations),
                    current_field,
                })
        }
    }

    #[test]
    fn secondary_forwards_and_exits_without_reading_primary_sources() {
        let forward_calls = Arc::new(AtomicUsize::new(0));
        let platform = FakePlatform::secondary(
            Arc::clone(&forward_calls),
            Ok(NativeApplicationAck::Reopened),
        );
        let preferences = FakePreferenceSource::returning(RestorePreferenceReadOutcome::Missing);
        let core = FakeCoreSource::returning(field(&[]));
        let factory = FakeHostFactory::available();
        let mut recovery = FakeRecovery::empty();

        let result = start_native_application(
            &scope(),
            &platform,
            &mut recovery,
            &preferences,
            &core,
            &factory,
            Duration::from_millis(50),
        )
        .expect("secondary should activate the existing primary");

        assert!(matches!(
            result,
            NativeApplicationStart::Secondary {
                acknowledgement: NativeApplicationAck::Reopened
            }
        ));
        assert_eq!(platform.calls.get(), 1);
        assert_eq!(forward_calls.load(Ordering::SeqCst), 1);
        assert_eq!(recovery.calls.get(), 0);
        assert_eq!(preferences.calls.get(), 0);
        assert_eq!(core.calls.get(), 0);
        assert_eq!(factory.calls.get(), 0);
    }

    #[test]
    fn primary_restores_exact_pair_and_constructs_one_host_from_one_snapshot() {
        let intended = ExactSessionSelection::new("plot-b", "session-b2");
        let field = field(&[
            ("plot-a", Some("session-a"), &["session-a"]),
            ("plot-b", Some("session-b1"), &["session-b1", "session-b2"]),
        ]);
        let platform = FakePlatform::primary();
        let preferences = FakePreferenceSource::returning(RestorePreferenceReadOutcome::Ready(
            RestorePreference {
                selection: Some(intended.clone()),
            },
        ));
        let core = FakeCoreSource::returning(field.clone());
        let factory = FakeHostFactory::available();
        let mut recovery = FakeRecovery::empty();

        let result = start_native_application(
            &scope(),
            &platform,
            &mut recovery,
            &preferences,
            &core,
            &factory,
            Duration::from_millis(50),
        )
        .expect("primary should compose");
        let NativeApplicationStart::Primary(mut application) = result else {
            panic!("primary acquisition must not return secondary");
        };

        assert_eq!(application.lease(), &FakeLease("primary"));
        assert_eq!(application.field(), &field);
        assert_eq!(
            application.restore_resolution(),
            &RestoreResolution::Exact(intended.clone())
        );
        assert_eq!(
            application.recovery_report(),
            &OwnedResourceRecoveryReport::Empty
        );
        assert_eq!(application.preference_notice(), None);
        assert_eq!(
            application.host.current_field.accepted().as_ref(),
            application.field()
        );
        assert_eq!(
            application
                .activate(activation_deadline())
                .expect("host should activate"),
            NativeApplicationAck::Focused
        );
        assert_eq!(preferences.calls.get(), 1);
        assert_eq!(recovery.calls.get(), 1);
        assert_eq!(core.calls.get(), 1);
        assert_eq!(factory.calls.get(), 1);
        assert_eq!(
            factory
                .received
                .lock()
                .expect("recording lock should remain available")
                .as_slice(),
            &[(
                field,
                RestoreResolution::Exact(intended),
                OwnedResourceRecoveryReport::Empty,
                None,
            )]
        );
    }

    #[test]
    fn stale_pair_uses_deterministic_core_order_before_host_construction() {
        let field = field(&[
            (
                "plot-first",
                Some("session-selected"),
                &["session-first", "session-selected"],
            ),
            ("plot-old", None, &["session-other"]),
        ]);
        let preferences = FakePreferenceSource::returning(RestorePreferenceReadOutcome::Ready(
            RestorePreference {
                selection: Some(ExactSessionSelection::new("plot-old", "session-missing")),
            },
        ));
        let core = FakeCoreSource::returning(field);
        let factory = FakeHostFactory::available();

        let result = start_native_application(
            &scope(),
            &FakePlatform::primary(),
            &mut FakeRecovery::empty(),
            &preferences,
            &core,
            &factory,
            Duration::from_millis(50),
        )
        .expect("stale intent should use deterministic fallback");
        let NativeApplicationStart::Primary(application) = result else {
            panic!("primary acquisition must not return secondary");
        };

        assert_eq!(
            application.restore_resolution(),
            &RestoreResolution::Fallback {
                selection: RestoreSelection::Session(ExactSessionSelection::new(
                    "plot-first",
                    "session-selected",
                )),
                reason: RestoreFallbackReason::SessionMissing {
                    plot_id: "plot-old".to_owned(),
                    session_id: "session-missing".to_owned(),
                },
            }
        );
        assert_eq!(factory.calls.get(), 1);
    }

    #[test]
    fn scoped_source_reads_the_exact_bounded_preference_path() {
        let sandbox = tempfile::tempdir().expect("create preference sandbox");
        let scope = NativeInstanceScope::new(
            CanonicalStateRoot::create(sandbox.path().join("state"))
                .expect("create canonical state root"),
            ReleaseChannel::Preview,
        );
        let store = RestorePreferenceStore::new(scope.state_paths().restore_preference());
        let intended = ExactSessionSelection::new("plot-exact", "session-exact");
        assert_eq!(
            store
                .write(&RestorePreferenceUpdate::select(intended.clone()))
                .expect("write scoped preference"),
            crate::preferences::RestorePreferenceWriteOutcome::Written
        );

        assert_eq!(
            ScopedRestorePreferenceSource
                .read_for_scope(&scope)
                .expect("read scoped preference"),
            RestorePreferenceReadOutcome::Ready(RestorePreference {
                selection: Some(intended),
            })
        );
    }

    #[test]
    fn exact_selection_persists_and_restores_exactly_after_restart() {
        let scope = scope();
        let snapshot = field(&[
            ("plot-a", Some("session-a"), &["session-a"]),
            ("plot-b", Some("session-b"), &["session-b"]),
        ]);
        let intended = ExactSessionSelection::new("plot-b", "session-b");

        let application = start_primary_with_scoped_preferences(&scope, snapshot.clone());
        assert_eq!(
            application
                .host
                .selection_persistence()
                .select(&intended)
                .expect("persist exact Core selection"),
            NativeSelectionWriteOutcome::Written
        );
        drop(application);

        let restarted = start_primary_with_scoped_preferences(&scope, snapshot);
        assert_eq!(
            restarted.restore_resolution(),
            &RestoreResolution::Exact(intended)
        );
    }

    #[test]
    fn persisted_selection_with_stale_plot_uses_deterministic_core_fallback() {
        let scope = scope();
        let intended = ExactSessionSelection::new("plot-old", "session-old");
        let application = start_primary_with_scoped_preferences(
            &scope,
            field(&[("plot-old", Some("session-old"), &["session-old"])]),
        );
        assert!(matches!(
            application
                .host
                .selection_persistence()
                .select(&intended)
                .expect("persist exact Core selection"),
            NativeSelectionWriteOutcome::Written
        ));
        drop(application);

        let restarted = start_primary_with_scoped_preferences(
            &scope,
            field(&[("plot-first", Some("session-first"), &["session-first"])]),
        );
        assert_eq!(
            restarted.restore_resolution(),
            &RestoreResolution::Fallback {
                selection: RestoreSelection::Session(ExactSessionSelection::new(
                    "plot-first",
                    "session-first",
                )),
                reason: RestoreFallbackReason::PlotMissing {
                    plot_id: "plot-old".to_owned(),
                },
            }
        );
    }

    #[test]
    fn persisted_selection_with_stale_session_uses_deterministic_core_fallback() {
        let scope = scope();
        let intended = ExactSessionSelection::new("plot-a", "session-old");
        let application = start_primary_with_scoped_preferences(
            &scope,
            field(&[("plot-a", Some("session-old"), &["session-old"])]),
        );
        assert!(matches!(
            application
                .host
                .selection_persistence()
                .select(&intended)
                .expect("persist exact Core selection"),
            NativeSelectionWriteOutcome::Written
        ));
        drop(application);

        let restarted = start_primary_with_scoped_preferences(
            &scope,
            field(&[("plot-a", Some("session-new"), &["session-new"])]),
        );
        assert_eq!(
            restarted.restore_resolution(),
            &RestoreResolution::Fallback {
                selection: RestoreSelection::Session(ExactSessionSelection::new(
                    "plot-a",
                    "session-new",
                )),
                reason: RestoreFallbackReason::SessionMissing {
                    plot_id: "plot-a".to_owned(),
                    session_id: "session-old".to_owned(),
                },
            }
        );
    }

    #[test]
    fn selection_persistence_rejects_non_core_intent_and_supports_explicit_clear() {
        let scope = scope();
        let snapshot = field(&[("plot-a", Some("session-a"), &["session-a"])]);
        let intended = ExactSessionSelection::new("plot-a", "session-a");
        let application = start_primary_with_scoped_preferences(&scope, snapshot.clone());

        let rejected = application
            .host
            .selection_persistence()
            .select(&ExactSessionSelection::new("invented", "not-a-core-fact"))
            .expect("reject non-Core intent without an I/O error");
        assert!(matches!(
            rejected,
            NativeSelectionWriteOutcome::NotWritten(NativeSelectionNotWrittenReason::NotExact {
                resolution: RestoreResolution::Fallback {
                    reason: RestoreFallbackReason::PlotMissing { .. },
                    ..
                }
            })
        ));
        assert_eq!(
            RestorePreferenceStore::new(application.host.selection_persistence().path())
                .read()
                .expect("inspect rejected preference"),
            RestorePreferenceReadOutcome::Missing
        );

        application
            .host
            .selection_persistence()
            .select(&intended)
            .expect("persist exact Core selection");
        assert_eq!(
            application
                .host
                .selection_persistence()
                .clear()
                .expect("clear selection intent"),
            NativeSelectionWriteOutcome::Written
        );
        drop(application);

        let restarted = start_primary_with_scoped_preferences(&scope, snapshot);
        assert_eq!(
            restarted.restore_resolution(),
            &RestoreResolution::Fallback {
                selection: RestoreSelection::Session(intended),
                reason: RestoreFallbackReason::NoPreviousSelection,
            }
        );
    }

    #[test]
    fn selection_persistence_reports_oversized_exact_intent_as_not_written() {
        let scope = scope();
        let oversized_plot_id = "p".repeat(MAX_RESTORE_PREFERENCE_BYTES + 1_024);
        let snapshot = field(&[(
            oversized_plot_id.as_str(),
            Some("session-a"),
            &["session-a"],
        )]);
        let intended = ExactSessionSelection::new(oversized_plot_id.as_str(), "session-a");
        let application = start_primary_with_scoped_preferences(&scope, snapshot);

        let outcome = application
            .host
            .selection_persistence()
            .select(&intended)
            .expect("oversized exact selection is a typed non-I/O outcome");

        assert!(matches!(
            outcome,
            NativeSelectionWriteOutcome::NotWritten(
                NativeSelectionNotWrittenReason::Oversized {
                    max_bytes: MAX_RESTORE_PREFERENCE_BYTES,
                    encoded_bytes,
                }
            ) if encoded_bytes > MAX_RESTORE_PREFERENCE_BYTES
        ));
        assert_eq!(
            RestorePreferenceStore::new(application.host.selection_persistence().path())
                .read()
                .expect("inspect oversized preference outcome"),
            RestorePreferenceReadOutcome::Missing
        );
    }

    #[test]
    fn selection_and_clear_report_preserved_unreadable_content_as_not_written() {
        let scope = scope();
        let snapshot = field(&[("plot-a", Some("session-a"), &["session-a"])]);
        let intended = ExactSessionSelection::new("plot-a", "session-a");
        let application = start_primary_with_scoped_preferences(&scope, snapshot);
        fs::create_dir_all(
            application
                .host
                .selection_persistence()
                .path()
                .parent()
                .expect("preference path has a parent"),
        )
        .expect("create preference parent");
        fs::create_dir(application.host.selection_persistence().path())
            .expect("preplant unreadable preference path");

        for outcome in [
            application
                .host
                .selection_persistence()
                .select(&intended)
                .expect("select preserves unreadable preference content"),
            application
                .host
                .selection_persistence()
                .clear()
                .expect("clear preserves unreadable preference content"),
        ] {
            assert!(matches!(
                outcome,
                NativeSelectionWriteOutcome::NotWritten(
                    NativeSelectionNotWrittenReason::PreservedUnreadable(
                        PreservedPreference::Unreadable { ref message }
                    )
                ) if message.contains("ownership, permissions, or file type")
            ));
        }
        assert!(application.host.selection_persistence().path().is_dir());
    }

    #[test]
    fn clear_reports_preserved_oversized_content_as_not_written() {
        let scope = scope();
        let snapshot = field(&[("plot-a", Some("session-a"), &["session-a"])]);
        let application = start_primary_with_scoped_preferences(&scope, snapshot);
        let oversized = vec![b'x'; MAX_RESTORE_PREFERENCE_BYTES + 1];
        fs::create_dir_all(
            application
                .host
                .selection_persistence()
                .path()
                .parent()
                .expect("preference path has a parent"),
        )
        .expect("create preference parent");
        fs::write(application.host.selection_persistence().path(), &oversized)
            .expect("preplant oversized preference document");

        let outcome = application
            .host
            .selection_persistence()
            .clear()
            .expect("clear preserves oversized preference content");

        assert!(matches!(
            outcome,
            NativeSelectionWriteOutcome::NotWritten(
                NativeSelectionNotWrittenReason::PreservedUnreadable(
                    PreservedPreference::Oversized {
                        max_bytes: MAX_RESTORE_PREFERENCE_BYTES,
                        observed_bytes: Some(observed_bytes),
                        ..
                    }
                )
            ) if observed_bytes == oversized.len() as u64
        ));
        assert_eq!(
            fs::read(application.host.selection_persistence().path())
                .expect("read preserved oversized preference"),
            oversized
        );
    }

    #[test]
    fn every_store_rejection_maps_to_an_explicit_not_written_reason() {
        assert_eq!(
            map_store_write_outcome(RestorePreferenceWriteOutcome::RejectedUnknownSelection),
            NativeSelectionWriteOutcome::NotWritten(
                NativeSelectionNotWrittenReason::UnknownSelection
            )
        );
        assert_eq!(
            map_store_write_outcome(RestorePreferenceWriteOutcome::RejectedOversized {
                max_bytes: 64,
                encoded_bytes: 65,
            }),
            NativeSelectionWriteOutcome::NotWritten(NativeSelectionNotWrittenReason::Oversized {
                max_bytes: 64,
                encoded_bytes: 65,
            })
        );
        let preserved = PreservedPreference::Malformed;
        assert_eq!(
            map_store_write_outcome(RestorePreferenceWriteOutcome::PreservedUnreadable(
                preserved.clone(),
            )),
            NativeSelectionWriteOutcome::NotWritten(
                NativeSelectionNotWrittenReason::PreservedUnreadable(preserved)
            )
        );
    }

    #[test]
    fn acquisition_and_secondary_failures_report_exact_stages() {
        let acquisition_platform = FakePlatform {
            acquisition: RefCell::new(Some(Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "lock denied",
            )))),
            calls: Cell::new(0),
        };
        let error = start_native_application(
            &scope(),
            &acquisition_platform,
            &mut FakeRecovery::empty(),
            &FakePreferenceSource::returning(RestorePreferenceReadOutcome::Missing),
            &FakeCoreSource::returning(field(&[])),
            &FakeHostFactory::available(),
            Duration::from_millis(50),
        )
        .err()
        .expect("acquisition must fail");
        assert_eq!(
            error.stage(),
            NativeApplicationStartStage::InstanceAcquisition
        );
        assert!(error.message().contains("lock denied"));

        let forwarding_platform = FakePlatform::secondary(
            Arc::new(AtomicUsize::new(0)),
            Err(unavailable("focus refused")),
        );
        let error = start_native_application(
            &scope(),
            &forwarding_platform,
            &mut FakeRecovery::empty(),
            &FakePreferenceSource::returning(RestorePreferenceReadOutcome::Missing),
            &FakeCoreSource::returning(field(&[])),
            &FakeHostFactory::available(),
            Duration::from_millis(50),
        )
        .err()
        .expect("forwarding must fail");
        assert_eq!(
            error.stage(),
            NativeApplicationStartStage::SecondaryActivation
        );
        assert!(error.message().contains("focus refused"));
    }

    #[test]
    fn preference_failures_stop_before_core_or_host_work() {
        let preferences =
            FakePreferenceSource::failing(io::ErrorKind::PermissionDenied, "read denied");
        let core = FakeCoreSource::returning(field(&[]));
        let factory = FakeHostFactory::available();

        let error = start_native_application(
            &scope(),
            &FakePlatform::primary(),
            &mut FakeRecovery::empty(),
            &preferences,
            &core,
            &factory,
            Duration::from_millis(50),
        )
        .err()
        .expect("preference read must fail");

        assert_eq!(
            error.stage(),
            NativeApplicationStartStage::RestorePreference
        );
        assert!(error.message().contains("read denied"));
        assert_eq!(core.calls.get(), 0);
        assert_eq!(factory.calls.get(), 0);
    }

    #[test]
    fn blocked_or_failed_owned_resource_recovery_stops_before_primary_sources() {
        let blocked_cases = [
            PreservedRecoveryJournal::Malformed {
                message: "recovery journal is malformed".to_owned(),
            },
            PreservedRecoveryJournal::FutureVersion {
                version: "nopal.native_owned_resources/v9".to_owned(),
                message: "recovery journal version is unsupported".to_owned(),
            },
            PreservedRecoveryJournal::Oversized {
                max_bytes: 262_144,
                observed_bytes: Some(300_000),
                message: "recovery journal is oversized".to_owned(),
            },
            PreservedRecoveryJournal::Unreadable {
                message: "recovery journal cannot be read safely".to_owned(),
            },
        ];

        for blocked in blocked_cases {
            let expected = blocked.diagnostic().to_owned();
            let mut recovery = FakeRecovery::returning(RecoveryReconcileOutcome::Blocked(blocked));
            let preferences =
                FakePreferenceSource::returning(RestorePreferenceReadOutcome::Missing);
            let core = FakeCoreSource::returning(field(&[]));
            let factory = FakeHostFactory::available();

            let error = start_native_application(
                &scope(),
                &FakePlatform::primary(),
                &mut recovery,
                &preferences,
                &core,
                &factory,
                Duration::from_millis(50),
            )
            .err()
            .expect("unsafe recovery journal must block primary startup");

            assert_eq!(
                error.stage(),
                NativeApplicationStartStage::OwnedResourceRecovery
            );
            assert_eq!(error.message(), expected);
            assert_eq!(recovery.calls.get(), 1);
            assert_eq!(preferences.calls.get(), 0);
            assert_eq!(core.calls.get(), 0);
            assert_eq!(factory.calls.get(), 0);
        }

        let mut recovery =
            FakeRecovery::failing(io::ErrorKind::PermissionDenied, "checkpoint denied");
        let preferences = FakePreferenceSource::returning(RestorePreferenceReadOutcome::Missing);
        let core = FakeCoreSource::returning(field(&[]));
        let factory = FakeHostFactory::available();
        let error = start_native_application(
            &scope(),
            &FakePlatform::primary(),
            &mut recovery,
            &preferences,
            &core,
            &factory,
            Duration::from_millis(50),
        )
        .err()
        .expect("recovery persistence failure must block primary startup");

        assert_eq!(
            error.stage(),
            NativeApplicationStartStage::OwnedResourceRecovery
        );
        assert!(error.message().contains("checkpoint denied"));
        assert_eq!(preferences.calls.get(), 0);
        assert_eq!(core.calls.get(), 0);
        assert_eq!(factory.calls.get(), 0);
    }

    #[test]
    fn scoped_recovery_reconciles_all_entries_before_constructing_the_host() {
        let scope = scope();
        register_filesystem_recovery(&scope, "stale-socket");
        let recovery_calls = Arc::new(AtomicUsize::new(0));
        let mut recovery = ScopedOwnedResourceRecovery::new(ScriptedRecoveryAdapter {
            calls: Arc::clone(&recovery_calls),
            result: Ok(RecoveryDisposition::Recovered),
        });
        let preferences = FakePreferenceSource::returning(RestorePreferenceReadOutcome::Missing);
        let core = FakeCoreSource::returning(field(&[]));
        let factory = FakeHostFactory::available();

        let result = start_native_application(
            &scope,
            &FakePlatform::primary(),
            &mut recovery,
            &preferences,
            &core,
            &factory,
            Duration::from_millis(50),
        )
        .expect("fully reconciled owned resources should permit primary startup");
        let NativeApplicationStart::Primary(application) = result else {
            panic!("primary acquisition must not return secondary");
        };
        let OwnedResourceRecoveryReport::Reconciled(report) = application.recovery_report() else {
            panic!("recovered entry must produce a reconciliation report");
        };

        assert_eq!(report.attempts().len(), 1);
        assert_eq!(report.remaining_entries(), 0);
        assert_eq!(recovery_calls.load(Ordering::SeqCst), 1);
        assert_eq!(preferences.calls.get(), 1);
        assert_eq!(core.calls.get(), 1);
        assert_eq!(factory.calls.get(), 1);
        assert!(
            !ScopedOwnedResourceRecovery::<ScriptedRecoveryAdapter>::journal_path(&scope).exists()
        );
        let received = factory
            .received
            .lock()
            .expect("recording lock should remain available");
        assert!(matches!(
            received[0].2,
            OwnedResourceRecoveryReport::Reconciled(_)
        ));
    }

    #[test]
    fn retained_identity_mismatch_blocks_before_core_and_host_construction() {
        let scope = scope();
        register_filesystem_recovery(&scope, "replaced-socket");
        let mut recovery = ScopedOwnedResourceRecovery::new(ScriptedRecoveryAdapter {
            calls: Arc::new(AtomicUsize::new(0)),
            result: Ok(RecoveryDisposition::IdentityMismatch {
                observed_identity: Some("unix.dev_inode:9:9".to_owned()),
            }),
        });
        let preferences = FakePreferenceSource::returning(RestorePreferenceReadOutcome::Missing);
        let core = FakeCoreSource::returning(field(&[]));
        let factory = FakeHostFactory::available();

        let error = start_native_application(
            &scope,
            &FakePlatform::primary(),
            &mut recovery,
            &preferences,
            &core,
            &factory,
            Duration::from_millis(50),
        )
        .err()
        .expect("identity mismatch must retain the entry and block startup");

        assert_eq!(
            error.stage(),
            NativeApplicationStartStage::OwnedResourceRecovery
        );
        assert!(
            error
                .message()
                .contains("1 application-owned recovery entry")
        );
        assert_eq!(preferences.calls.get(), 0);
        assert_eq!(core.calls.get(), 0);
        assert_eq!(factory.calls.get(), 0);
        assert!(
            ScopedOwnedResourceRecovery::<ScriptedRecoveryAdapter>::journal_path(&scope).exists()
        );
    }

    struct DropOrderLease {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl Drop for DropOrderLease {
        fn drop(&mut self) {
            self.events
                .lock()
                .expect("drop-order recording lock should remain available")
                .push("lease");
        }
    }

    struct DropOrderHost {
        events: Arc<Mutex<Vec<&'static str>>>,
    }

    impl NativeApplicationHost for DropOrderHost {
        fn activate(
            &mut self,
            _deadline: ActivationDeadline,
        ) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
            Ok(NativeApplicationAck::Focused)
        }
    }

    impl Drop for DropOrderHost {
        fn drop(&mut self) {
            self.events
                .lock()
                .expect("drop-order recording lock should remain available")
                .push("host");
        }
    }

    #[test]
    fn clean_application_drop_destroys_host_before_releasing_singleton_lease() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let application = super::NativePrimaryApplication {
            lease: DropOrderLease {
                events: Arc::clone(&events),
            },
            field: field(&[]),
            restore: RestoreResolution::Unavailable {
                reason: RestoreFallbackReason::NoPlotsAvailable,
            },
            recovery_report: OwnedResourceRecoveryReport::Empty,
            preference_notice: None,
            host: DropOrderHost {
                events: Arc::clone(&events),
            },
        };

        drop(application);

        assert_eq!(
            events
                .lock()
                .expect("drop-order recording lock should remain available")
                .as_slice(),
            &["host", "lease"]
        );
    }

    #[test]
    fn unsafe_preference_content_falls_back_and_reaches_host_with_typed_notice() {
        let cases = [
            (
                RestorePreferenceReadOutcome::Malformed {
                    message: "invalid selection shape".to_owned(),
                },
                RestorePreferenceNotice::Malformed {
                    message: "invalid selection shape".to_owned(),
                },
            ),
            (
                RestorePreferenceReadOutcome::FutureVersion {
                    version: "nopal.native_field_preference/v9".to_owned(),
                },
                RestorePreferenceNotice::FutureVersion {
                    version: "nopal.native_field_preference/v9".to_owned(),
                },
            ),
            (
                RestorePreferenceReadOutcome::Oversized {
                    max_bytes: 65_536,
                    observed_bytes: Some(70_000),
                    message: "preference is oversized".to_owned(),
                },
                RestorePreferenceNotice::Oversized {
                    max_bytes: 65_536,
                    observed_bytes: Some(70_000),
                    message: "preference is oversized".to_owned(),
                },
            ),
            (
                RestorePreferenceReadOutcome::Unreadable {
                    message: "preference permissions are unsafe".to_owned(),
                },
                RestorePreferenceNotice::Unreadable {
                    message: "preference permissions are unsafe".to_owned(),
                },
            ),
        ];

        for (outcome, expected_notice) in cases {
            let preferences = FakePreferenceSource::returning(outcome);
            let core = FakeCoreSource::returning(field(&[(
                "plot-first",
                Some("session-selected"),
                &["session-first", "session-selected"],
            )]));
            let factory = FakeHostFactory::available();

            let result = start_native_application(
                &scope(),
                &FakePlatform::primary(),
                &mut FakeRecovery::empty(),
                &preferences,
                &core,
                &factory,
                Duration::from_millis(50),
            )
            .expect("unsafe intent should not prevent deterministic fallback");
            let NativeApplicationStart::Primary(application) = result else {
                panic!("primary acquisition must not return secondary");
            };

            assert_eq!(
                application.restore_resolution(),
                &RestoreResolution::Fallback {
                    selection: RestoreSelection::Session(ExactSessionSelection::new(
                        "plot-first",
                        "session-selected",
                    )),
                    reason: RestoreFallbackReason::NoPreviousSelection,
                }
            );
            assert_eq!(application.preference_notice(), Some(&expected_notice));
            assert_eq!(core.calls.get(), 1);
            assert_eq!(factory.calls.get(), 1);
            let received = factory
                .received
                .lock()
                .expect("recording lock should remain available");
            assert_eq!(received.len(), 1);
            assert_eq!(received[0].3.as_ref(), Some(&expected_notice));
        }
    }

    #[test]
    fn core_and_host_failures_report_distinct_stages() {
        let core = FakeCoreSource::failing("Core unavailable");
        let factory = FakeHostFactory::available();
        let error = start_native_application(
            &scope(),
            &FakePlatform::primary(),
            &mut FakeRecovery::empty(),
            &FakePreferenceSource::returning(RestorePreferenceReadOutcome::Missing),
            &core,
            &factory,
            Duration::from_millis(50),
        )
        .err()
        .expect("Core loading must fail");
        assert_eq!(error.stage(), NativeApplicationStartStage::CoreSnapshot);
        assert!(error.message().contains("Core unavailable"));
        assert_eq!(factory.calls.get(), 0);

        let factory = FakeHostFactory::failing("renderer unavailable");
        let error = start_native_application(
            &scope(),
            &FakePlatform::primary(),
            &mut FakeRecovery::empty(),
            &FakePreferenceSource::returning(RestorePreferenceReadOutcome::Missing),
            &FakeCoreSource::returning(field(&[])),
            &factory,
            Duration::from_millis(50),
        )
        .err()
        .expect("host construction must fail");
        assert_eq!(error.stage(), NativeApplicationStartStage::HostConstruction);
        assert!(error.message().contains("renderer unavailable"));
        assert_eq!(factory.calls.get(), 1);
    }

    fn scope() -> NativeInstanceScope {
        let sandbox = tempfile::tempdir().expect("create scope sandbox");
        NativeInstanceScope::new(
            CanonicalStateRoot::create(sandbox.keep()).expect("create canonical state root"),
            ReleaseChannel::Development,
        )
    }

    fn start_primary_with_scoped_preferences(
        scope: &NativeInstanceScope,
        snapshot: FieldSnapshot,
    ) -> Box<super::NativePrimaryApplication<FakeLease, FakeHost>> {
        let result = start_native_application(
            scope,
            &FakePlatform::primary(),
            &mut FakeRecovery::empty(),
            &ScopedRestorePreferenceSource,
            &FakeCoreSource::returning(snapshot),
            &FakeHostFactory::available(),
            Duration::from_millis(50),
        )
        .expect("primary application should compose");
        let NativeApplicationStart::Primary(application) = result else {
            panic!("primary acquisition must not return secondary");
        };
        application
    }

    fn register_filesystem_recovery(scope: &NativeInstanceScope, entry_id: &str) {
        let identity = DurableIdentity::new("unix.dev_inode", "1:2")
            .expect("create durable filesystem identity");
        let recipe = FilesystemRecoveryRecipe::new(
            scope.state_paths().state_directory().join("stale.sock"),
            identity,
        )
        .expect("create durable filesystem recipe");
        let entry = DurableRecoveryEntry::new(
            entry_id,
            "stale native socket",
            ResourceOwnership::ApplicationOwned,
            DurableRecoveryRecipe::Filesystem(recipe),
        )
        .expect("create application-owned recovery entry");
        let store = RecoveryJournalStore::new(
            ScopedOwnedResourceRecovery::<ScriptedRecoveryAdapter>::journal_path(scope),
        );
        assert_eq!(
            store.register(entry).expect("register recovery entry"),
            RecoveryJournalUpdateOutcome::Written { entry_count: 1 }
        );
    }

    fn unavailable(message: &'static str) -> NativeApplicationUnavailable {
        NativeApplicationUnavailable::new(message)
    }

    fn activation_deadline() -> ActivationDeadline {
        ActivationDeadline::after(Duration::from_secs(1))
            .expect("test activation deadline should be valid")
    }

    fn field(plots: &[(&str, Option<&str>, &[&str])]) -> FieldSnapshot {
        serde_json::from_value(serde_json::json!({
            "kind": "nopal.field/v1",
            "plots": plots
                .iter()
                .map(|(plot_id, selected_session_id, sessions)| serde_json::json!({
                    "kind": "nopal.plot/v1",
                    "plot_id": plot_id,
                    "selected_session_id": selected_session_id,
                    "sessions": sessions
                        .iter()
                        .map(|session_id| serde_json::json!({ "session_id": session_id }))
                        .collect::<Vec<_>>(),
                }))
                .collect::<Vec<_>>(),
            "entries": [],
        }))
        .expect("fixture should satisfy the Field contract")
    }
}
