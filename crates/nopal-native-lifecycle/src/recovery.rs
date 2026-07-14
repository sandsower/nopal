//! Durable recovery of application-owned native Field resources.
//!
//! The journal contains only cleanup recipes that remain meaningful after the
//! application process exits. Threads and pipes are intentionally excluded:
//! they cannot be safely rediscovered or controlled after a restart. Process
//! recipes require both a birth identity and an executable identity, so a PID
//! is never sufficient authority for cleanup.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use fs2::FileExt;
use serde::{Deserialize, Serialize};

use crate::resources::ResourceOwnership;

const JOURNAL_KIND: &str = "nopal.native_owned_resources/v1";
const TEMP_CREATE_ATTEMPTS: usize = 32;
const MAX_RECOVERY_ENTRIES: usize = 1_024;
const MAX_ID_BYTES: usize = 128;
const MAX_LABEL_BYTES: usize = 512;
const MAX_IDENTITY_BYTES: usize = 1_024;
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(5);

/// Default overall budget for one recovery transaction or reconciliation pass.
pub const DEFAULT_RECOVERY_TIMEOUT: Duration = Duration::from_secs(30);

/// Maximum accepted size of a native owned-resource recovery journal.
pub const MAX_RECOVERY_JOURNAL_BYTES: usize = 256 * 1024;

/// One shared monotonic deadline for a complete recovery operation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RecoveryDeadline {
    started_at: Instant,
    timeout: Duration,
}

impl RecoveryDeadline {
    /// Starts a deadline with the supplied complete-operation budget.
    pub fn from_timeout(timeout: Duration) -> Self {
        Self {
            started_at: Instant::now(),
            timeout,
        }
    }

    pub(crate) fn from_started_at(started_at: Instant, timeout: Duration) -> Self {
        Self {
            started_at,
            timeout,
        }
    }

    /// Returns the total budget for the operation.
    pub fn timeout(self) -> Duration {
        self.timeout
    }

    /// Returns the time available to all remaining adapter and persistence work.
    pub fn remaining(self) -> Duration {
        self.timeout.saturating_sub(self.started_at.elapsed())
    }

    /// Returns whether the complete-operation budget has been consumed.
    pub fn is_expired(self) -> bool {
        self.started_at.elapsed() >= self.timeout
    }

    fn check(self, operation: &str) -> io::Result<()> {
        if self.is_expired() {
            Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("recovery deadline expired while {operation}"),
            ))
        } else {
            Ok(())
        }
    }
}

/// An exact, namespaced identity that must be revalidated before cleanup.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DurableIdentity {
    namespace: String,
    value: String,
}

impl DurableIdentity {
    /// Creates a non-empty identity supplied by the platform adapter.
    pub fn new(
        namespace: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self, RecoveryEntryError> {
        let identity = Self {
            namespace: namespace.into(),
            value: value.into(),
        };
        validate_text(
            "identity namespace",
            &identity.namespace,
            MAX_IDENTITY_BYTES,
        )?;
        validate_text("identity value", &identity.value, MAX_IDENTITY_BYTES)?;
        Ok(identity)
    }

    /// Returns the adapter-defined identity namespace.
    pub fn namespace(&self) -> &str {
        &self.namespace
    }

    /// Returns the exact identity value recorded at acquisition time.
    pub fn value(&self) -> &str {
        &self.value
    }
}

/// A filesystem object that may be removed only if its identity is unchanged.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct FilesystemRecoveryRecipe {
    path: PathBuf,
    identity: DurableIdentity,
}

impl FilesystemRecoveryRecipe {
    /// Creates an exact filesystem cleanup recipe.
    pub fn new(
        path: impl Into<PathBuf>,
        identity: DurableIdentity,
    ) -> Result<Self, RecoveryEntryError> {
        let path = path.into();
        if !path.is_absolute() {
            return Err(RecoveryEntryError::new(
                RecoveryEntryErrorKind::InvalidPath,
                "durable recovery paths must be absolute",
            ));
        }
        if path.as_os_str().is_empty() {
            return Err(RecoveryEntryError::new(
                RecoveryEntryErrorKind::InvalidPath,
                "durable recovery paths must not be empty",
            ));
        }
        Ok(Self { path, identity })
    }

    /// Returns the exact path recorded at acquisition time.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Returns the identity that must still name the path before removal.
    pub fn identity(&self) -> &DurableIdentity {
        &self.identity
    }
}

/// A process that may be stopped only after exact identity revalidation.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct VerifiedProcessRecoveryRecipe {
    pid: NonZeroU32,
    birth_identity: DurableIdentity,
    executable_identity: DurableIdentity,
}

impl VerifiedProcessRecoveryRecipe {
    /// Creates a process recipe with the identities required to reject PID reuse.
    pub fn new(
        pid: NonZeroU32,
        birth_identity: DurableIdentity,
        executable_identity: DurableIdentity,
    ) -> Self {
        Self {
            pid,
            birth_identity,
            executable_identity,
        }
    }

    /// Returns the observed PID, which is never sufficient cleanup authority.
    pub fn pid(&self) -> NonZeroU32 {
        self.pid
    }

    /// Returns the process birth identity recorded at acquisition time.
    pub fn birth_identity(&self) -> &DurableIdentity {
        &self.birth_identity
    }

    /// Returns the executable identity recorded at acquisition time.
    pub fn executable_identity(&self) -> &DurableIdentity {
        &self.executable_identity
    }
}

/// One durable cleanup operation supported after application restart.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DurableRecoveryRecipe {
    /// Remove an exact filesystem object.
    Filesystem(FilesystemRecoveryRecipe),
    /// Stop an exact process after rejecting PID reuse.
    VerifiedProcess(VerifiedProcessRecoveryRecipe),
}

/// One application-owned resource registered for recovery after a crash.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct DurableRecoveryEntry {
    id: String,
    label: String,
    recipe: DurableRecoveryRecipe,
}

impl DurableRecoveryEntry {
    /// Creates a durable entry only for application-owned resources.
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        ownership: ResourceOwnership,
        recipe: DurableRecoveryRecipe,
    ) -> Result<Self, RecoveryEntryError> {
        if ownership != ResourceOwnership::ApplicationOwned {
            return Err(RecoveryEntryError::new(
                RecoveryEntryErrorKind::BorrowedResource,
                "borrowed resources must never be written to the crash-recovery journal",
            ));
        }
        let entry = Self {
            id: id.into(),
            label: label.into(),
            recipe,
        };
        entry.validate()?;
        Ok(entry)
    }

    /// Returns the stable registration identity.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Returns the human-readable diagnostic label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the exact cleanup recipe.
    pub fn recipe(&self) -> &DurableRecoveryRecipe {
        &self.recipe
    }

    fn validate(&self) -> Result<(), RecoveryEntryError> {
        validate_text("entry id", &self.id, MAX_ID_BYTES)?;
        validate_text("entry label", &self.label, MAX_LABEL_BYTES)?;
        match &self.recipe {
            DurableRecoveryRecipe::Filesystem(recipe) => {
                if !recipe.path.is_absolute() || recipe.path.as_os_str().is_empty() {
                    return Err(RecoveryEntryError::new(
                        RecoveryEntryErrorKind::InvalidPath,
                        "durable recovery paths must be absolute and non-empty",
                    ));
                }
                validate_identity(&recipe.identity)?;
            }
            DurableRecoveryRecipe::VerifiedProcess(recipe) => {
                validate_identity(&recipe.birth_identity)?;
                validate_identity(&recipe.executable_identity)?;
            }
        }
        Ok(())
    }
}

fn validate_identity(identity: &DurableIdentity) -> Result<(), RecoveryEntryError> {
    validate_text(
        "identity namespace",
        &identity.namespace,
        MAX_IDENTITY_BYTES,
    )?;
    validate_text("identity value", &identity.value, MAX_IDENTITY_BYTES)
}

