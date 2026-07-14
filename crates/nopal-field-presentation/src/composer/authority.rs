use std::collections::{HashMap, HashSet};

use super::{
    ComposerDraft, ComposerIntent, ComposerRevision, ComposerSubmission, ComposerTarget,
    RejectedSubmission, SubmissionResolution,
};

#[derive(Clone, Debug, Eq, PartialEq)]
struct PendingSubmission {
    revision: ComposerRevision,
    submission: ComposerSubmission,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
struct TargetDraft {
    editor: ComposerDraft,
    revision: ComposerRevision,
    pending: Option<PendingSubmission>,
    diagnostic: Option<String>,
    rejected: Option<RejectedSubmission>,
}

/// Sole process-local authority for exact per-Session Composer drafts.
///
/// Renderers may mutate only the active draft through this type, so every
/// semantic mutation advances its target-scoped revision and late submission
/// outcomes cannot clear a newer draft or affect another Session.
#[derive(Debug, Default)]
pub struct ComposerAuthority {
    active: Option<ComposerTarget>,
    drafts: HashMap<ComposerTarget, TargetDraft>,
}

impl ComposerAuthority {
    /// Creates an authority and, when present, an empty draft for the initial target.
    pub fn new(active: Option<ComposerTarget>) -> Self {
        let mut authority = Self::default();
        authority.retarget(active);
        authority
    }

    /// Activates one target without copying draft state between Sessions.
    ///
    /// Any marked text on the previous target is committed before switching.
    pub fn retarget(&mut self, target: Option<ComposerTarget>) {
        if self.active != target {
            self.edit_active(ComposerDraft::unmark);
        }
        if let Some(target) = &target {
            self.drafts.entry(target.clone()).or_default();
        }
        self.active = target;
    }

    /// Retains drafts only for targets that remain present in the latest Core snapshot.
    pub fn retain_targets(&mut self, targets: impl IntoIterator<Item = ComposerTarget>) {
        let targets = targets.into_iter().collect::<HashSet<_>>();
        self.drafts.retain(|target, _| targets.contains(target));
        if self
            .active
            .as_ref()
            .is_some_and(|active| !targets.contains(active))
        {
            self.active = None;
        }
    }

    /// Returns the currently active exact target.
    pub fn active_target(&self) -> Option<&ComposerTarget> {
        self.active.as_ref()
    }

    /// Returns the current target's immutable draft view.
    pub fn active_draft(&self) -> Option<&ComposerDraft> {
        self.active
            .as_ref()
            .and_then(|target| self.drafts.get(target))
            .map(|draft| &draft.editor)
    }

    /// Returns the current target's opaque revision.
    pub fn active_revision(&self) -> Option<ComposerRevision> {
        self.active
            .as_ref()
            .and_then(|target| self.drafts.get(target))
            .map(|draft| draft.revision)
    }

    /// Applies one renderer-neutral edit and advances the revision only on mutation.
    pub fn edit_active<R>(&mut self, edit: impl FnOnce(&mut ComposerDraft) -> R) -> Option<R> {
        let target = self.active.clone()?;
        let draft = self.drafts.get_mut(&target)?;
        let before = draft.editor.mutation_generation();
        let result = edit(&mut draft.editor);
        if draft.editor.mutation_generation() != before {
            draft.revision = draft.revision.next();
            draft.diagnostic = None;
        }
        Some(result)
    }

    /// Replaces the active selection with exact text.
    ///
    /// Returns false only when there is no active target.
    pub fn replace_active(&mut self, text: &str) -> bool {
        self.edit_active(|editor| editor.replace(None, text))
            .is_some()
    }

    /// Returns whether the active target has one unresolved submission.
    pub fn is_pending(&self) -> bool {
        self.active
            .as_ref()
            .and_then(|target| self.drafts.get(target))
            .is_some_and(|draft| draft.pending.is_some())
    }

    /// Returns the active target's latest rejection diagnostic.
    pub fn diagnostic(&self) -> Option<&str> {
        self.active
            .as_ref()
            .and_then(|target| self.drafts.get(target))
            .and_then(|draft| draft.diagnostic.as_deref())
    }

    /// Returns the active target's retained rejected submission snapshot.
    pub fn rejected_submission(&self) -> Option<&RejectedSubmission> {
        self.active
            .as_ref()
            .and_then(|target| self.drafts.get(target))
            .and_then(|draft| draft.rejected.as_ref())
    }

    /// Creates an immutable intent from the active exact draft.
    ///
    /// Blank drafts and targets with an unresolved submission produce no intent.
    pub fn prepare_submission(&mut self) -> Option<ComposerIntent> {
        let target = self.active.clone()?;
        let draft = self.drafts.get_mut(&target)?;
        if draft.pending.is_some() || draft.editor.text().trim().is_empty() {
            return None;
        }
        let submission = ComposerSubmission::new(draft.editor.text().to_owned());
        let revision = draft.revision;
        draft.pending = Some(PendingSubmission {
            revision,
            submission: submission.clone(),
        });
        Some(ComposerIntent::Submit {
            target,
            revision,
            submission,
        })
    }

    /// Retries the exact rejected snapshot without replacing newer editable text.
    pub fn retry_rejected(&mut self) -> Option<ComposerIntent> {
        let target = self.active.clone()?;
        let draft = self.drafts.get_mut(&target)?;
        if draft.pending.is_some() {
            return None;
        }
        let rejected = draft.rejected.as_ref()?;
        let revision = rejected.revision();
        let submission = rejected.submission().clone();
        draft.pending = Some(PendingSubmission {
            revision,
            submission: submission.clone(),
        });
        draft.diagnostic = None;
        Some(ComposerIntent::Submit {
            target,
            revision,
            submission,
        })
    }

    /// Dismisses the active rejected snapshot and its diagnostic.
    pub fn dismiss_rejected(&mut self) -> bool {
        let Some(target) = self.active.as_ref() else {
            return false;
        };
        let Some(draft) = self.drafts.get_mut(target) else {
            return false;
        };
        let dismissed = draft.rejected.take().is_some();
        if dismissed {
            draft.diagnostic = None;
        }
        dismissed
    }

    /// Applies a resolution only to its matching target and pending revision.
    ///
    /// A matching success clears text only when that text and revision remain
    /// unchanged. A matching rejection retains the immutable submitted snapshot
    /// beside any newer editable text.
    pub fn resolve(&mut self, resolution: SubmissionResolution) -> bool {
        let (target, revision, outcome) = match resolution {
            SubmissionResolution::Sent {
                target,
                revision,
                command_id: _,
            } => (target, revision, Ok(())),
            SubmissionResolution::Rejected {
                target,
                revision,
                reason,
            } => (target, revision, Err(reason)),
        };
        let Some(draft) = self.drafts.get_mut(&target) else {
            return false;
        };
        if draft.pending.as_ref().map(|pending| pending.revision) != Some(revision) {
            return false;
        }
        let Some(pending) = draft.pending.take() else {
            return false;
        };
        match outcome {
            Ok(()) => {
                if draft.revision == revision && draft.editor.text() == pending.submission.text() {
                    draft.editor = ComposerDraft::default();
                    draft.revision = draft.revision.next();
                }
                draft.diagnostic = None;
                if draft
                    .rejected
                    .as_ref()
                    .is_some_and(|rejected| rejected.revision() <= revision)
                {
                    draft.rejected = None;
                }
            }
            Err(reason) => {
                draft.diagnostic = Some(reason.clone());
                draft.rejected = Some(RejectedSubmission {
                    target,
                    revision,
                    submission: pending.submission,
                    reason,
                });
            }
        }
        true
    }
}
