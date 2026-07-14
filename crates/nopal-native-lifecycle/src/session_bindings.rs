//! Renderer-neutral bindings for presenting one exact Core Session.

use std::fmt;
use std::num::NonZeroU32;

use crate::resources::{
    ExactSessionIdentity, InvalidDegradationDiagnostic, PresentationMode, SessionPresentation,
    StructuredOutputDegradation,
};

/// The presentation resource being created, validated, or closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BindingKind {
    /// A structured activity runtime.
    StructuredOutput,
    /// A terminal process and pane.
    Terminal,
}

/// A blank renderer or pane identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidBindingIdentity;

impl fmt::Display for InvalidBindingIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("binding identity must not be blank")
    }
}

impl std::error::Error for InvalidBindingIdentity {}

macro_rules! string_identity {
    ($name:ident, $description:literal) => {
        #[doc = $description]
        #[derive(Clone, Debug, Eq, Hash, PartialEq)]
        pub struct $name(String);

        impl $name {
            /// Validates and creates the identity.
            pub fn new(value: impl Into<String>) -> Result<Self, InvalidBindingIdentity> {
                let value = value.into();
                if value.trim().is_empty() {
                    return Err(InvalidBindingIdentity);
                }
                Ok(Self(value))
            }

            /// Returns the stable wire value.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

string_identity!(
    StructuredRuntimeIdentity,
    "Stable identity for one structured activity runtime."
);
string_identity!(
    TerminalPaneIdentity,
    "Stable identity for one terminal pane."
);
string_identity!(
    StructuredCursor,
    "The last structured event cursor applied to the presentation."
);
string_identity!(
    StructuredHistoryToken,
    "An opaque token identifying the structured history already loaded."
);

/// Stable identity for the process attached to a terminal pane.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct TerminalProcessIdentity(NonZeroU32);

impl TerminalProcessIdentity {
    /// Creates an identity from a non-zero operating-system process id.
    pub fn new(process_id: u32) -> Result<Self, InvalidBindingIdentity> {
        NonZeroU32::new(process_id)
            .map(Self)
            .ok_or(InvalidBindingIdentity)
    }

    /// Returns the process id.
    pub fn get(self) -> u32 {
        self.0.get()
    }
}

/// Independently resolved identity of the process already hosting the Core Session.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SessionHostProcessIdentity(NonZeroU32);

impl SessionHostProcessIdentity {
    /// Creates an identity from a non-zero operating-system process id.
    pub fn new(process_id: u32) -> Result<Self, InvalidBindingIdentity> {
        NonZeroU32::new(process_id)
            .map(Self)
            .ok_or(InvalidBindingIdentity)
    }

    /// Returns the existing Session-host process id.
    pub fn get(self) -> u32 {
        self.0.get()
    }
}

/// Structured state that must cross a failed binding and its replacement.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuredContinuity {
    cursor: StructuredCursor,
    history_token: StructuredHistoryToken,
}

impl StructuredContinuity {
    /// Creates one structured continuity checkpoint.
    pub fn new(cursor: StructuredCursor, history_token: StructuredHistoryToken) -> Self {
        Self {
            cursor,
            history_token,
        }
    }

    /// Returns the last applied activity cursor.
    pub fn cursor(&self) -> &StructuredCursor {
        &self.cursor
    }

    /// Returns the loaded-history token.
    pub fn history_token(&self) -> &StructuredHistoryToken {
        &self.history_token
    }
}

/// Typed identity exposed by a structured activity binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StructuredOutputBindingIdentity {
    session: ExactSessionIdentity,
    runtime: StructuredRuntimeIdentity,
}

impl StructuredOutputBindingIdentity {
    /// Names the exact Session and structured runtime behind one binding.
    pub fn new(session: ExactSessionIdentity, runtime: StructuredRuntimeIdentity) -> Self {
        Self { session, runtime }
    }

    /// Returns the exact Core Session identity.
    pub fn session(&self) -> &ExactSessionIdentity {
        &self.session
    }

    /// Returns the structured runtime identity.
    pub fn runtime(&self) -> &StructuredRuntimeIdentity {
        &self.runtime
    }
}

/// Typed identity exposed by a terminal binding.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TerminalBindingIdentity {
    session: ExactSessionIdentity,
    process: TerminalProcessIdentity,
    pane: TerminalPaneIdentity,
}

impl TerminalBindingIdentity {
    /// Names the exact Session, process, and pane behind one binding.
    pub fn new(
        session: ExactSessionIdentity,
        process: TerminalProcessIdentity,
        pane: TerminalPaneIdentity,
    ) -> Self {
        Self {
            session,
            process,
            pane,
        }
    }

    /// Returns the exact Core Session identity.
    pub fn session(&self) -> &ExactSessionIdentity {
        &self.session
    }

    /// Returns the terminal process identity.
    pub fn process(&self) -> TerminalProcessIdentity {
        self.process
    }

    /// Returns the terminal pane identity.
    pub fn pane(&self) -> &TerminalPaneIdentity {
        &self.pane
    }
}

/// Immutable state passed to an injectable binding factory.
#[derive(Clone, Copy, Debug)]
pub struct SessionBindingContext<'a> {
    session: &'a ExactSessionIdentity,
    session_host_process: SessionHostProcessIdentity,
    continuity: &'a StructuredContinuity,
}

impl<'a> SessionBindingContext<'a> {
    fn new(
        session: &'a ExactSessionIdentity,
        session_host_process: SessionHostProcessIdentity,
        continuity: &'a StructuredContinuity,
    ) -> Self {
        Self {
            session,
            session_host_process,
            continuity,
        }
    }

    /// Returns the already-existing exact Core Session.
    pub fn session(&self) -> &ExactSessionIdentity {
        self.session
    }

    /// Returns the independently resolved existing Session-host process.
    pub fn session_host_process(&self) -> SessionHostProcessIdentity {
        self.session_host_process
    }

    /// Returns the structured resume checkpoint.
    pub fn continuity(&self) -> &StructuredContinuity {
        self.continuity
    }
}

/// A renderer-neutral owned structured activity binding.
pub trait StructuredOutputBinding {
    /// Returns the Session and runtime actually bound.
    fn identity(&self) -> &StructuredOutputBindingIdentity;

    /// Closes the owned binding.
    fn close(&mut self) -> Result<(), BindingCloseError>;
}