fn validate_text(
    field: &'static str,
    value: &str,
    max_bytes: usize,
) -> Result<(), RecoveryEntryError> {
    if value.trim().is_empty() {
        return Err(RecoveryEntryError::new(
            RecoveryEntryErrorKind::InvalidIdentity,
            format!("{field} must not be empty"),
        ));
    }
    if value.len() > max_bytes {
        return Err(RecoveryEntryError::new(
            RecoveryEntryErrorKind::InvalidIdentity,
            format!("{field} must not exceed {max_bytes} bytes"),
        ));
    }
    Ok(())
}

/// Machine-readable category for rejected journal entries.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryEntryErrorKind {
    /// A borrowed resource was offered for durable recovery.
    BorrowedResource,
    /// An identifier or identity marker was empty or too large.
    InvalidIdentity,
    /// A filesystem recipe did not contain an absolute path.
    InvalidPath,
}

/// A typed invalid-entry diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryEntryError {
    kind: RecoveryEntryErrorKind,
    message: String,
}

impl RecoveryEntryError {
    fn new(kind: RecoveryEntryErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    /// Returns the machine-readable rejection category.
    pub fn kind(&self) -> RecoveryEntryErrorKind {
        self.kind
    }

    /// Returns an actionable rejection diagnostic.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RecoveryEntryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RecoveryEntryError {}

/// A decoded exact v1 recovery journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryJournal {
    entries: Vec<DurableRecoveryEntry>,
}

impl RecoveryJournal {
    /// Returns entries in their stable registration order.
    pub fn entries(&self) -> &[DurableRecoveryEntry] {
        &self.entries
    }
}

/// A typed bounded read of the durable recovery journal.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryJournalReadOutcome {
    /// No journal has been written.
    Missing,
    /// An exact v1 journal was decoded and validated.
    Ready(RecoveryJournal),
    /// Existing content was not valid exact v1 JSON.
    Malformed {
        /// An actionable diagnostic. The source file remains unchanged.
        message: String,
    },
    /// Existing content names a version this build does not understand.
    FutureVersion {
        /// The exact unsupported version string.
        version: String,
        /// An actionable diagnostic. The source file remains unchanged.
        message: String,
    },
    /// Existing content exceeded the bounded wire size.
    Oversized {
        /// The maximum accepted document size.
        max_bytes: usize,
        /// The size reported by metadata or the bounded prefix read.
        observed_bytes: Option<u64>,
        /// An actionable diagnostic. The source file remains unchanged.
        message: String,
    },
    /// Existing content could not be opened, inspected, or read.
    Unreadable {
        /// An actionable diagnostic. The source file remains unchanged.
        message: String,
    },
}

/// Existing content intentionally preserved instead of overwritten.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PreservedRecoveryJournal {
    /// Invalid JSON or invalid exact v1 data.
    Malformed { message: String },
    /// A version this build cannot interpret.
    FutureVersion { version: String, message: String },
    /// Content beyond the bounded wire size.
    Oversized {
        max_bytes: usize,
        observed_bytes: Option<u64>,
        message: String,
    },
    /// Content that could not be safely read.
    Unreadable { message: String },
}

impl PreservedRecoveryJournal {
    /// Returns the diagnostic explaining how to unblock recovery.
    pub fn diagnostic(&self) -> &str {
        match self {
            Self::Malformed { message }
            | Self::FutureVersion { message, .. }
            | Self::Oversized { message, .. }
            | Self::Unreadable { message } => message,
        }
    }
}

/// The result of registering or retiring one durable entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryJournalUpdateOutcome {
    /// A complete replacement journal was installed.
    Written { entry_count: usize },
    /// The named entry did not exist, so no file changed.
    EntryMissing,
    /// Existing unsafe or unsupported content was preserved byte-for-byte.
    Preserved(PreservedRecoveryJournal),
    /// The entry could not be registered without exceeding the entry bound.
    CapacityExceeded { max_entries: usize },
    /// Encoding exceeded the bounded journal wire size.
    EncodedJournalOversized {
        max_bytes: usize,
        encoded_bytes: usize,
    },
}

/// An adapter result that proves cleanup did not cross an identity boundary.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryDisposition {
    /// The exact resource was recovered during this attempt.
    Recovered,
    /// The exact resource was already absent, so cleanup is idempotently done.
    AlreadyAbsent,
    /// Something exists under the address but its identity differs.
    IdentityMismatch {
        /// The observed identity, when it can be safely described.
        observed_identity: Option<String>,
    },
}

/// An adapter failure that should be retained for a later startup attempt.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryAdapterError {
    kind: RecoveryAdapterErrorKind,
    message: String,
}

/// Machine-readable reason an exact recovery attempt could not complete.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RecoveryAdapterErrorKind {
    /// The adapter encountered a platform cleanup failure.
    Failure,
    /// The adapter cooperatively stopped at the shared recovery deadline.
    DeadlineExceeded,
}

impl RecoveryAdapterError {
    /// Creates an actionable adapter failure.
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            kind: RecoveryAdapterErrorKind::Failure,
            message: message.into(),
        }
    }

    /// Creates a typed cooperative timeout diagnostic.
    pub fn deadline_exceeded(message: impl Into<String>) -> Self {
        Self {
            kind: RecoveryAdapterErrorKind::DeadlineExceeded,
            message: message.into(),
        }
    }

    /// Returns the machine-readable failure category.
    pub fn kind(&self) -> RecoveryAdapterErrorKind {
        self.kind
    }

    /// Returns the diagnostic supplied by the platform adapter.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for RecoveryAdapterError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for RecoveryAdapterError {}

/// Platform cleanup with mandatory exact-identity semantics.
///
/// Each implementation must observe the current resource identity and compare
/// every identity field immediately before cleanup. It must return
/// [`RecoveryDisposition::IdentityMismatch`] without changing the resource when
/// any value differs. Process adapters must compare both birth and executable
/// identities before signaling a PID. Filesystem adapters must avoid following
/// a replacement symbolic link and close the check/remove race using the
/// strongest primitives available on the target platform.
pub trait ExactRecoveryAdapter {
    /// Recovers one exact filesystem resource or rejects an identity mismatch.
    fn recover_filesystem_exact(
        &mut self,
        recipe: &FilesystemRecoveryRecipe,
        deadline: RecoveryDeadline,
    ) -> Result<RecoveryDisposition, RecoveryAdapterError>;

    /// Recovers one exact process or rejects PID reuse and identity mismatch.
    fn recover_process_exact(
        &mut self,
        recipe: &VerifiedProcessRecoveryRecipe,
        deadline: RecoveryDeadline,
    ) -> Result<RecoveryDisposition, RecoveryAdapterError>;
}

/// Machine-readable result for one attempted durable entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryAttemptDisposition {
    /// Cleanup completed now.
    Recovered,
    /// Cleanup had already completed before this pass.
    AlreadyAbsent,
    /// Cleanup was rejected because the current identity did not match.
    RetainedIdentityMismatch { observed_identity: Option<String> },
    /// Cleanup failed and remains registered for the next startup.
    RetainedFailure { message: String },
    /// Cleanup cooperatively stopped at the shared pass deadline.
    RetainedDeadlineExceeded { message: String },
}

/// One stable diagnostic emitted by startup reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryAttempt {
    entry_id: String,
    label: String,
    disposition: RecoveryAttemptDisposition,
}

impl RecoveryAttempt {
    /// Returns the durable entry identity.
    pub fn entry_id(&self) -> &str {
        &self.entry_id
    }

    /// Returns the human-readable resource label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the typed result of this attempt.
    pub fn disposition(&self) -> &RecoveryAttemptDisposition {
        &self.disposition
    }
}

/// Complete evidence from one best-effort startup reconciliation pass.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryPassReport {
    attempts: Vec<RecoveryAttempt>,
    remaining_entries: usize,
}

impl RecoveryPassReport {
    /// Returns attempts in the original stable journal order.
    pub fn attempts(&self) -> &[RecoveryAttempt] {
        &self.attempts
    }

    /// Returns entries durably retained for another startup.
    pub fn remaining_entries(&self) -> usize {
        self.remaining_entries
    }
}

/// Top-level result of startup reconciliation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum RecoveryReconcileOutcome {
    /// No prior owned resources required recovery.
    Empty,
    /// Every entry was attempted and the durable journal was updated.
    Completed(RecoveryPassReport),
    /// Unsafe or unsupported content blocked cleanup and was preserved.
    Blocked(PreservedRecoveryJournal),
}

