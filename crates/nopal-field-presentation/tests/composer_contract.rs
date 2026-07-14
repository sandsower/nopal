use nopal_feed_client::session::MAX_SESSION_IDENTITY_BYTES;
use nopal_field_presentation::composer::{
    ComposerAuthority, ComposerIntent, ComposerTarget, ComposerTargetError, SubmissionResolution,
};

fn target(name: &str) -> ComposerTarget {
    ComposerTarget::new(format!("plot-{name}"), format!("session-{name}"))
        .unwrap_or_else(|error| panic!("valid target: {error}"))
}

fn submission(
    authority: &mut ComposerAuthority,
) -> (
    ComposerTarget,
    nopal_field_presentation::composer::ComposerRevision,
    String,
) {
    let ComposerIntent::Submit {
        target,
        revision,
        submission,
    } = authority
        .prepare_submission()
        .unwrap_or_else(|| panic!("submission should be ready"));
    (target, revision, submission.text().to_owned())
}

#[test]
fn target_rejects_blank_control_and_oversized_identities() {
    assert_eq!(
        ComposerTarget::new("", "session-a"),
        Err(ComposerTargetError::InvalidPlotId)
    );
    assert_eq!(
        ComposerTarget::new("plot-a", " \n"),
        Err(ComposerTargetError::InvalidSessionId)
    );
    assert_eq!(
        ComposerTarget::new("plot\u{7f}", "session-a"),
        Err(ComposerTargetError::InvalidPlotId)
    );
    assert_eq!(
        ComposerTarget::new("p".repeat(MAX_SESSION_IDENTITY_BYTES + 1), "session-a"),
        Err(ComposerTargetError::InvalidPlotId)
    );
    assert!(ComposerTarget::new("p".repeat(MAX_SESSION_IDENTITY_BYTES), "session-a").is_ok());
}

#[test]
fn retargeting_restores_exact_per_session_text_and_selection() {
    let a = target("a");
    let b = target("b");
    let mut authority = ComposerAuthority::new(Some(a.clone()));

    assert!(authority.replace_active("  draft a\n"));
    assert!(
        authority
            .edit_active(|draft| draft.move_left(true, false))
            .is_some()
    );
    let a_selection = authority
        .active_draft()
        .unwrap_or_else(|| panic!("active draft"))
        .selection();
    authority.retarget(Some(b.clone()));
    assert!(authority.replace_active("draft b"));
    authority.retarget(Some(a.clone()));

    let restored = authority
        .active_draft()
        .unwrap_or_else(|| panic!("restored draft"));
    assert_eq!(authority.active_target(), Some(&a));
    assert_eq!(restored.text(), "  draft a\n");
    assert_eq!(restored.selection(), a_selection);
    authority.retarget(Some(b));
    assert_eq!(
        authority.active_draft().map(|draft| draft.text()),
        Some("draft b")
    );
}

#[test]
fn retargeting_commits_marked_text_without_copying_it_to_another_session() {
    let a = target("a");
    let b = target("b");
    let mut authority = ComposerAuthority::new(Some(a.clone()));
    assert!(
        authority
            .edit_active(|draft| draft.replace_and_mark(None, "ñ"))
            .is_some()
    );
    assert_eq!(
        authority
            .active_draft()
            .and_then(|draft| draft.marked_range()),
        Some(0..2)
    );

    authority.retarget(Some(b));
    assert_eq!(authority.active_draft().map(|draft| draft.text()), Some(""));
    authority.retarget(Some(a));
    let restored = authority
        .active_draft()
        .unwrap_or_else(|| panic!("restored marked-text draft"));
    assert_eq!(restored.text(), "ñ");
    assert_eq!(restored.marked_range(), None);
}

#[test]
fn exact_submission_clears_only_the_matching_unchanged_revision() {
    let a = target("a");
    let mut authority = ComposerAuthority::new(Some(a.clone()));
    assert!(authority.replace_active("  exact\n"));

    let (submitted_target, revision, text) = submission(&mut authority);
    assert_eq!(submitted_target, a.clone());
    assert_eq!(text, "  exact\n");
    assert!(authority.is_pending());
    assert!(authority.prepare_submission().is_none());
    assert!(authority.resolve(SubmissionResolution::Sent {
        target: a,
        revision,
        command_id: "command-01".to_owned(),
    }));

    assert_eq!(authority.active_draft().map(|draft| draft.text()), Some(""));
    assert!(!authority.is_pending());
}