/// A renderer-neutral owned terminal binding.
pub trait TerminalBinding {
    /// Returns the Session, process, and pane actually bound.
    fn identity(&self) -> &TerminalBindingIdentity;

    /// Closes the owned binding.
    fn close(&mut self) -> Result<(), BindingCloseError>;
}

/// Injectable structured binding constructor.
pub trait StructuredOutputBindingFactory {
    /// The owned binding produced by this factory.
    type Binding: StructuredOutputBinding;

    /// Binds structured output to the supplied existing Session.
    fn bind(
        &mut self,
        context: SessionBindingContext<'_>,
    ) -> Result<Self::Binding, BindingFactoryError>;
}

/// Injectable terminal binding constructor.
pub trait TerminalBindingFactory {
    /// The owned binding produced by this factory.
    type Binding: TerminalBinding;

    /// Lazily attaches a terminal to the supplied existing Session.
    fn bind(
        &mut self,
        context: SessionBindingContext<'_>,
    ) -> Result<Self::Binding, BindingFactoryError>;
}

/// A factory diagnostic that does not erase which binding was requested.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingFactoryError(String);

impl BindingFactoryError {
    /// Creates a factory diagnostic.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// Returns the original diagnostic.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BindingFactoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BindingFactoryError {}

/// A binding cleanup diagnostic.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingCloseError(String);

impl BindingCloseError {
    /// Creates a cleanup diagnostic.
    pub fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }

    /// Returns the original diagnostic.
    pub fn message(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for BindingCloseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for BindingCloseError {}

/// A fail-closed Session binding transition error.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionBindingError {
    /// A factory could not create the requested binding.
    Factory {
        /// The binding being requested.
        kind: BindingKind,
        /// The factory's diagnostic.
        error: BindingFactoryError,
    },
    /// A factory returned a binding for a different Core Session.
    SessionMismatch {
        /// The rejected binding kind.
        kind: BindingKind,
        /// The controller's immutable Session identity.
        expected: ExactSessionIdentity,
        /// The Session identity exposed by the rejected binding.
        actual: ExactSessionIdentity,
        /// Cleanup evidence when rejecting the owned binding also failed.
        cleanup_error: Option<BindingCloseError>,
    },
    /// A Terminal factory attached to a process other than the existing Session host.
    TerminalProcessMismatch {
        /// The independently resolved Session-host process.
        expected: SessionHostProcessIdentity,
        /// The process exposed by the rejected Terminal binding.
        actual: TerminalProcessIdentity,
        /// Cleanup evidence when rejecting the owned binding also failed.
        cleanup_error: Option<BindingCloseError>,
    },
    /// An active or replaced owned binding could not be closed.
    Cleanup {
        /// The binding being closed.
        kind: BindingKind,
        /// The cleanup diagnostic.
        error: BindingCloseError,
    },
    /// Structured output failed without an actionable user-facing diagnostic.
    InvalidDegradationDiagnostic,
}

impl fmt::Display for SessionBindingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Factory { kind, error } => {
                write!(formatter, "{kind:?} binding failed: {error}")
            }
            Self::SessionMismatch {
                kind,
                expected,
                actual,
                cleanup_error,
            } => {
                write!(
                    formatter,
                    "{kind:?} binding Session mismatch: expected {}/{}, got {}/{}",
                    expected.plot_id(),
                    expected.session_id(),
                    actual.plot_id(),
                    actual.session_id()
                )?;
                if let Some(cleanup_error) = cleanup_error {
                    write!(
                        formatter,
                        "; rejected binding cleanup failed: {cleanup_error}"
                    )?;
                }
                Ok(())
            }
            Self::TerminalProcessMismatch {
                expected,
                actual,
                cleanup_error,
            } => {
                write!(
                    formatter,
                    "Terminal binding process mismatch: expected existing Session host {}, got {}",
                    expected.get(),
                    actual.get()
                )?;
                if let Some(cleanup_error) = cleanup_error {
                    write!(
                        formatter,
                        "; rejected binding cleanup failed: {cleanup_error}"
                    )?;
                }
                Ok(())
            }
            Self::Cleanup { kind, error } => {
                write!(formatter, "{kind:?} binding cleanup failed: {error}")
            }
            Self::InvalidDegradationDiagnostic => formatter.write_str(
                "structured output degradation requires a non-blank actionable diagnostic",
            ),
        }
    }
}

impl std::error::Error for SessionBindingError {}

impl From<InvalidDegradationDiagnostic> for SessionBindingError {
    fn from(_: InvalidDegradationDiagnostic) -> Self {
        Self::InvalidDegradationDiagnostic
    }
}

/// An owning startup failure that preserves cleanup authority for rejected output.
pub struct SessionBindingStartError<Binding>
where
    Binding: StructuredOutputBinding,
{
    error: Box<SessionBindingError>,
    pending_output_cleanup: Option<Box<Binding>>,
}

impl<Binding> SessionBindingStartError<Binding>
where
    Binding: StructuredOutputBinding,
{
    fn new(error: SessionBindingError, pending_output_cleanup: Option<Binding>) -> Self {
        Self {
            error: Box::new(error),
            pending_output_cleanup: pending_output_cleanup.map(Box::new),
        }
    }

    /// Returns the diagnostic that prevented controller startup.
    pub fn binding_error(&self) -> &SessionBindingError {
        self.error.as_ref()
    }

    /// Returns whether a rejected output binding still needs cleanup.
    pub fn has_pending_cleanup(&self) -> bool {
        self.pending_output_cleanup.is_some()
    }

    /// Retries cleanup of the same rejected output binding.
    pub fn retry_cleanup(&mut self) -> Result<(), BindingCloseError> {
        let Some(mut binding) = self.pending_output_cleanup.take() else {
            return Ok(());
        };
        match binding.close() {
            Ok(()) => Ok(()),
            Err(error) => {
                self.pending_output_cleanup = Some(binding);
                Err(error)
            }
        }
    }
}

impl<Binding> fmt::Debug for SessionBindingStartError<Binding>
where
    Binding: StructuredOutputBinding,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SessionBindingStartError")
            .field("error", self.error.as_ref())
            .field("has_pending_cleanup", &self.has_pending_cleanup())
            .finish()
    }
}

impl<Binding> fmt::Display for SessionBindingStartError<Binding>
where
    Binding: StructuredOutputBinding,
{
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.error.fmt(formatter)
    }
}

impl<Binding> std::error::Error for SessionBindingStartError<Binding>
where
    Binding: StructuredOutputBinding,
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(self.error.as_ref())
    }
}