/// Recovery could not inspect the journal or persist a completed cleanup pass.
#[derive(Debug)]
pub struct RecoveryPersistenceFailure {
    report: Option<RecoveryPassReport>,
    error: io::Error,
}

impl RecoveryPersistenceFailure {
    /// Returns evidence for every cleanup already attempted, when known.
    ///
    /// `None` means reconciliation failed before the journal could be read, so
    /// neither the attempted resources nor the retained entry count is known.
    pub fn report(&self) -> Option<&RecoveryPassReport> {
        self.report.as_ref()
    }

    /// Returns the storage error that prevented an atomic checkpoint.
    pub fn error(&self) -> &io::Error {
        &self.error
    }
}

impl fmt::Display for RecoveryPersistenceFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.report {
            Some(report) => write!(
                formatter,
                "recovery attempted {} resource(s), but could not checkpoint {} retained entry or entries: {}",
                report.attempts.len(),
                report.remaining_entries,
                self.error
            ),
            None => write!(
                formatter,
                "recovery could not inspect the journal before attempting resources; retained entry count is unknown: {}",
                self.error
            ),
        }
    }
}

impl std::error::Error for RecoveryPersistenceFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

/// Crash-safe storage and startup reconciliation for owned resources.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecoveryJournalStore {
    path: PathBuf,
}

impl RecoveryJournalStore {
    /// Targets one journal file under the native state directory.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Returns the journal path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads and validates a bounded journal without modifying unsafe content.
    pub fn read(&self) -> RecoveryJournalReadOutcome {
        self.read_unlocked()
    }

