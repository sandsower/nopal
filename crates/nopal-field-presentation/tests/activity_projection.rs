#![allow(clippy::expect_used)]

use std::collections::BTreeMap;

use nopal_feed_client::session_activity::{
    ActivityOutput, ActivityOutputChannel, CommandExit, CommandOutcome,
    DURABLE_SESSION_ACTIVITY_EVENT_KIND, DurableSessionActivityEvent, SessionActivityEventPayload,
};
use nopal_field_presentation::activity::{
    ActivityItem, ActivityKey, ActivityProjectionError, ActivityTerminalKind, CommandActivityState,
    Direction, VerifiedSessionEvent, project_activity,
};

fn event(
    event_id: &str,
    sequence: u64,
    payload: SessionActivityEventPayload,
) -> VerifiedSessionEvent {
    VerifiedSessionEvent::V3(DurableSessionActivityEvent {
        kind: DURABLE_SESSION_ACTIVITY_EVENT_KIND.to_owned(),
        event_id: event_id.to_owned(),
        plot_id: "plot-a".to_owned(),
        session_id: "session-a".to_owned(),
        stream_id: "stream-a".to_owned(),
        sequence,
        previous_cursor: (sequence > 1).then(|| format!("cursor-{}", sequence - 1)),
        cursor: format!("cursor-{sequence}"),
        command_id: Some("command-a".to_owned()),
        event: payload,
        extra: BTreeMap::new(),
    })
}

#[test]
fn terminal_folds_into_the_stable_start_position_and_navigation_uses_exact_keys() {
    let projection = project_activity(&[
        event(
            "start",
            1,
            SessionActivityEventPayload::CommandStarted {
                activity_id: "activity-a".to_owned(),
                tool_call_id: "call-a".to_owned(),
                command: "printf exact".to_owned(),
                started_at: "t1".to_owned(),
                working_directory: Some("/repo".to_owned()),
                extra: BTreeMap::new(),
            },
        ),
        event(
            "message",
            2,
            SessionActivityEventPayload::AssistantMessage {
                text: "between".to_owned(),
                extra: BTreeMap::new(),
            },
        ),
        event(
            "finish",
            3,
            SessionActivityEventPayload::CommandFinished {
                activity_id: "activity-a".to_owned(),
                tool_call_id: "call-a".to_owned(),
                duration_ms: 11,
                exit: CommandExit::Code { code: 0 },
                outcome: CommandOutcome::Succeeded,
                output: Some(ActivityOutput {
                    channel: ActivityOutputChannel::Combined,
                    text: "exact".to_owned(),
                    truncated: false,
                    original_bytes: 5,
                    omitted_bytes: 0,
                    extra: BTreeMap::new(),
                }),
                extra: BTreeMap::new(),
            },
        ),
    ])
    .expect("valid activity stream");

    let activity_key = ActivityKey::Activity("activity-a".to_owned());
    let message_key = ActivityKey::Event("message".to_owned());
    assert_eq!(projection.first_key(), Some(&activity_key));
    assert_eq!(
        projection.adjacent_key(&activity_key, Direction::Next),
        Some(&message_key)
    );
    assert!(matches!(
        projection.item(&activity_key),
        Some(ActivityItem::Command(command))
            if matches!(command.state, CommandActivityState::Finished { duration_ms: 11, .. })
    ));
}

#[test]
fn orphan_terminal_fails_closed_instead_of_inventing_a_start() {
    let error = project_activity(&[event(
        "orphan",
        1,
        SessionActivityEventPayload::CommandFailed {
            activity_id: "missing".to_owned(),
            tool_call_id: "missing-call".to_owned(),
            duration_ms: None,
            message: "missing start".to_owned(),
            extra: BTreeMap::new(),
        },
    )])
    .expect_err("orphan terminal must be rejected");

    assert_eq!(
        error,
        ActivityProjectionError::OrphanTerminal {
            activity_id: "missing".to_owned(),
            terminal: ActivityTerminalKind::CommandFailed,
        }
    );
}