impl<Binding> Drop for SessionBindingStartError<Binding>
where
    Binding: StructuredOutputBinding,
{
    fn drop(&mut self) {
        let _ = self.retry_cleanup();
    }
}

/// Non-fatal cleanup evidence from an otherwise applied presentation transition.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BindingCleanupWarning {
    kind: BindingKind,
    error: BindingCloseError,
}

impl BindingCleanupWarning {
    fn new(kind: BindingKind, error: BindingCloseError) -> Self {
        Self { kind, error }
    }

    /// Returns the retained binding kind.
    pub fn kind(&self) -> BindingKind {
        self.kind
    }

    /// Returns the cleanup diagnostic.
    pub fn error(&self) -> &BindingCloseError {
        &self.error
    }
}

/// Result of a presentation transition that was definitely applied.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionTransitionOutcome {
    /// The transition applied and all superseded binding cleanup completed.
    Applied,
    /// The transition applied, while one superseded binding remains owned for retry.
    AppliedWithCleanupWarning(BindingCleanupWarning),
}

impl SessionTransitionOutcome {
    fn from_cleanup_warning(warning: Option<BindingCleanupWarning>) -> Self {
        warning.map_or(Self::Applied, Self::AppliedWithCleanupWarning)
    }
}

/// Owns presentation bindings for one immutable, already-existing Core Session.
///
/// The controller has no Session factory.
/// Output and Terminal factories only receive the immutable Session identity,
/// which prevents a presentation transition from creating another Core Session.
pub struct SessionBindingController<OutputFactory, TerminalFactory>
where
    OutputFactory: StructuredOutputBindingFactory,
    TerminalFactory: TerminalBindingFactory,
{
    presentation: SessionPresentation,
    session_host_process: SessionHostProcessIdentity,
    continuity: StructuredContinuity,
    output_factory: OutputFactory,
    terminal_factory: TerminalFactory,
    output: Option<OutputFactory::Binding>,
    terminal: Option<TerminalFactory::Binding>,
    pending_output_cleanup: Vec<OutputFactory::Binding>,
    pending_terminal_cleanup: Vec<TerminalFactory::Binding>,
}