    fn read_unlocked(&self) -> RecoveryJournalReadOutcome {
        let mut file = match open_journal_for_read(&self.path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return RecoveryJournalReadOutcome::Missing;
            }
            Err(error) => return self.unreadable("open", &error),
        };
        let metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(error) => return self.unreadable("inspect", &error),
        };
        if metadata.len() > MAX_RECOVERY_JOURNAL_BYTES as u64 {
            return self.oversized(Some(metadata.len()));
        }
        let mut bytes = Vec::with_capacity(
            usize::try_from(metadata.len())
                .unwrap_or(MAX_RECOVERY_JOURNAL_BYTES + 1)
                .min(MAX_RECOVERY_JOURNAL_BYTES + 1),
        );
        if let Err(error) = Read::by_ref(&mut file)
            .take((MAX_RECOVERY_JOURNAL_BYTES + 1) as u64)
            .read_to_end(&mut bytes)
        {
            return self.unreadable("read", &error);
        }
        if bytes.len() > MAX_RECOVERY_JOURNAL_BYTES {
            return self.oversized(Some(bytes.len() as u64));
        }
        parse_journal(&bytes, &self.path)
    }

    /// Atomically registers or replaces one application-owned recovery entry.
    pub fn register(
        &self,
        entry: DurableRecoveryEntry,
    ) -> io::Result<RecoveryJournalUpdateOutcome> {
        self.register_with_deadline(
            entry,
            RecoveryDeadline::from_timeout(DEFAULT_RECOVERY_TIMEOUT),
        )
    }

    /// Registers an entry within one shared transaction deadline.
    pub fn register_with_deadline(
        &self,
        entry: DurableRecoveryEntry,
        deadline: RecoveryDeadline,
    ) -> io::Result<RecoveryJournalUpdateOutcome> {
        let _lock = self.acquire_transaction_lock(deadline)?;
        self.register_locked(entry, deadline)
    }

    fn register_locked(
        &self,
        entry: DurableRecoveryEntry,
        deadline: RecoveryDeadline,
    ) -> io::Result<RecoveryJournalUpdateOutcome> {
        let mut entries = match self.entries_for_update() {
            Ok(entries) => entries,
            Err(preserved) => return Ok(RecoveryJournalUpdateOutcome::Preserved(preserved)),
        };
        if let Some(existing) = entries.iter_mut().find(|existing| existing.id == entry.id) {
            *existing = entry;
        } else {
            if entries.len() >= MAX_RECOVERY_ENTRIES {
                return Ok(RecoveryJournalUpdateOutcome::CapacityExceeded {
                    max_entries: MAX_RECOVERY_ENTRIES,
                });
            }
            entries.push(entry);
        }
        self.write_entries(&entries, deadline)
    }

    /// Atomically retires one entry after ordinary owned-resource shutdown.
    pub fn retire(&self, entry_id: &str) -> io::Result<RecoveryJournalUpdateOutcome> {
        self.retire_with_deadline(
            entry_id,
            RecoveryDeadline::from_timeout(DEFAULT_RECOVERY_TIMEOUT),
        )
    }

    /// Retires an entry within one shared transaction deadline.
    pub fn retire_with_deadline(
        &self,
        entry_id: &str,
        deadline: RecoveryDeadline,
    ) -> io::Result<RecoveryJournalUpdateOutcome> {
        let _lock = self.acquire_transaction_lock(deadline)?;
        self.retire_locked(entry_id, deadline)
    }

    fn retire_locked(
        &self,
        entry_id: &str,
        deadline: RecoveryDeadline,
    ) -> io::Result<RecoveryJournalUpdateOutcome> {
        let mut entries = match self.entries_for_update() {
            Ok(entries) => entries,
            Err(preserved) => return Ok(RecoveryJournalUpdateOutcome::Preserved(preserved)),
        };
        let original_len = entries.len();
        entries.retain(|entry| entry.id != entry_id);
        if entries.len() == original_len {
            return Ok(RecoveryJournalUpdateOutcome::EntryMissing);
        }
        if entries.is_empty() {
            self.remove_journal(deadline)?;
            return Ok(RecoveryJournalUpdateOutcome::Written { entry_count: 0 });
        }
        self.write_entries(&entries, deadline)
    }

    /// Attempts every prior owned resource and checkpoints only unresolved work.
    pub fn reconcile<A: ExactRecoveryAdapter>(
        &self,
        adapter: &mut A,
    ) -> Result<RecoveryReconcileOutcome, RecoveryPersistenceFailure> {
        self.reconcile_with_timeout(adapter, DEFAULT_RECOVERY_TIMEOUT)
    }

    /// Reconciles every entry under one shared adapter and persistence budget.
    pub fn reconcile_with_timeout<A: ExactRecoveryAdapter>(
        &self,
        adapter: &mut A,
        timeout: Duration,
    ) -> Result<RecoveryReconcileOutcome, RecoveryPersistenceFailure> {
        let deadline = RecoveryDeadline::from_timeout(timeout);
        self.reconcile_with_before_replace(adapter, deadline, |_| Ok(()))
    }

    fn reconcile_with_before_replace<A, F>(
        &self,
        adapter: &mut A,
        deadline: RecoveryDeadline,
        before_replace: F,
    ) -> Result<RecoveryReconcileOutcome, RecoveryPersistenceFailure>
    where
        A: ExactRecoveryAdapter,
        F: FnOnce(&Path) -> io::Result<()>,
    {
        let _lock = self.acquire_transaction_lock(deadline).map_err(|error| {
            RecoveryPersistenceFailure {
                report: None,
                error,
            }
        })?;
        let entries = match self.read_unlocked() {
            RecoveryJournalReadOutcome::Missing => return Ok(RecoveryReconcileOutcome::Empty),
            RecoveryJournalReadOutcome::Ready(journal) if journal.entries.is_empty() => {
                if let Err(error) = self.remove_journal(deadline) {
                    return Err(RecoveryPersistenceFailure {
                        report: Some(RecoveryPassReport {
                            attempts: Vec::new(),
                            remaining_entries: 0,
                        }),
                        error,
                    });
                }
                return Ok(RecoveryReconcileOutcome::Empty);
            }
            RecoveryJournalReadOutcome::Ready(journal) => journal.entries,
            RecoveryJournalReadOutcome::Malformed { message } => {
                return Ok(RecoveryReconcileOutcome::Blocked(
                    PreservedRecoveryJournal::Malformed { message },
                ));
            }
            RecoveryJournalReadOutcome::FutureVersion { version, message } => {
                return Ok(RecoveryReconcileOutcome::Blocked(
                    PreservedRecoveryJournal::FutureVersion { version, message },
                ));
            }
            RecoveryJournalReadOutcome::Oversized {
                max_bytes,
                observed_bytes,
                message,
            } => {
                return Ok(RecoveryReconcileOutcome::Blocked(
                    PreservedRecoveryJournal::Oversized {
                        max_bytes,
                        observed_bytes,
                        message,
                    },
                ));
            }
            RecoveryJournalReadOutcome::Unreadable { message } => {
                return Ok(RecoveryReconcileOutcome::Blocked(
                    PreservedRecoveryJournal::Unreadable { message },
                ));
            }
        };

        let mut attempts = Vec::with_capacity(entries.len());
        let mut retained = Vec::new();
        for entry in entries {
            let result = if deadline.is_expired() {
                Err(RecoveryAdapterError::deadline_exceeded(
                    "shared recovery deadline expired before this entry could be attempted",
                ))
            } else {
                match &entry.recipe {
                    DurableRecoveryRecipe::Filesystem(recipe) => {
                        adapter.recover_filesystem_exact(recipe, deadline)
                    }
                    DurableRecoveryRecipe::VerifiedProcess(recipe) => {
                        adapter.recover_process_exact(recipe, deadline)
                    }
                }
            };
            let disposition = match result {
                Ok(RecoveryDisposition::Recovered) => RecoveryAttemptDisposition::Recovered,
                Ok(RecoveryDisposition::AlreadyAbsent) => RecoveryAttemptDisposition::AlreadyAbsent,
                Ok(RecoveryDisposition::IdentityMismatch { observed_identity }) => {
                    retained.push(entry.clone());
                    RecoveryAttemptDisposition::RetainedIdentityMismatch { observed_identity }
                }
                Err(error) if error.kind == RecoveryAdapterErrorKind::DeadlineExceeded => {
                    retained.push(entry.clone());
                    RecoveryAttemptDisposition::RetainedDeadlineExceeded {
                        message: error.message,
                    }
                }
                Err(error) => {
                    retained.push(entry.clone());
                    RecoveryAttemptDisposition::RetainedFailure {
                        message: error.message,
                    }
                }
            };
            attempts.push(RecoveryAttempt {
                entry_id: entry.id,
                label: entry.label,
                disposition,
            });
        }

        let report = RecoveryPassReport {
            attempts,
            remaining_entries: retained.len(),
        };
        if let Err(error) = deadline.check("checkpointing recovery results") {
            return Err(RecoveryPersistenceFailure {
                report: Some(report),
                error,
            });
        }
        let persist_result = if retained.is_empty() {
            self.remove_journal(deadline)
        } else {
            self.write_entries_with_before_replace(&retained, deadline, before_replace)
                .and_then(update_result_to_io)
        };
        if let Err(error) = persist_result {
            return Err(RecoveryPersistenceFailure {
                report: Some(report),
                error,
            });
        }
        Ok(RecoveryReconcileOutcome::Completed(report))
    }

    fn entries_for_update(&self) -> Result<Vec<DurableRecoveryEntry>, PreservedRecoveryJournal> {
        match self.read_unlocked() {
            RecoveryJournalReadOutcome::Missing => Ok(Vec::new()),
            RecoveryJournalReadOutcome::Ready(journal) => Ok(journal.entries),
            RecoveryJournalReadOutcome::Malformed { message } => {
                Err(PreservedRecoveryJournal::Malformed { message })
            }
            RecoveryJournalReadOutcome::FutureVersion { version, message } => {
                Err(PreservedRecoveryJournal::FutureVersion { version, message })
            }
            RecoveryJournalReadOutcome::Oversized {
                max_bytes,
                observed_bytes,
                message,
            } => Err(PreservedRecoveryJournal::Oversized {
                max_bytes,
                observed_bytes,
                message,
            }),
            RecoveryJournalReadOutcome::Unreadable { message } => {
                Err(PreservedRecoveryJournal::Unreadable { message })
            }
        }
    }

    fn write_entries(
        &self,
        entries: &[DurableRecoveryEntry],
        deadline: RecoveryDeadline,
    ) -> io::Result<RecoveryJournalUpdateOutcome> {
        self.write_entries_with_before_replace(entries, deadline, |_| Ok(()))
    }

    fn write_entries_with_before_replace<F>(
        &self,
        entries: &[DurableRecoveryEntry],
        deadline: RecoveryDeadline,
        before_replace: F,
    ) -> io::Result<RecoveryJournalUpdateOutcome>
    where
        F: FnOnce(&Path) -> io::Result<()>,
    {
        deadline.check("encoding the recovery journal")?;
        let bytes = encode_journal(entries)?;
        if bytes.len() > MAX_RECOVERY_JOURNAL_BYTES {
            return Ok(RecoveryJournalUpdateOutcome::EncodedJournalOversized {
                max_bytes: MAX_RECOVERY_JOURNAL_BYTES,
                encoded_bytes: bytes.len(),
            });
        }
        let parent = journal_parent(&self.path)?;
        deadline.check("preparing the recovery journal directory")?;
        create_private_parent(parent)?;
        let mut replacement = PrivateReplacement::create(parent, &self.path)?;
        deadline.check("writing the recovery journal replacement")?;
        replacement.file.write_all(&bytes)?;
        replacement.file.flush()?;
        replacement.file.sync_all()?;
        deadline.check("installing the recovery journal replacement")?;
        before_replace(&replacement.path)?;
        deadline.check("installing the recovery journal replacement")?;
        replacement.install(&self.path)?;
        sync_parent_directory(parent)?;
        deadline.check("durably installing the recovery journal replacement")?;
        Ok(RecoveryJournalUpdateOutcome::Written {
            entry_count: entries.len(),
        })
    }

    fn remove_journal(&self, deadline: RecoveryDeadline) -> io::Result<()> {
        deadline.check("removing the recovery journal")?;
        match fs::remove_file(&self.path) {
            Ok(()) => {
                sync_parent_directory(journal_parent(&self.path)?)?;
                deadline.check("durably removing the recovery journal")
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn acquire_transaction_lock(&self, deadline: RecoveryDeadline) -> io::Result<JournalLock> {
        deadline.check("acquiring the recovery journal lock")?;
        let parent = journal_parent(&self.path)?;
        create_private_parent(parent)?;
        let lock_path = journal_lock_path(&self.path)?;
        JournalLock::acquire(lock_path, deadline)
    }

    fn oversized(&self, observed_bytes: Option<u64>) -> RecoveryJournalReadOutcome {
        let observed = observed_bytes
            .map(|bytes| bytes.to_string())
            .unwrap_or_else(|| "more than the configured limit".to_owned());
        RecoveryJournalReadOutcome::Oversized {
            max_bytes: MAX_RECOVERY_JOURNAL_BYTES,
            observed_bytes,
            message: format!(
                "recovery journal {} is oversized: observed {observed} bytes, maximum is {} bytes; move or inspect the file before restarting native Field",
                self.path.display(),
                MAX_RECOVERY_JOURNAL_BYTES
            ),
        }
    }

    fn unreadable(&self, operation: &str, error: &io::Error) -> RecoveryJournalReadOutcome {
        RecoveryJournalReadOutcome::Unreadable {
            message: format!(
                "could not {operation} recovery journal {}: {error}; fix its ownership, permissions, or file type before restarting native Field",
                self.path.display()
            ),
        }
    }
}

fn update_result_to_io(outcome: RecoveryJournalUpdateOutcome) -> io::Result<()> {
    match outcome {
        RecoveryJournalUpdateOutcome::Written { .. } => Ok(()),
        RecoveryJournalUpdateOutcome::EncodedJournalOversized {
            max_bytes,
            encoded_bytes,
        } => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "retained recovery journal requires {encoded_bytes} bytes but the maximum is {max_bytes} bytes"
            ),
        )),
        other => Err(io::Error::other(format!(
            "unexpected recovery journal checkpoint outcome: {other:?}"
        ))),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalDocument {
    kind: String,
    entries: Vec<DurableRecoveryEntry>,
}

#[derive(Serialize)]
struct JournalDocumentRef<'a> {
    kind: &'static str,
    entries: &'a [DurableRecoveryEntry],
}