#[test]
fn late_success_preserves_newer_edits_and_another_active_target() {
    let a = target("a");
    let b = target("b");
    let mut authority = ComposerAuthority::new(Some(a.clone()));
    assert!(authority.replace_active("first"));
    let (_, revision, _) = submission(&mut authority);
    assert!(authority.replace_active(" plus newer"));
    authority.retarget(Some(b.clone()));
    assert!(authority.replace_active("draft b"));

    assert!(authority.resolve(SubmissionResolution::Sent {
        target: a.clone(),
        revision,
        command_id: "command-01".to_owned(),
    }));
    assert_eq!(authority.active_target(), Some(&b));
    assert_eq!(
        authority.active_draft().map(|draft| draft.text()),
        Some("draft b")
    );
    authority.retarget(Some(a));
    assert_eq!(
        authority.active_draft().map(|draft| draft.text()),
        Some("first plus newer")
    );
}

#[test]
fn rejection_retains_exact_snapshot_for_retry_and_ignores_stale_resolution() {
    let a = target("a");
    let mut authority = ComposerAuthority::new(Some(a.clone()));
    assert!(authority.replace_active("  keep exact  "));
    let (_, revision, _) = submission(&mut authority);
    assert!(authority.replace_active(" with newer text"));

    assert!(!authority.resolve(SubmissionResolution::Rejected {
        target: a.clone(),
        revision: revision.next(),
        reason: "stale".to_owned(),
    }));
    assert!(authority.resolve(SubmissionResolution::Rejected {
        target: a.clone(),
        revision,
        reason: "offline".to_owned(),
    }));
    assert_eq!(authority.diagnostic(), Some("offline"));
    assert_eq!(
        authority.active_draft().map(|draft| draft.text()),
        Some("  keep exact   with newer text")
    );
    let rejected = authority
        .rejected_submission()
        .unwrap_or_else(|| panic!("rejected snapshot"));
    assert_eq!(rejected.target(), &a);
    assert_eq!(rejected.revision(), revision);
    assert_eq!(rejected.submission().text(), "  keep exact  ");

    let ComposerIntent::Submit {
        target: retry_target,
        revision: retry_revision,
        submission: retry_submission,
    } = authority
        .retry_rejected()
        .unwrap_or_else(|| panic!("rejected snapshot should be retryable"));
    assert_eq!(retry_target, a);
    assert_eq!(retry_revision, revision);
    assert_eq!(retry_submission.text(), "  keep exact  ");
    assert!(authority.is_pending());
}

#[test]
fn revisions_advance_only_for_semantic_draft_mutations() {
    let mut authority = ComposerAuthority::new(Some(target("a")));
    let initial = authority
        .active_revision()
        .unwrap_or_else(|| panic!("active revision"));
    assert!(authority.replace_active("abc"));
    let edited = authority
        .active_revision()
        .unwrap_or_else(|| panic!("edited revision"));
    assert_eq!(edited, initial.next());

    assert!(
        authority
            .edit_active(|draft| {
                let cursor = draft.cursor();
                draft.move_to(cursor, false);
            })
            .is_some()
    );
    assert_eq!(authority.active_revision(), Some(edited));

    assert_eq!(
        authority.edit_active(|draft| draft.delete_backward()),
        Some(true)
    );
    assert_eq!(authority.active_revision(), Some(edited.next()));
}

#[test]
fn retaining_core_targets_prunes_deleted_session_drafts_and_active_selection() {
    let a = target("a");
    let b = target("b");
    let mut authority = ComposerAuthority::new(Some(a.clone()));
    assert!(authority.replace_active("draft a"));
    authority.retarget(Some(b.clone()));
    assert!(authority.replace_active("draft b"));

    authority.retain_targets([a.clone()]);
    assert_eq!(authority.active_target(), None);
    authority.retarget(Some(a));
    assert_eq!(
        authority.active_draft().map(|draft| draft.text()),
        Some("draft a")
    );
    authority.retarget(Some(b));
    assert_eq!(authority.active_draft().map(|draft| draft.text()), Some(""));
}