impl<OutputFactory, TerminalFactory> SessionBindingController<OutputFactory, TerminalFactory>
where
    OutputFactory: StructuredOutputBindingFactory,
    TerminalFactory: TerminalBindingFactory,
{
    /// Starts healthy with only structured output bound.
    ///
    /// Terminal construction is deliberately absent from this startup path.
    pub fn start(
        identity: ExactSessionIdentity,
        session_host_process: SessionHostProcessIdentity,
        continuity: StructuredContinuity,
        mut output_factory: OutputFactory,
        terminal_factory: TerminalFactory,
    ) -> Result<Self, SessionBindingStartError<OutputFactory::Binding>> {
        let context = SessionBindingContext::new(&identity, session_host_process, &continuity);
        let output = output_factory.bind(context).map_err(|error| {
            SessionBindingStartError::new(
                SessionBindingError::Factory {
                    kind: BindingKind::StructuredOutput,
                    error,
                },
                None,
            )
        })?;
        Self::validate_startup_output(&identity, output).map(|output| Self {
            presentation: SessionPresentation::new(identity),
            session_host_process,
            continuity,
            output_factory,
            terminal_factory,
            output: Some(output),
            terminal: None,
            pending_output_cleanup: Vec::new(),
            pending_terminal_cleanup: Vec::new(),
        })
    }

    /// Returns the immutable exact Core Session identity.
    pub fn identity(&self) -> &ExactSessionIdentity {
        self.presentation.identity()
    }

    /// Returns the independently resolved process hosting this exact Session.
    pub fn session_host_process(&self) -> SessionHostProcessIdentity {
        self.session_host_process
    }

    /// Returns the user's requested mode.
    pub fn requested_mode(&self) -> PresentationMode {
        self.presentation.requested_mode()
    }

    /// Returns the presentation that is currently effective.
    pub fn effective_mode(&self) -> PresentationMode {
        self.presentation.effective_mode()
    }

    /// Returns any actionable structured-output failure.
    pub fn structured_output_degradation(&self) -> Option<&StructuredOutputDegradation> {
        self.presentation.structured_output_degradation()
    }

    /// Returns the latest structured resume checkpoint.
    pub fn continuity(&self) -> &StructuredContinuity {
        &self.continuity
    }

    /// Records a renderer-reported checkpoint without changing the Session.
    pub fn update_continuity(&mut self, continuity: StructuredContinuity) {
        self.continuity = continuity;
    }

    /// Returns the active structured binding identity, if healthy.
    pub fn structured_binding_identity(&self) -> Option<&StructuredOutputBindingIdentity> {
        self.output.as_ref().map(StructuredOutputBinding::identity)
    }

    /// Returns the lazily attached terminal identity, if any.
    pub fn terminal_binding_identity(&self) -> Option<&TerminalBindingIdentity> {
        self.terminal.as_ref().map(TerminalBinding::identity)
    }

    /// Requests one presentation mode without replacing the Core Session.
    pub fn request_mode(
        &mut self,
        mode: PresentationMode,
    ) -> Result<SessionTransitionOutcome, SessionBindingError> {
        if mode == PresentationMode::Terminal {
            self.ensure_terminal()?;
        }
        self.presentation.switch_to(mode);
        Ok(SessionTransitionOutcome::Applied)
    }

    /// Falls back to a lazily attached same-Session Terminal.
    pub fn structured_failed(
        &mut self,
        actionable_diagnostic: impl Into<String>,
    ) -> Result<SessionTransitionOutcome, SessionBindingError> {
        let actionable_diagnostic = actionable_diagnostic.into();
        if actionable_diagnostic.trim().is_empty() {
            return Err(SessionBindingError::InvalidDegradationDiagnostic);
        }
        self.ensure_terminal()?;
        let cleanup_warning = self.retire_active_output();
        self.presentation
            .structured_unavailable(actionable_diagnostic)?;
        Ok(SessionTransitionOutcome::from_cleanup_warning(
            cleanup_warning,
        ))
    }

    /// Rebinds structured output from the preserved checkpoint.
    ///
    /// Clearing degradation does not overwrite explicit Terminal intent.
    pub fn retry_structured(&mut self) -> Result<SessionTransitionOutcome, SessionBindingError> {
        let context = SessionBindingContext::new(
            self.presentation.identity(),
            self.session_host_process,
            &self.continuity,
        );
        let output =
            self.output_factory
                .bind(context)
                .map_err(|error| SessionBindingError::Factory {
                    kind: BindingKind::StructuredOutput,
                    error,
                })?;
        let output = self.validate_output_candidate(output)?;
        let cleanup_warning = self.retire_active_output();
        self.output = Some(output);
        self.presentation.structured_restored();
        Ok(SessionTransitionOutcome::from_cleanup_warning(
            cleanup_warning,
        ))
    }

    /// Closes all presentation-owned bindings while leaving Core Session ownership external.
    pub fn shutdown(&mut self) -> Result<(), SessionBindingError> {
        let mut first_error = None;
        for warning in self.retry_pending_terminal_cleanup() {
            first_error.get_or_insert_with(|| Self::cleanup_error(warning));
        }
        for warning in self.retry_pending_output_cleanup() {
            first_error.get_or_insert_with(|| Self::cleanup_error(warning));
        }
        if let Some(warning) = self.retire_active_terminal() {
            first_error.get_or_insert_with(|| Self::cleanup_error(warning));
        }
        if let Some(warning) = self.retire_active_output() {
            first_error.get_or_insert_with(|| Self::cleanup_error(warning));
        }
        first_error.map_or(Ok(()), Err)
    }

    fn ensure_terminal(&mut self) -> Result<(), SessionBindingError> {
        if self.terminal.is_some() {
            return Ok(());
        }
        let context = SessionBindingContext::new(
            self.presentation.identity(),
            self.session_host_process,
            &self.continuity,
        );
        let terminal =
            self.terminal_factory
                .bind(context)
                .map_err(|error| SessionBindingError::Factory {
                    kind: BindingKind::Terminal,
                    error,
                })?;
        self.terminal = Some(self.validate_terminal_candidate(terminal)?);
        Ok(())
    }

    fn validate_startup_output(
        expected: &ExactSessionIdentity,
        mut binding: OutputFactory::Binding,
    ) -> Result<OutputFactory::Binding, SessionBindingStartError<OutputFactory::Binding>> {
        let actual = binding.identity().session().clone();
        if &actual == expected {
            return Ok(binding);
        }
        let cleanup_error = binding.close().err();
        let pending_output_cleanup = cleanup_error.is_some().then_some(binding);
        Err(SessionBindingStartError::new(
            SessionBindingError::SessionMismatch {
                kind: BindingKind::StructuredOutput,
                expected: expected.clone(),
                actual,
                cleanup_error,
            },
            pending_output_cleanup,
        ))
    }

    fn validate_output_candidate(
        &mut self,
        mut binding: OutputFactory::Binding,
    ) -> Result<OutputFactory::Binding, SessionBindingError> {
        let actual = binding.identity().session().clone();
        if &actual == self.presentation.identity() {
            return Ok(binding);
        }
        let cleanup_error = binding.close().err();
        if cleanup_error.is_some() {
            self.pending_output_cleanup.push(binding);
        }
        Err(SessionBindingError::SessionMismatch {
            kind: BindingKind::StructuredOutput,
            expected: self.presentation.identity().clone(),
            actual,
            cleanup_error,
        })
    }

    fn validate_terminal_candidate(
        &mut self,
        mut binding: TerminalFactory::Binding,
    ) -> Result<TerminalFactory::Binding, SessionBindingError> {
        let actual = binding.identity().session().clone();
        if &actual != self.presentation.identity() {
            let cleanup_error = binding.close().err();
            if cleanup_error.is_some() {
                self.pending_terminal_cleanup.push(binding);
            }
            return Err(SessionBindingError::SessionMismatch {
                kind: BindingKind::Terminal,
                expected: self.presentation.identity().clone(),
                actual,
                cleanup_error,
            });
        }
        let actual_process = binding.identity().process();
        if actual_process.get() == self.session_host_process.get() {
            return Ok(binding);
        }
        let cleanup_error = binding.close().err();
        if cleanup_error.is_some() {
            self.pending_terminal_cleanup.push(binding);
        }
        Err(SessionBindingError::TerminalProcessMismatch {
            expected: self.session_host_process,
            actual: actual_process,
            cleanup_error,
        })
    }

    fn retire_active_output(&mut self) -> Option<BindingCleanupWarning> {
        let mut output = self.output.take()?;
        match output.close() {
            Ok(()) => None,
            Err(error) => {
                self.pending_output_cleanup.push(output);
                Some(BindingCleanupWarning::new(
                    BindingKind::StructuredOutput,
                    error,
                ))
            }
        }
    }

    fn retire_active_terminal(&mut self) -> Option<BindingCleanupWarning> {
        let mut terminal = self.terminal.take()?;
        match terminal.close() {
            Ok(()) => None,
            Err(error) => {
                self.pending_terminal_cleanup.push(terminal);
                Some(BindingCleanupWarning::new(BindingKind::Terminal, error))
            }
        }
    }

    fn retry_pending_output_cleanup(&mut self) -> Vec<BindingCleanupWarning> {
        let pending = std::mem::take(&mut self.pending_output_cleanup);
        let mut warnings = Vec::new();
        for mut output in pending {
            if let Err(error) = output.close() {
                warnings.push(BindingCleanupWarning::new(
                    BindingKind::StructuredOutput,
                    error,
                ));
                self.pending_output_cleanup.push(output);
            }
        }
        warnings
    }

    fn retry_pending_terminal_cleanup(&mut self) -> Vec<BindingCleanupWarning> {
        let pending = std::mem::take(&mut self.pending_terminal_cleanup);
        let mut warnings = Vec::new();
        for mut terminal in pending {
            if let Err(error) = terminal.close() {
                warnings.push(BindingCleanupWarning::new(BindingKind::Terminal, error));
                self.pending_terminal_cleanup.push(terminal);
            }
        }
        warnings
    }

    fn cleanup_error(warning: BindingCleanupWarning) -> SessionBindingError {
        SessionBindingError::Cleanup {
            kind: warning.kind,
            error: warning.error,
        }
    }
}

impl<OutputFactory, TerminalFactory> Drop
    for SessionBindingController<OutputFactory, TerminalFactory>