fn parse_journal(bytes: &[u8], path: &Path) -> RecoveryJournalReadOutcome {
    let value = match serde_json::from_slice::<serde_json::Value>(bytes) {
        Ok(value) => value,
        Err(error) => {
            return RecoveryJournalReadOutcome::Malformed {
                message: format!(
                    "recovery journal {} is not valid JSON: {error}; move or repair it before restarting native Field",
                    path.display()
                ),
            };
        }
    };
    let version = match value
        .as_object()
        .and_then(|object| object.get("kind"))
        .and_then(serde_json::Value::as_str)
    {
        Some(version) => version,
        None => {
            return RecoveryJournalReadOutcome::Malformed {
                message: format!(
                    "recovery journal {} must contain a string kind; move or repair it before restarting native Field",
                    path.display()
                ),
            };
        }
    };
    if version != JOURNAL_KIND {
        return RecoveryJournalReadOutcome::FutureVersion {
            version: version.to_owned(),
            message: format!(
                "recovery journal {} uses unsupported kind {version:?}; open it with a compatible Nopal build or move it aside",
                path.display()
            ),
        };
    }
    let document = match serde_json::from_value::<JournalDocument>(value) {
        Ok(document) => document,
        Err(error) => {
            return RecoveryJournalReadOutcome::Malformed {
                message: format!(
                    "recovery journal {} is not exact valid v1 data: {error}; move or repair it before restarting native Field",
                    path.display()
                ),
            };
        }
    };
    if document.kind != JOURNAL_KIND {
        return RecoveryJournalReadOutcome::Malformed {
            message: "recovery journal kind changed while decoding".to_owned(),
        };
    }
    if document.entries.len() > MAX_RECOVERY_ENTRIES {
        return RecoveryJournalReadOutcome::Malformed {
            message: format!(
                "recovery journal {} contains {} entries but the maximum is {}; move or repair it before restarting native Field",
                path.display(),
                document.entries.len(),
                MAX_RECOVERY_ENTRIES
            ),
        };
    }
    for entry in &document.entries {
        if let Err(error) = entry.validate() {
            return RecoveryJournalReadOutcome::Malformed {
                message: format!(
                    "recovery journal {} contains invalid entry {:?}: {error}; move or repair it before restarting native Field",
                    path.display(),
                    entry.id
                ),
            };
        }
    }
    RecoveryJournalReadOutcome::Ready(RecoveryJournal {
        entries: document.entries,
    })
}

fn encode_journal(entries: &[DurableRecoveryEntry]) -> io::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec_pretty(&JournalDocumentRef {
        kind: JOURNAL_KIND,
        entries,
    })
    .map_err(io::Error::other)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn journal_parent(path: &Path) -> io::Result<&Path> {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => Ok(parent),
        Some(_) => Ok(Path::new(".")),
        None => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "recovery journal path must name a file",
        )),
    }
}

fn journal_lock_path(path: &Path) -> io::Result<PathBuf> {
    let name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing journal file name"))?
        .to_string_lossy();
    Ok(journal_parent(path)?.join(format!(".{name}.lock")))
}

struct JournalLock {
    file: File,
}

impl JournalLock {
    fn acquire(path: PathBuf, deadline: RecoveryDeadline) -> io::Result<Self> {
        let file = open_journal_lock(&path)?;
        loop {
            deadline.check("waiting for the recovery journal lock")?;
            match file.try_lock_exclusive() {
                Ok(()) => break,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    thread::sleep(LOCK_RETRY_INTERVAL.min(deadline.remaining()));
                }
                Err(error) => return Err(error),
            }
        }
        validate_journal_lock(&file, &path)?;
        Ok(Self { file })
    }
}

impl Drop for JournalLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

fn open_journal_lock(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options.open(path)?;
    validate_journal_lock(&file, path)?;
    Ok(file)
}

fn validate_journal_lock(file: &File, path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;

        let descriptor = file.metadata()?;
        if !descriptor.is_file() || descriptor.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recovery journal lock must be one regular file with exactly one filesystem link",
            ));
        }
        // SAFETY: geteuid has no preconditions and does not dereference memory.
        if descriptor.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "recovery journal lock must be owned by the effective user",
            ));
        }
        let path_metadata = fs::symlink_metadata(path)?;
        if path_metadata.dev() != descriptor.dev() || path_metadata.ino() != descriptor.ino() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "recovery journal lock path changed while it was being acquired",
            ));
        }
    }
    #[cfg(not(unix))]
    {
        let descriptor = file.metadata()?;
        if !descriptor.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recovery journal lock must be a regular file",
            ));
        }
        let _ = path;
    }
    Ok(())
}

fn open_journal_for_read(path: &Path) -> io::Result<File> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, OpenOptionsExt};

        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() || metadata.nlink() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recovery journal must be one regular file with exactly one filesystem link",
            ));
        }
        // SAFETY: geteuid has no preconditions and does not dereference memory.
        let effective_user = unsafe { libc::geteuid() };
        if metadata.uid() != effective_user {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "recovery journal must be owned by the effective user",
            ));
        }
        Ok(file)
    }
    #[cfg(not(unix))]
    {
        File::open(path)
    }
}

fn create_private_parent(parent: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        use std::os::unix::fs::PermissionsExt;

        let mut builder = fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder.create(parent)?;
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "recovery journal parent must be a real directory",
            ));
        }
        use std::os::unix::fs::MetadataExt;
        // SAFETY: geteuid has no preconditions and does not dereference memory.
        if metadata.uid() != unsafe { libc::geteuid() } {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "recovery journal parent must be owned by the effective user",
            ));
        }
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir_all(parent)
    }
}

struct PrivateReplacement {
    path: PathBuf,
    file: File,
    installed: bool,
}

impl PrivateReplacement {
    fn create(parent: &Path, destination: &Path) -> io::Result<Self> {
        let destination_name = destination
            .file_name()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "missing file name"))?
            .to_string_lossy();
        for _ in 0..TEMP_CREATE_ATTEMPTS {
            let mut nonce = [0_u8; 16];
            getrandom::fill(&mut nonce)
                .map_err(|error| io::Error::other(format!("generate temp nonce: {error}")))?;
            let nonce = nonce
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            let path = parent.join(format!(".{destination_name}.{nonce}.tmp"));
            let mut options = OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;

                options.mode(0o600);
            }
            match options.open(&path) {
                Ok(file) => {
                    return Ok(Self {
                        path,
                        file,
                        installed: false,
                    });
                }
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not create a unique private recovery journal replacement",
        ))
    }

    fn install(&mut self, destination: &Path) -> io::Result<()> {
        atomic_install(&self.path, destination)?;
        self.installed = true;
        Ok(())
    }
}

#[cfg(not(windows))]
fn atomic_install(source: &Path, destination: &Path) -> io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(windows)]
fn atomic_install(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;

    const MOVE_FILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVE_FILE_WRITE_THROUGH: u32 = 0x8;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        #[allow(non_snake_case)]
        fn MoveFileExW(
            existing_file_name: *const u16,
            new_file_name: *const u16,
            flags: u32,
        ) -> i32;
    }

    fn wide_path(path: &Path) -> io::Result<Vec<u16>> {
        let mut wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if wide.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "recovery journal path contains an interior NUL",
            ));
        }
        wide.push(0);
        Ok(wide)
    }

    let source = wide_path(source)?;
    let destination = wide_path(destination)?;
    // SAFETY: both paths are NUL-terminated buffers that remain alive for the
    // call, and the flags are documented MoveFileExW constants.
    let moved = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVE_FILE_REPLACE_EXISTING | MOVE_FILE_WRITE_THROUGH,
        )
    };
    if moved == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

