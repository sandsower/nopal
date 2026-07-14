//! Per-Session Composer authority and typed submission contract.

mod authority;
mod model;

use std::fmt;

use nopal_feed_client::session::MAX_SESSION_IDENTITY_BYTES;

pub use authority::ComposerAuthority;
pub use model::ComposerDraft;

/// The exact Plot and Session receiving structured Composer submissions.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub struct ComposerTarget {
    plot_id: String,
    session_id: String,
}

impl ComposerTarget {
    /// Validates one stable Core target.
    pub fn new(
        plot_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<Self, ComposerTargetError> {
        let plot_id = plot_id.into();
        validate_target_identity(&plot_id).map_err(|()| ComposerTargetError::InvalidPlotId)?;
        let session_id = session_id.into();
        validate_target_identity(&session_id)
            .map_err(|()| ComposerTargetError::InvalidSessionId)?;
        Ok(Self {
            plot_id,
            session_id,
        })
    }

    /// Returns the stable Plot identity.
    pub fn plot_id(&self) -> &str {
        &self.plot_id
    }

    /// Returns the stable Session identity.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }
}

/// Why a Composer target could not be created.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComposerTargetError {
    /// The Plot identity was blank, unsafe, or too large.
    InvalidPlotId,
    /// The Session identity was blank, unsafe, or too large.
    InvalidSessionId,
}

impl fmt::Display for ComposerTargetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPlotId => formatter.write_str("Composer target Plot identity is invalid"),
            Self::InvalidSessionId => {
                formatter.write_str("Composer target Session identity is invalid")
            }
        }
    }
}

impl std::error::Error for ComposerTargetError {}

fn validate_target_identity(value: &str) -> Result<(), ()> {
    if value.trim().is_empty()
        || value.len() > MAX_SESSION_IDENTITY_BYTES
        || value.chars().any(char::is_control)
    {
        Err(())
    } else {
        Ok(())
    }
}

/// An opaque monotonic mutation revision scoped to one target draft.
#[derive(Clone, Copy, Debug, Default, Eq, Ord, PartialEq, PartialOrd)]
pub struct ComposerRevision(u64);

impl ComposerRevision {
    /// Returns the next revision without wrapping.
    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

/// The exact immutable text submitted to a structured Session feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ComposerSubmission {
    text: String,
}

impl ComposerSubmission {
    fn new(text: String) -> Self {
        Self { text }
    }

    /// Returns the exact visible text without trimming or normalization.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// A rejected immutable submission snapshot retained beside the editable draft.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RejectedSubmission {
    target: ComposerTarget,
    revision: ComposerRevision,
    submission: ComposerSubmission,
    reason: String,
}

impl RejectedSubmission {
    /// Returns the original target.
    pub fn target(&self) -> &ComposerTarget {
        &self.target
    }

    /// Returns the original draft revision.
    pub fn revision(&self) -> ComposerRevision {
        self.revision
    }

    /// Returns the exact original submission.
    pub fn submission(&self) -> &ComposerSubmission {
        &self.submission
    }

    /// Returns the submission diagnostic.
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// A renderer-neutral intent that must be routed only to the structured Session feed.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ComposerIntent {
    /// Submit one immutable snapshot for the exact target and revision.
    Submit {
        /// The exact Core target.
        target: ComposerTarget,
        /// The revision being submitted.
        revision: ComposerRevision,
        /// The exact submission payload.
        submission: ComposerSubmission,
    },
}

/// The outcome of routing an earlier Composer intent.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SubmissionResolution {
    /// The structured Session feed accepted the command.
    Sent {
        /// The original exact target.
        target: ComposerTarget,
        /// The original revision.
        revision: ComposerRevision,
        /// The durable Session command identity.
        command_id: String,
    },
    /// The structured Session feed rejected the command.
    Rejected {
        /// The original exact target.
        target: ComposerTarget,
        /// The original revision.
        revision: ComposerRevision,
        /// The actionable diagnostic.
        reason: String,
    },
}