where
    OutputFactory: StructuredOutputBindingFactory,
    TerminalFactory: TerminalBindingFactory,
{
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use super::*;

    #[derive(Clone, Debug, Eq, PartialEq)]
    struct FactoryCall {
        session: ExactSessionIdentity,
        session_host_process: SessionHostProcessIdentity,
        continuity: StructuredContinuity,
    }

    struct SpyOutputBinding {
        identity: StructuredOutputBindingIdentity,
        closes: Rc<RefCell<Vec<BindingKind>>>,
        close_error: Option<BindingCloseError>,
    }

    impl StructuredOutputBinding for SpyOutputBinding {
        fn identity(&self) -> &StructuredOutputBindingIdentity {
            &self.identity
        }

        fn close(&mut self) -> Result<(), BindingCloseError> {
            self.closes.borrow_mut().push(BindingKind::StructuredOutput);
            self.close_error.take().map_or(Ok(()), Err)
        }
    }

    struct SpyTerminalBinding {
        identity: TerminalBindingIdentity,
        closes: Rc<RefCell<Vec<BindingKind>>>,
        close_error: Option<BindingCloseError>,
    }

    impl TerminalBinding for SpyTerminalBinding {
        fn identity(&self) -> &TerminalBindingIdentity {
            &self.identity
        }

        fn close(&mut self) -> Result<(), BindingCloseError> {
            self.closes.borrow_mut().push(BindingKind::Terminal);
            self.close_error.take().map_or(Ok(()), Err)
        }
    }

    struct SpyOutputFactory {
        calls: Rc<RefCell<Vec<FactoryCall>>>,
        bindings: VecDeque<Result<SpyOutputBinding, BindingFactoryError>>,
    }

    impl StructuredOutputBindingFactory for SpyOutputFactory {
        type Binding = SpyOutputBinding;

        fn bind(
            &mut self,
            context: SessionBindingContext<'_>,
        ) -> Result<Self::Binding, BindingFactoryError> {
            self.calls.borrow_mut().push(FactoryCall {
                session: context.session().clone(),
                session_host_process: context.session_host_process(),
                continuity: context.continuity().clone(),
            });
            self.bindings.pop_front().expect("unexpected output bind")
        }
    }

    struct SpyTerminalFactory {
        calls: Rc<RefCell<Vec<FactoryCall>>>,
        bindings: VecDeque<Result<SpyTerminalBinding, BindingFactoryError>>,
    }

    impl TerminalBindingFactory for SpyTerminalFactory {
        type Binding = SpyTerminalBinding;

        fn bind(
            &mut self,
            context: SessionBindingContext<'_>,
        ) -> Result<Self::Binding, BindingFactoryError> {
            self.calls.borrow_mut().push(FactoryCall {
                session: context.session().clone(),
                session_host_process: context.session_host_process(),
                continuity: context.continuity().clone(),
            });
            self.bindings.pop_front().expect("unexpected terminal bind")
        }
    }

    type Controller = SessionBindingController<SpyOutputFactory, SpyTerminalFactory>;

    struct Fixture {
        identity: ExactSessionIdentity,
        session_host_process: SessionHostProcessIdentity,
        continuity: StructuredContinuity,
        output_calls: Rc<RefCell<Vec<FactoryCall>>>,
        terminal_calls: Rc<RefCell<Vec<FactoryCall>>>,
        closes: Rc<RefCell<Vec<BindingKind>>>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                identity: session("plot-7", "session-9"),
                session_host_process: SessionHostProcessIdentity::new(4312).unwrap(),
                continuity: continuity("cursor-3", "history-a"),
                output_calls: Rc::new(RefCell::new(Vec::new())),
                terminal_calls: Rc::new(RefCell::new(Vec::new())),
                closes: Rc::new(RefCell::new(Vec::new())),
            }
        }

        fn output(
            &self,
            identity: ExactSessionIdentity,
            _checkpoint: StructuredContinuity,
        ) -> SpyOutputBinding {
            SpyOutputBinding {
                identity: StructuredOutputBindingIdentity::new(
                    identity,
                    StructuredRuntimeIdentity::new("feed-runtime-1").unwrap(),
                ),
                closes: Rc::clone(&self.closes),
                close_error: None,
            }
        }

        fn terminal(&self, identity: ExactSessionIdentity) -> SpyTerminalBinding {
            self.terminal_with_process(identity, self.session_host_process.get())
        }

        fn terminal_with_process(
            &self,
            identity: ExactSessionIdentity,
            process_id: u32,
        ) -> SpyTerminalBinding {
            SpyTerminalBinding {
                identity: TerminalBindingIdentity::new(
                    identity,
                    TerminalProcessIdentity::new(process_id).unwrap(),
                    TerminalPaneIdentity::new("pane-main").unwrap(),
                ),
                closes: Rc::clone(&self.closes),
                close_error: None,
            }
        }

        fn start(
            &self,
            outputs: Vec<Result<SpyOutputBinding, BindingFactoryError>>,
            terminals: Vec<Result<SpyTerminalBinding, BindingFactoryError>>,
        ) -> Result<Controller, SessionBindingStartError<SpyOutputBinding>> {
            SessionBindingController::start(
                self.identity.clone(),
                self.session_host_process,
                self.continuity.clone(),
                SpyOutputFactory {
                    calls: Rc::clone(&self.output_calls),
                    bindings: outputs.into(),
                },
                SpyTerminalFactory {
                    calls: Rc::clone(&self.terminal_calls),
                    bindings: terminals.into(),
                },
            )
        }
    }

    fn session(plot: &str, session: &str) -> ExactSessionIdentity {
        ExactSessionIdentity::new(plot, session).unwrap()
    }

    fn continuity(cursor: &str, history: &str) -> StructuredContinuity {
        StructuredContinuity::new(
            StructuredCursor::new(cursor).unwrap(),
            StructuredHistoryToken::new(history).unwrap(),
        )
    }

    #[test]
    fn healthy_startup_binds_structured_output_without_creating_terminal() {
        let fixture = Fixture::new();
        let controller = fixture
            .start(
                vec![Ok(fixture.output(
                    fixture.identity.clone(),
                    fixture.continuity.clone(),
                ))],
                Vec::new(),
            )
            .unwrap();

        assert_eq!(
            controller.effective_mode(),
            PresentationMode::StructuredOutput
        );
        let output = controller.structured_binding_identity().unwrap();
        assert_eq!(output.session(), &fixture.identity);
        assert_eq!(output.runtime().as_str(), "feed-runtime-1");
        assert!(controller.terminal_binding_identity().is_none());
        assert_eq!(fixture.output_calls.borrow().len(), 1);
        assert!(fixture.terminal_calls.borrow().is_empty());
    }

    #[test]
    fn explicit_terminal_request_attaches_once_and_exposes_typed_identifiers() {
        let fixture = Fixture::new();
        let mut controller = fixture
            .start(
                vec![Ok(fixture.output(
                    fixture.identity.clone(),
                    fixture.continuity.clone(),
                ))],
                vec![Ok(fixture.terminal(fixture.identity.clone()))],
            )
            .unwrap();

        controller.request_mode(PresentationMode::Terminal).unwrap();
        controller.request_mode(PresentationMode::Terminal).unwrap();

        let terminal = controller.terminal_binding_identity().unwrap();
        assert_eq!(terminal.session(), &fixture.identity);
        assert_eq!(terminal.process().get(), 4312);
        assert_eq!(terminal.pane().as_str(), "pane-main");
        assert_eq!(fixture.terminal_calls.borrow().len(), 1);
        assert_eq!(controller.effective_mode(), PresentationMode::Terminal);
    }

    #[test]
    fn terminal_process_must_match_independently_supplied_session_host() {
        let fixture = Fixture::new();
        let wrong_process = TerminalProcessIdentity::new(9999).unwrap();
        let mut controller = fixture
            .start(
                vec![Ok(fixture.output(
                    fixture.identity.clone(),
                    fixture.continuity.clone(),
                ))],
                vec![Ok(fixture.terminal_with_process(
                    fixture.identity.clone(),
                    wrong_process.get(),
                ))],
            )
            .unwrap();

        assert_eq!(
            controller.request_mode(PresentationMode::Terminal),
            Err(SessionBindingError::TerminalProcessMismatch {
                expected: fixture.session_host_process,
                actual: wrong_process,
                cleanup_error: None,
            })
        );
        assert_eq!(
            controller.requested_mode(),
            PresentationMode::StructuredOutput
        );
        assert_eq!(
            controller.effective_mode(),
            PresentationMode::StructuredOutput
        );
        assert!(controller.terminal_binding_identity().is_none());
        assert_eq!(*fixture.closes.borrow(), vec![BindingKind::Terminal]);
    }

    #[test]
    fn terminal_process_mismatch_with_failed_cleanup_remains_owned_for_shutdown_retry() {
        let fixture = Fixture::new();
        let wrong_process = TerminalProcessIdentity::new(9999).unwrap();
        let mut terminal =
            fixture.terminal_with_process(fixture.identity.clone(), wrong_process.get());
        terminal.close_error = Some(BindingCloseError::new("first terminal close failed"));
        let mut controller = fixture
            .start(
                vec![Ok(fixture.output(
                    fixture.identity.clone(),
                    fixture.continuity.clone(),
                ))],
                vec![Ok(terminal)],
            )
            .unwrap();

        assert_eq!(
            controller.request_mode(PresentationMode::Terminal),
            Err(SessionBindingError::TerminalProcessMismatch {
                expected: fixture.session_host_process,
                actual: wrong_process,
                cleanup_error: Some(BindingCloseError::new("first terminal close failed")),
            })
        );
        assert_eq!(controller.shutdown(), Ok(()));
        assert_eq!(
            *fixture.closes.borrow(),
            vec![
                BindingKind::Terminal,
                BindingKind::Terminal,
                BindingKind::StructuredOutput,
            ]
        );
    }

    #[test]
    fn structured_failure_lazily_attaches_same_session_terminal_with_diagnostic() {
        let fixture = Fixture::new();
        let checkpoint = continuity("cursor-44", "history-b");
        let mut controller = fixture
            .start(
                vec![Ok(
                    fixture.output(fixture.identity.clone(), checkpoint.clone())
                )],
                vec![Ok(fixture.terminal(fixture.identity.clone()))],
            )
            .unwrap();
        controller.update_continuity(checkpoint.clone());

        controller
            .structured_failed("activity stream disconnected; select Retry")
            .unwrap();

        assert_eq!(controller.identity(), &fixture.identity);
        assert_eq!(controller.effective_mode(), PresentationMode::Terminal);
        assert_eq!(controller.continuity(), &checkpoint);
        assert_eq!(
            controller
                .structured_output_degradation()
                .unwrap()
                .diagnostic(),
            "activity stream disconnected; select Retry"
        );
        assert!(controller.structured_binding_identity().is_none());
        assert_eq!(fixture.terminal_calls.borrow().len(), 1);
    }

    #[test]
    fn degradation_applies_with_typed_warning_and_retains_failed_output_cleanup() {
        let fixture = Fixture::new();
        let mut output = fixture.output(fixture.identity.clone(), fixture.continuity.clone());
        output.close_error = Some(BindingCloseError::new("first output close failed"));
        let mut controller = fixture
            .start(
                vec![Ok(output)],
                vec![Ok(fixture.terminal(fixture.identity.clone()))],
            )
            .unwrap();

        assert_eq!(
            controller.structured_failed("feed unavailable; retry"),
            Ok(SessionTransitionOutcome::AppliedWithCleanupWarning(
                BindingCleanupWarning::new(
                    BindingKind::StructuredOutput,
                    BindingCloseError::new("first output close failed"),
                ),
            ))
        );
        assert_eq!(controller.effective_mode(), PresentationMode::Terminal);
        assert!(controller.structured_binding_identity().is_none());

        assert_eq!(controller.shutdown(), Ok(()));
        assert_eq!(
            *fixture.closes.borrow(),
            vec![
                BindingKind::StructuredOutput,
                BindingKind::StructuredOutput,
                BindingKind::Terminal,
            ]
        );
    }

    #[test]
    fn shutdown_retains_failed_active_binding_and_succeeds_on_second_call() {
        let fixture = Fixture::new();
        let mut output = fixture.output(fixture.identity.clone(), fixture.continuity.clone());
        output.close_error = Some(BindingCloseError::new("first output close failed"));
        let mut controller = fixture.start(vec![Ok(output)], Vec::new()).unwrap();

        assert_eq!(
            controller.shutdown(),
            Err(SessionBindingError::Cleanup {
                kind: BindingKind::StructuredOutput,
                error: BindingCloseError::new("first output close failed"),
            })
        );
        assert_eq!(controller.shutdown(), Ok(()));
        assert_eq!(
            *fixture.closes.borrow(),
            vec![BindingKind::StructuredOutput, BindingKind::StructuredOutput,]
        );
    }

    #[test]
    fn retry_rebinds_output_with_cursor_and_history_only() {
        let fixture = Fixture::new();
        let checkpoint = continuity("cursor-44", "history-b");
        let replacement = fixture.output(fixture.identity.clone(), checkpoint.clone());
        let mut controller = fixture
            .start(
                vec![
                    Ok(fixture.output(fixture.identity.clone(), checkpoint.clone())),
                    Ok(replacement),
                ],
                vec![Ok(fixture.terminal(fixture.identity.clone()))],
            )
            .unwrap();
        controller.update_continuity(checkpoint.clone());
        controller
            .structured_failed("feed unavailable; retry")
            .unwrap();

        controller.retry_structured().unwrap();

        assert_eq!(
            controller.effective_mode(),
            PresentationMode::StructuredOutput
        );
        assert_eq!(controller.identity(), &fixture.identity);
        let retry = &fixture.output_calls.borrow()[1];
        assert_eq!(retry.session, fixture.identity);
        assert_eq!(retry.session_host_process, fixture.session_host_process);
        assert_eq!(retry.continuity, checkpoint);
    }

    #[test]
    fn controller_continuity_update_cannot_be_overwritten_by_stale_output_state() {
        let fixture = Fixture::new();
        let stale = continuity("cursor-3", "history-a");
        let latest = continuity("cursor-99", "history-latest");
        let mut controller = fixture
            .start(
                vec![
                    Ok(fixture.output(fixture.identity.clone(), stale)),
                    Ok(fixture.output(fixture.identity.clone(), latest.clone())),
                ],
                vec![Ok(fixture.terminal(fixture.identity.clone()))],
            )
            .unwrap();
        controller.update_continuity(latest.clone());

        controller
            .structured_failed("feed unavailable; retry")
            .unwrap();
        controller.retry_structured().unwrap();

        assert_eq!(controller.continuity(), &latest);
        assert_eq!(fixture.output_calls.borrow()[1].continuity, latest);
    }

    #[test]
    fn successful_rebind_reports_retained_old_output_as_an_applied_warning() {
        let fixture = Fixture::new();
        let mut original = fixture.output(fixture.identity.clone(), fixture.continuity.clone());
        original.close_error = Some(BindingCloseError::new("old output still stopping"));
        let replacement = fixture.output(fixture.identity.clone(), fixture.continuity.clone());
        let mut controller = fixture
            .start(vec![Ok(original), Ok(replacement)], Vec::new())
            .unwrap();

        assert_eq!(
            controller.retry_structured(),
            Ok(SessionTransitionOutcome::AppliedWithCleanupWarning(
                BindingCleanupWarning::new(
                    BindingKind::StructuredOutput,
                    BindingCloseError::new("old output still stopping"),
                ),
            ))
        );
        assert_eq!(
            controller.effective_mode(),
            PresentationMode::StructuredOutput
        );
        assert!(controller.structured_binding_identity().is_some());
        assert_eq!(controller.shutdown(), Ok(()));
    }

    #[test]
    fn explicit_terminal_intent_survives_structured_recovery() {
        let fixture = Fixture::new();
        let mut controller = fixture
            .start(
                vec![
                    Ok(fixture.output(fixture.identity.clone(), fixture.continuity.clone())),
                    Ok(fixture.output(fixture.identity.clone(), fixture.continuity.clone())),
                ],
                vec![Ok(fixture.terminal(fixture.identity.clone()))],
            )
            .unwrap();
        controller.request_mode(PresentationMode::Terminal).unwrap();
        controller
            .structured_failed("feed unavailable; retry")
            .unwrap();

        controller.retry_structured().unwrap();

        assert_eq!(controller.requested_mode(), PresentationMode::Terminal);
        assert_eq!(controller.effective_mode(), PresentationMode::Terminal);
        assert!(controller.structured_output_degradation().is_none());
        assert!(controller.structured_binding_identity().is_some());
    }

    #[test]
    fn requesting_output_during_outage_waits_for_successful_retry() {
        let fixture = Fixture::new();
        let mut controller = fixture
            .start(
                vec![
                    Ok(fixture.output(fixture.identity.clone(), fixture.continuity.clone())),
                    Ok(fixture.output(fixture.identity.clone(), fixture.continuity.clone())),
                ],
                vec![Ok(fixture.terminal(fixture.identity.clone()))],
            )
            .unwrap();
        controller
            .structured_failed("feed unavailable; retry")
            .unwrap();

        controller
            .request_mode(PresentationMode::StructuredOutput)
            .unwrap();
        assert_eq!(
            controller.requested_mode(),
            PresentationMode::StructuredOutput
        );
        assert_eq!(controller.effective_mode(), PresentationMode::Terminal);

        controller.retry_structured().unwrap();
        assert_eq!(
            controller.effective_mode(),
            PresentationMode::StructuredOutput
        );
    }

    #[test]
    fn mismatched_terminal_is_rejected_cleaned_and_never_becomes_effective() {
        let fixture = Fixture::new();
        let wrong = session("plot-7", "session-other");
        let mut controller = fixture
            .start(
                vec![Ok(fixture.output(
                    fixture.identity.clone(),
                    fixture.continuity.clone(),
                ))],
                vec![Ok(fixture.terminal(wrong.clone()))],
            )
            .unwrap();

        let result = controller.request_mode(PresentationMode::Terminal);

        assert_eq!(
            result,
            Err(SessionBindingError::SessionMismatch {
                kind: BindingKind::Terminal,
                expected: fixture.identity.clone(),
                actual: wrong,
                cleanup_error: None,
            })
        );
        assert_eq!(
            controller.effective_mode(),
            PresentationMode::StructuredOutput
        );
        assert!(controller.terminal_binding_identity().is_none());
        assert_eq!(*fixture.closes.borrow(), vec![BindingKind::Terminal]);
    }

    #[test]
    fn mismatched_startup_output_is_cleaned_and_controller_is_not_created() {
        let fixture = Fixture::new();
        let wrong = session("plot-other", "session-other");

        let result = fixture.start(
            vec![Ok(fixture.output(wrong.clone(), fixture.continuity.clone()))],
            Vec::new(),
        );

        assert!(matches!(
            result
                .as_ref()
                .map_err(SessionBindingStartError::binding_error),
            Err(SessionBindingError::SessionMismatch {
                kind: BindingKind::StructuredOutput,
                actual,
                cleanup_error: None,
                ..
            }) if actual == &wrong
        ));
        assert_eq!(
            *fixture.closes.borrow(),
            vec![BindingKind::StructuredOutput]
        );
        assert!(fixture.terminal_calls.borrow().is_empty());
    }

    #[test]
    fn mismatched_startup_output_retains_failed_cleanup_for_explicit_retry() {
        let fixture = Fixture::new();
        let wrong = session("plot-other", "session-other");
        let output = SpyOutputBinding {
            identity: StructuredOutputBindingIdentity::new(
                wrong.clone(),
                StructuredRuntimeIdentity::new("feed-runtime-rejected").unwrap(),
            ),
            closes: Rc::clone(&fixture.closes),
            close_error: Some(BindingCloseError::new("renderer still draining")),
        };

        let mut error = match fixture.start(vec![Ok(output)], Vec::new()) {
            Ok(_) => panic!("mismatched startup output must be rejected"),
            Err(error) => error,
        };

        assert!(matches!(
            error.binding_error(),
            SessionBindingError::SessionMismatch {
                kind: BindingKind::StructuredOutput,
                actual,
                cleanup_error: Some(cleanup_error),
                ..
            } if actual == &wrong && cleanup_error.message() == "renderer still draining"
        ));
        assert!(error.has_pending_cleanup());
        assert_eq!(
            *fixture.closes.borrow(),
            vec![BindingKind::StructuredOutput]
        );

        assert_eq!(error.retry_cleanup(), Ok(()));
        assert!(!error.has_pending_cleanup());
        assert_eq!(
            *fixture.closes.borrow(),
            vec![BindingKind::StructuredOutput, BindingKind::StructuredOutput]
        );
    }

    #[test]
    fn terminal_factory_failure_does_not_claim_terminal_is_effective() {
        let fixture = Fixture::new();
        let checkpoint = continuity("cursor-44", "history-b");
        let mut controller = fixture
            .start(
                vec![Ok(
                    fixture.output(fixture.identity.clone(), checkpoint.clone())
                )],
                vec![Err(BindingFactoryError::new("pty unavailable"))],
            )
            .unwrap();
        controller.update_continuity(checkpoint.clone());

        assert_eq!(
            controller.structured_failed("feed unavailable; retry"),
            Err(SessionBindingError::Factory {
                kind: BindingKind::Terminal,
                error: BindingFactoryError::new("pty unavailable"),
            })
        );
        assert_eq!(controller.continuity(), &checkpoint);
        assert_eq!(
            controller.effective_mode(),
            PresentationMode::StructuredOutput
        );
        assert!(controller.structured_output_degradation().is_none());
        assert!(controller.structured_binding_identity().is_some());
    }

    #[test]
    fn mismatched_rebound_output_is_rejected_and_degradation_remains_visible() {
        let fixture = Fixture::new();
        let wrong = session("plot-other", "session-9");
        let mut controller = fixture
            .start(
                vec![
                    Ok(fixture.output(fixture.identity.clone(), fixture.continuity.clone())),
                    Ok(fixture.output(wrong.clone(), fixture.continuity.clone())),
                ],
                vec![Ok(fixture.terminal(fixture.identity.clone()))],
            )
            .unwrap();
        controller
            .structured_failed("feed unavailable; retry")
            .unwrap();

        let result = controller.retry_structured();

        assert!(matches!(
            result,
            Err(SessionBindingError::SessionMismatch {
                kind: BindingKind::StructuredOutput,
                actual,
                ..
            }) if actual == wrong
        ));
        assert_eq!(controller.effective_mode(), PresentationMode::Terminal);
        assert!(controller.structured_binding_identity().is_none());
        assert_eq!(
            *fixture.closes.borrow(),
            vec![BindingKind::StructuredOutput, BindingKind::StructuredOutput]
        );
    }

    #[test]
    fn rejected_binding_reports_cleanup_failure_without_adopting_it() {
        let fixture = Fixture::new();
        let wrong = session("plot-other", "session-other");
        let mut terminal = fixture.terminal(wrong.clone());
        terminal.close_error = Some(BindingCloseError::new("kill failed"));
        let mut controller = fixture
            .start(
                vec![Ok(fixture.output(
                    fixture.identity.clone(),
                    fixture.continuity.clone(),
                ))],
                vec![Ok(terminal)],
            )
            .unwrap();

        assert_eq!(
            controller.request_mode(PresentationMode::Terminal),
            Err(SessionBindingError::SessionMismatch {
                kind: BindingKind::Terminal,
                expected: fixture.identity.clone(),
                actual: wrong,
                cleanup_error: Some(BindingCloseError::new("kill failed")),
            })
        );
        assert!(controller.terminal_binding_identity().is_none());
    }

    #[test]
    fn blank_failure_diagnostic_does_not_create_terminal_or_change_mode() {
        let fixture = Fixture::new();
        let mut controller = fixture
            .start(
                vec![Ok(fixture.output(
                    fixture.identity.clone(),
                    fixture.continuity.clone(),
                ))],
                Vec::new(),
            )
            .unwrap();

        assert_eq!(
            controller.structured_failed(" \n "),
            Err(SessionBindingError::InvalidDegradationDiagnostic)
        );
        assert!(fixture.terminal_calls.borrow().is_empty());
        assert_eq!(
            controller.effective_mode(),
            PresentationMode::StructuredOutput
        );
    }

    #[test]
    fn all_factory_calls_receive_one_immutable_core_session_identity() {
        let fixture = Fixture::new();
        let mut controller = fixture
            .start(
                vec![
                    Ok(fixture.output(fixture.identity.clone(), fixture.continuity.clone())),
                    Ok(fixture.output(fixture.identity.clone(), fixture.continuity.clone())),
                ],
                vec![Ok(fixture.terminal(fixture.identity.clone()))],
            )
            .unwrap();
        controller.request_mode(PresentationMode::Terminal).unwrap();
        controller
            .request_mode(PresentationMode::StructuredOutput)
            .unwrap();
        controller
            .structured_failed("feed unavailable; retry")
            .unwrap();
        controller.retry_structured().unwrap();

        assert!(
            fixture
                .output_calls
                .borrow()
                .iter()
                .all(|call| call.session == fixture.identity)
        );
        assert!(
            fixture
                .terminal_calls
                .borrow()
                .iter()
                .all(|call| call.session == fixture.identity)
        );
        assert_eq!(controller.identity(), &fixture.identity);
    }
}