impl Drop for PrivateReplacement {
    fn drop(&mut self) {
        if !self.installed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

fn sync_parent_directory(parent: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        File::open(parent)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = parent;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};
    use std::io::{Seek, SeekFrom};

    use tempfile::tempdir;

    use super::*;

    fn identity(namespace: &str, value: &str) -> DurableIdentity {
        DurableIdentity::new(namespace, value).unwrap()
    }

    fn filesystem_entry(id: &str, path: &Path, identity_value: &str) -> DurableRecoveryEntry {
        DurableRecoveryEntry::new(
            id,
            format!("filesystem {id}"),
            ResourceOwnership::ApplicationOwned,
            DurableRecoveryRecipe::Filesystem(
                FilesystemRecoveryRecipe::new(path, identity("unix.dev_inode", identity_value))
                    .unwrap(),
            ),
        )
        .unwrap()
    }

    fn process_entry(id: &str, pid: u32) -> DurableRecoveryEntry {
        DurableRecoveryEntry::new(
            id,
            format!("process {id}"),
            ResourceOwnership::ApplicationOwned,
            DurableRecoveryRecipe::VerifiedProcess(VerifiedProcessRecoveryRecipe::new(
                NonZeroU32::new(pid).unwrap(),
                identity("linux.start_time", "start-10"),
                identity("sha256.executable", "exe-20"),
            )),
        )
        .unwrap()
    }

    #[derive(Default)]
    struct FakeAdapter {
        absent: HashSet<String>,
        mismatched: HashMap<String, String>,
        failed: HashMap<String, String>,
        attempts: Vec<String>,
        deadlines: Vec<RecoveryDeadline>,
    }

    impl FakeAdapter {
        fn recover_key(
            &mut self,
            key: String,
            deadline: RecoveryDeadline,
        ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
            self.attempts.push(key.clone());
            self.deadlines.push(deadline);
            if let Some(message) = self.failed.get(&key) {
                return Err(RecoveryAdapterError::new(message.clone()));
            }
            if let Some(observed) = self.mismatched.get(&key) {
                return Ok(RecoveryDisposition::IdentityMismatch {
                    observed_identity: Some(observed.clone()),
                });
            }
            if !self.absent.insert(key) {
                return Ok(RecoveryDisposition::AlreadyAbsent);
            }
            Ok(RecoveryDisposition::Recovered)
        }
    }

    impl ExactRecoveryAdapter for FakeAdapter {
        fn recover_filesystem_exact(
            &mut self,
            recipe: &FilesystemRecoveryRecipe,
            deadline: RecoveryDeadline,
        ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
            self.recover_key(format!("file:{}", recipe.identity.value()), deadline)
        }

        fn recover_process_exact(
            &mut self,
            recipe: &VerifiedProcessRecoveryRecipe,
            deadline: RecoveryDeadline,
        ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
            self.recover_key(format!("process:{}", recipe.pid), deadline)
        }
    }

    #[test]
    fn rejects_borrowed_resources_relative_paths_and_pid_only_construction() {
        let root = tempdir().unwrap();
        let recipe = DurableRecoveryRecipe::Filesystem(
            FilesystemRecoveryRecipe::new(
                root.path().join("owned.sock"),
                identity("unix.dev_inode", "1:2"),
            )
            .unwrap(),
        );
        let error = DurableRecoveryEntry::new(
            "borrowed",
            "borrowed socket",
            ResourceOwnership::Borrowed,
            recipe,
        )
        .unwrap_err();
        assert_eq!(error.kind(), RecoveryEntryErrorKind::BorrowedResource);

        let error =
            FilesystemRecoveryRecipe::new("relative.sock", identity("unix.dev_inode", "1:2"))
                .unwrap_err();
        assert_eq!(error.kind(), RecoveryEntryErrorKind::InvalidPath);

        assert!(NonZeroU32::new(0).is_none());
    }

    #[test]
    fn exact_v1_round_trip_preserves_stable_order_and_process_identities() {
        let root = tempdir().unwrap();
        let path = root.path().join("recovery.json");
        let store = RecoveryJournalStore::new(&path);
        let first = filesystem_entry("socket", &root.path().join("native.sock"), "5:10");
        let second = process_entry("worker", 42);

        assert_eq!(
            store.register(first.clone()).unwrap(),
            RecoveryJournalUpdateOutcome::Written { entry_count: 1 }
        );
        assert_eq!(
            store.register(second.clone()).unwrap(),
            RecoveryJournalUpdateOutcome::Written { entry_count: 2 }
        );

        assert_eq!(
            store.read(),
            RecoveryJournalReadOutcome::Ready(RecoveryJournal {
                entries: vec![first, second]
            })
        );
        let bytes = fs::read(&path).unwrap();
        assert!(bytes.ends_with(b"\n"));
        assert!(
            String::from_utf8(bytes)
                .unwrap()
                .contains("nopal.native_owned_resources/v1")
        );
    }

    #[test]
    fn registration_replaces_same_id_and_retire_removes_empty_journal() {
        let root = tempdir().unwrap();
        let path = root.path().join("recovery.json");
        let store = RecoveryJournalStore::new(&path);
        store
            .register(filesystem_entry(
                "socket",
                &root.path().join("old.sock"),
                "1:1",
            ))
            .unwrap();
        let replacement = filesystem_entry("socket", &root.path().join("new.sock"), "1:2");
        assert_eq!(
            store.register(replacement.clone()).unwrap(),
            RecoveryJournalUpdateOutcome::Written { entry_count: 1 }
        );
        assert_eq!(
            store.read(),
            RecoveryJournalReadOutcome::Ready(RecoveryJournal {
                entries: vec![replacement]
            })
        );
        assert_eq!(
            store.retire("socket").unwrap(),
            RecoveryJournalUpdateOutcome::Written { entry_count: 0 }
        );
        assert!(!path.exists());
        assert_eq!(
            store.retire("socket").unwrap(),
            RecoveryJournalUpdateOutcome::EntryMissing
        );
    }

    #[test]
    fn concurrent_cloned_store_transactions_do_not_lose_registers_or_retires() {
        use std::sync::{Arc, Barrier};

        let root = tempdir().unwrap();
        let store = RecoveryJournalStore::new(root.path().join("recovery.json"));
        let barrier = Arc::new(Barrier::new(17));
        let mut registrations = Vec::new();
        for index in 0..16 {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            let path = root.path().join(format!("{index}.sock"));
            registrations.push(std::thread::spawn(move || {
                barrier.wait();
                store
                    .register(filesystem_entry(
                        &format!("entry-{index}"),
                        &path,
                        &format!("1:{index}"),
                    ))
                    .unwrap();
            }));
        }
        barrier.wait();
        for registration in registrations {
            registration.join().unwrap();
        }
        let RecoveryJournalReadOutcome::Ready(journal) = store.read() else {
            panic!("concurrent registrations must leave a valid journal");
        };
        assert_eq!(journal.entries().len(), 16);

        let barrier = Arc::new(Barrier::new(17));
        let mut retirements = Vec::new();
        for index in 0..16 {
            let store = store.clone();
            let barrier = Arc::clone(&barrier);
            retirements.push(std::thread::spawn(move || {
                barrier.wait();
                store.retire(&format!("entry-{index}")).unwrap();
            }));
        }
        barrier.wait();
        for retirement in retirements {
            retirement.join().unwrap();
        }
        assert_eq!(store.read(), RecoveryJournalReadOutcome::Missing);
    }

    #[test]
    fn recovery_process_lock_helper() {
        let Ok(journal_path) = std::env::var("NOPAL_RECOVERY_LOCK_TEST_JOURNAL") else {
            return;
        };
        let id = std::env::var("NOPAL_RECOVERY_LOCK_TEST_ID").unwrap();
        let journal_path = PathBuf::from(journal_path);
        let resource_path = journal_path.parent().unwrap().join(format!("{id}.sock"));
        RecoveryJournalStore::new(journal_path)
            .register(filesystem_entry(
                &id,
                &resource_path,
                &format!("process:{id}"),
            ))
            .unwrap();
    }

    #[test]
    fn concurrent_process_transactions_do_not_lose_registrations() {
        let root = tempdir().unwrap();
        let journal_path = root.path().join("recovery.json");
        let executable = std::env::current_exe().unwrap();
        let mut children = Vec::new();
        for index in 0..8 {
            children.push(
                std::process::Command::new(&executable)
                    .args(["--exact", "recovery::tests::recovery_process_lock_helper"])
                    .env("NOPAL_RECOVERY_LOCK_TEST_JOURNAL", &journal_path)
                    .env("NOPAL_RECOVERY_LOCK_TEST_ID", format!("child-{index}"))
                    .spawn()
                    .unwrap(),
            );
        }
        for mut child in children {
            assert!(child.wait().unwrap().success());
        }

        let RecoveryJournalReadOutcome::Ready(journal) =
            RecoveryJournalStore::new(journal_path).read()
        else {
            panic!("process registrations must leave a valid journal");
        };
        assert_eq!(journal.entries().len(), 8);
    }

    #[test]
    fn registration_waits_for_the_complete_reconcile_transaction() {
        use std::sync::mpsc;

        struct BlockingAdapter {
            entered: mpsc::Sender<()>,
            release: mpsc::Receiver<()>,
        }

        impl ExactRecoveryAdapter for BlockingAdapter {
            fn recover_filesystem_exact(
                &mut self,
                _recipe: &FilesystemRecoveryRecipe,
                _deadline: RecoveryDeadline,
            ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
                self.entered.send(()).unwrap();
                self.release.recv().unwrap();
                Ok(RecoveryDisposition::Recovered)
            }

            fn recover_process_exact(
                &mut self,
                _recipe: &VerifiedProcessRecoveryRecipe,
                _deadline: RecoveryDeadline,
            ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
                unreachable!()
            }
        }

        let root = tempdir().unwrap();
        let store = RecoveryJournalStore::new(root.path().join("recovery.json"));
        store
            .register(filesystem_entry(
                "stale",
                &root.path().join("stale.sock"),
                "4:1",
            ))
            .unwrap();
        let (entered_tx, entered_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let reconcile_store = store.clone();
        let reconcile = std::thread::spawn(move || {
            reconcile_store
                .reconcile(&mut BlockingAdapter {
                    entered: entered_tx,
                    release: release_rx,
                })
                .unwrap()
        });
        entered_rx.recv().unwrap();

        let register_store = store.clone();
        let registered = filesystem_entry("new", &root.path().join("new.sock"), "4:2");
        let registered_for_thread = registered.clone();
        let (done_tx, done_rx) = mpsc::channel();
        let registration = std::thread::spawn(move || {
            let result = register_store.register(registered_for_thread);
            done_tx.send(()).unwrap();
            result
        });
        assert!(done_rx.recv_timeout(Duration::from_millis(30)).is_err());
        release_tx.send(()).unwrap();
        assert!(matches!(
            reconcile.join().unwrap(),
            RecoveryReconcileOutcome::Completed(_)
        ));
        registration.join().unwrap().unwrap();

        assert_eq!(
            store.read(),
            RecoveryJournalReadOutcome::Ready(RecoveryJournal {
                entries: vec![registered]
            })
        );
    }

    #[test]
    fn transaction_lock_wait_respects_the_shared_deadline() {
        let root = tempdir().unwrap();
        let path = root.path().join("recovery.json");
        let lock_path = journal_lock_path(&path).unwrap();
        let lock = open_journal_lock(&lock_path).unwrap();
        lock.lock_exclusive().unwrap();
        let store = RecoveryJournalStore::new(path);
        let started = Instant::now();
        let error = store
            .register_with_deadline(
                filesystem_entry("socket", &root.path().join("socket.sock"), "5:1"),
                RecoveryDeadline::from_timeout(Duration::from_millis(20)),
            )
            .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::TimedOut);
        assert!(started.elapsed() < Duration::from_millis(150));
        FileExt::unlock(&lock).unwrap();
    }

    #[test]
    fn reconciliation_lock_failure_preserves_unknown_entry_count() {
        let root = tempdir().unwrap();
        let path = root.path().join("recovery.json");
        let store = RecoveryJournalStore::new(&path);
        let entry = filesystem_entry("socket", &root.path().join("socket.sock"), "5:1");
        store.register(entry.clone()).unwrap();

        let lock_path = journal_lock_path(&path).unwrap();
        let lock = open_journal_lock(&lock_path).unwrap();
        lock.lock_exclusive().unwrap();
        let mut adapter = FakeAdapter::default();
        let failure = store
            .reconcile_with_timeout(&mut adapter, Duration::from_millis(20))
            .unwrap_err();

        assert_eq!(failure.error().kind(), io::ErrorKind::TimedOut);
        assert!(failure.report().is_none());
        assert!(
            failure
                .to_string()
                .contains("retained entry count is unknown")
        );
        FileExt::unlock(&lock).unwrap();
        assert_eq!(
            store.read(),
            RecoveryJournalReadOutcome::Ready(RecoveryJournal {
                entries: vec![entry]
            })
        );
    }

    #[cfg(unix)]
    #[test]
    fn transaction_lock_rejects_symlinks_and_hardlinks_without_touching_targets() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let journal_path = root.path().join("recovery.json");
        let lock_path = journal_lock_path(&journal_path).unwrap();
        let target = root.path().join("external.lock");
        fs::write(&target, b"external").unwrap();
        symlink(&target, &lock_path).unwrap();
        let store = RecoveryJournalStore::new(&journal_path);
        let entry = filesystem_entry("socket", &root.path().join("socket.sock"), "2:1");
        assert!(store.register(entry.clone()).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"external");

        fs::remove_file(&lock_path).unwrap();
        fs::hard_link(&target, &lock_path).unwrap();
        assert!(store.register(entry).is_err());
        assert_eq!(fs::read(&target).unwrap(), b"external");
    }

    #[cfg(unix)]
    #[test]
    fn transaction_lock_rejects_a_symlinked_parent_without_chmodding_the_target() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let root = tempdir().unwrap();
        let target = root.path().join("external-dir");
        fs::create_dir(&target).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o755)).unwrap();
        let linked_parent = root.path().join("linked-state");
        symlink(&target, &linked_parent).unwrap();
        let store = RecoveryJournalStore::new(linked_parent.join("recovery.json"));
        let entry = filesystem_entry("socket", &root.path().join("socket.sock"), "2:2");

        assert!(store.register(entry).is_err());
        assert_eq!(
            fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        assert!(!target.join(".recovery.json.lock").exists());
    }

    #[test]
    fn malformed_future_and_oversized_journals_are_preserved_byte_for_byte() {
        let root = tempdir().unwrap();
        let path = root.path().join("recovery.json");
        let store = RecoveryJournalStore::new(&path);
        let candidate = filesystem_entry("new", &root.path().join("new.sock"), "2:2");

        for original in [
            b"not json".to_vec(),
            br#"{"kind":"nopal.native_owned_resources/v9","entries":[]}"#.to_vec(),
        ] {
            fs::write(&path, &original).unwrap();
            let outcome = store.register(candidate.clone()).unwrap();
            assert!(matches!(
                outcome,
                RecoveryJournalUpdateOutcome::Preserved(_)
            ));
            assert_eq!(fs::read(&path).unwrap(), original);
        }

        let mut file = File::create(&path).unwrap();
        file.seek(SeekFrom::Start(MAX_RECOVERY_JOURNAL_BYTES as u64))
            .unwrap();
        file.write_all(b"x").unwrap();
        drop(file);
        let original_len = fs::metadata(&path).unwrap().len();
        assert!(matches!(
            store.read(),
            RecoveryJournalReadOutcome::Oversized { .. }
        ));
        assert!(matches!(
            store.register(candidate).unwrap(),
            RecoveryJournalUpdateOutcome::Preserved(PreservedRecoveryJournal::Oversized { .. })
        ));
        assert_eq!(fs::metadata(&path).unwrap().len(), original_len);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_and_hardlinked_journals_fail_closed_without_touching_targets() {
        use std::os::unix::fs::symlink;

        let root = tempdir().unwrap();
        let path = root.path().join("recovery.json");
        let target = root.path().join("target.json");
        let original = b"external content";
        fs::write(&target, original).unwrap();
        symlink(&target, &path).unwrap();
        let store = RecoveryJournalStore::new(&path);
        assert!(matches!(
            store.read(),
            RecoveryJournalReadOutcome::Unreadable { .. }
        ));
        let candidate = filesystem_entry("new", &root.path().join("new.sock"), "2:2");
        assert!(matches!(
            store.register(candidate.clone()).unwrap(),
            RecoveryJournalUpdateOutcome::Preserved(PreservedRecoveryJournal::Unreadable { .. })
        ));
        assert_eq!(fs::read(&target).unwrap(), original);

        fs::remove_file(&path).unwrap();
        fs::hard_link(&target, &path).unwrap();
        assert!(matches!(
            store.read(),
            RecoveryJournalReadOutcome::Unreadable { .. }
        ));
        assert!(matches!(
            store.register(candidate).unwrap(),
            RecoveryJournalUpdateOutcome::Preserved(PreservedRecoveryJournal::Unreadable { .. })
        ));
        assert_eq!(fs::read(&target).unwrap(), original);
    }

    #[test]
    fn reconciliation_attempts_every_entry_and_retains_failures_and_mismatches() {
        let root = tempdir().unwrap();
        let path = root.path().join("recovery.json");
        let store = RecoveryJournalStore::new(&path);
        let recovered = filesystem_entry("recovered", &root.path().join("a.sock"), "1:1");
        let mismatch = filesystem_entry("mismatch", &root.path().join("b.sock"), "1:2");
        let failed = process_entry("failed", 77);
        for entry in [recovered, mismatch.clone(), failed.clone()] {
            store.register(entry).unwrap();
        }
        let mut adapter = FakeAdapter::default();
        adapter
            .mismatched
            .insert("file:1:2".to_owned(), "9:9".to_owned());
        adapter
            .failed
            .insert("process:77".to_owned(), "permission denied".to_owned());

        let RecoveryReconcileOutcome::Completed(report) = store.reconcile(&mut adapter).unwrap()
        else {
            panic!("expected completed reconciliation");
        };
        assert_eq!(adapter.attempts.len(), 3);
        assert!(adapter.deadlines.windows(2).all(|pair| pair[0] == pair[1]));
        assert_eq!(report.attempts().len(), 3);
        assert_eq!(report.remaining_entries(), 2);
        assert!(matches!(
            report.attempts()[1].disposition(),
            RecoveryAttemptDisposition::RetainedIdentityMismatch { .. }
        ));
        assert!(matches!(
            report.attempts()[2].disposition(),
            RecoveryAttemptDisposition::RetainedFailure { message } if message == "permission denied"
        ));
        assert_eq!(
            store.read(),
            RecoveryJournalReadOutcome::Ready(RecoveryJournal {
                entries: vec![mismatch, failed]
            })
        );
    }

    #[test]
    fn reconciliation_retains_unattempted_entries_and_reports_shared_deadline_exhaustion() {
        struct SlowAdapter {
            deadlines: Vec<RecoveryDeadline>,
        }

        impl ExactRecoveryAdapter for SlowAdapter {
            fn recover_filesystem_exact(
                &mut self,
                _recipe: &FilesystemRecoveryRecipe,
                deadline: RecoveryDeadline,
            ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
                self.deadlines.push(deadline);
                std::thread::sleep(deadline.remaining() + Duration::from_millis(5));
                Err(RecoveryAdapterError::deadline_exceeded(
                    "filesystem cleanup reached the shared deadline",
                ))
            }

            fn recover_process_exact(
                &mut self,
                _recipe: &VerifiedProcessRecoveryRecipe,
                deadline: RecoveryDeadline,
            ) -> Result<RecoveryDisposition, RecoveryAdapterError> {
                self.deadlines.push(deadline);
                Err(RecoveryAdapterError::deadline_exceeded(
                    "process cleanup reached the shared deadline",
                ))
            }
        }

        let root = tempdir().unwrap();
        let store = RecoveryJournalStore::new(root.path().join("recovery.json"));
        for index in 0..2 {
            store
                .register(filesystem_entry(
                    &format!("entry-{index}"),
                    &root.path().join(format!("{index}.sock")),
                    &format!("3:{index}"),
                ))
                .unwrap();
        }
        let mut adapter = SlowAdapter {
            deadlines: Vec::new(),
        };
        let failure = store
            .reconcile_with_timeout(&mut adapter, Duration::from_millis(20))
            .unwrap_err();

        assert_eq!(adapter.deadlines.len(), 1);
        assert_eq!(failure.error().kind(), io::ErrorKind::TimedOut);
        let report = failure.report().unwrap();
        assert_eq!(report.attempts().len(), 2);
        assert!(report.attempts().iter().all(|attempt| matches!(
            attempt.disposition(),
            RecoveryAttemptDisposition::RetainedDeadlineExceeded { .. }
        )));
        let RecoveryJournalReadOutcome::Ready(journal) = store.read() else {
            panic!("timed-out reconciliation must retain the original journal");
        };
        assert_eq!(journal.entries().len(), 2);
    }

    #[test]
    fn successful_and_already_absent_entries_clear_the_journal() {
        let root = tempdir().unwrap();
        let path = root.path().join("recovery.json");
        let store = RecoveryJournalStore::new(&path);
        store
            .register(filesystem_entry(
                "socket",
                &root.path().join("native.sock"),
                "4:4",
            ))
            .unwrap();
        let mut adapter = FakeAdapter::default();
        adapter.absent.insert("file:4:4".to_owned());

        let RecoveryReconcileOutcome::Completed(report) = store.reconcile(&mut adapter).unwrap()
        else {
            panic!("expected completed reconciliation");
        };
        assert!(matches!(
            report.attempts()[0].disposition(),
            RecoveryAttemptDisposition::AlreadyAbsent
        ));
        assert_eq!(report.remaining_entries(), 0);
        assert!(!path.exists());
    }

    #[test]
    fn interrupted_checkpoint_is_safe_to_retry_after_restart() {
        let root = tempdir().unwrap();
        let path = root.path().join("recovery.json");
        let store = RecoveryJournalStore::new(&path);
        let first = filesystem_entry("first", &root.path().join("first.sock"), "8:1");
        let second = filesystem_entry("second", &root.path().join("second.sock"), "8:2");
        store.register(first.clone()).unwrap();
        store.register(second.clone()).unwrap();

        let mut first_process = FakeAdapter::default();
        first_process.failed.insert(
            "file:8:2".to_owned(),
            "resource is temporarily busy".to_owned(),
        );
        let error = store
            .reconcile_with_before_replace(
                &mut first_process,
                RecoveryDeadline::from_timeout(DEFAULT_RECOVERY_TIMEOUT),
                |_| Err(io::Error::other("simulated process crash before rename")),
            )
            .unwrap_err();
        let report = error.report().unwrap();
        assert_eq!(report.attempts().len(), 2);
        assert_eq!(report.remaining_entries(), 1);
        assert_eq!(
            store.read(),
            RecoveryJournalReadOutcome::Ready(RecoveryJournal {
                entries: vec![first, second]
            })
        );

        let mut restarted_process = first_process;
        restarted_process.failed.clear();
        let RecoveryReconcileOutcome::Completed(report) =
            store.reconcile(&mut restarted_process).unwrap()
        else {
            panic!("expected completed restart reconciliation");
        };
        assert_eq!(report.attempts().len(), 2);
        assert!(matches!(
            report.attempts()[0].disposition(),
            RecoveryAttemptDisposition::AlreadyAbsent
        ));
        assert_eq!(report.remaining_entries(), 0);
        assert!(!path.exists());
    }

    #[test]
    fn blocked_journal_never_calls_cleanup_adapter() {
        let root = tempdir().unwrap();
        let path = root.path().join("recovery.json");
        fs::write(&path, b"malformed").unwrap();
        let store = RecoveryJournalStore::new(&path);
        let mut adapter = FakeAdapter::default();

        assert!(matches!(
            store.reconcile(&mut adapter).unwrap(),
            RecoveryReconcileOutcome::Blocked(PreservedRecoveryJournal::Malformed { .. })
        ));
        assert!(adapter.attempts.is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"malformed");
    }
}
