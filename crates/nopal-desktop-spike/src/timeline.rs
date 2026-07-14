use std::collections::BTreeMap;

use nopal_feed_client::session::{DurableSessionEvent, SessionEvent, SessionReplayComplete};
use nopal_feed_client::session_activity::DurableSessionActivityEvent;

use crate::activity::VerifiedSessionEvent;
use crate::model::SelectedSessionContext;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct SessionTimelineKey {
    plot_id: String,
    session_id: String,
}

impl From<&SelectedSessionContext> for SessionTimelineKey {
    fn from(context: &SelectedSessionContext) -> Self {
        Self {
            plot_id: context.plot_id.clone(),
            session_id: context.session_id.clone(),
        }
    }
}

pub type DurableTimelineEvent = VerifiedSessionEvent;

impl From<DurableSessionEvent> for VerifiedSessionEvent {
    fn from(event: DurableSessionEvent) -> Self {
        Self::V2(event)
    }
}

impl From<DurableSessionActivityEvent> for VerifiedSessionEvent {
    fn from(event: DurableSessionActivityEvent) -> Self {
        Self::V3(event)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimelineFailure {
    NoSelectedSession {
        event_id: String,
        actual_plot_id: String,
        actual_session_id: String,
    },
    ForeignIdentity {
        event_id: String,
        expected_plot_id: String,
        expected_session_id: String,
        actual_plot_id: String,
        actual_session_id: String,
    },
    StreamConflict {
        event_id: String,
        expected_stream_id: String,
        actual_stream_id: String,
    },
    EventIdConflict {
        event_id: String,
    },
    CursorConflict {
        cursor: String,
    },
    SequenceExhausted {
        stream_id: Option<String>,
    },
    Gap {
        event_id: String,
        expected_sequence: u64,
        actual_sequence: u64,
        expected_previous_cursor: Option<String>,
        actual_previous_cursor: Option<String>,
    },
    ReplayStartMismatch {
        expected_cursor: Option<String>,
        actual_cursor: Option<String>,
    },
    ReplayCompleteMismatch(Box<ReplayHeadMismatch>),
    Feed {
        code: String,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayHeadMismatch {
    pub expected_stream_id: Option<String>,
    pub actual_stream_id: Option<String>,
    pub expected_sequence: u64,
    pub actual_sequence: u64,
    pub expected_cursor: Option<String>,
    pub actual_cursor: Option<String>,
    pub expected_count: u64,
    pub actual_count: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ReplayState {
    #[default]
    Idle,
    Restoring {
        after_cursor: Option<String>,
        received: u64,
    },
    Live,
    Reconnecting {
        attempt: u32,
        detail: String,
    },
    Failed(TimelineFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimelineIngestOutcome {
    Appended,
    Duplicate,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimelineRejection {
    pub event_id: String,
    pub expected_plot_id: String,
    pub expected_session_id: String,
    pub actual_plot_id: String,
    pub actual_session_id: String,
}

#[derive(Debug, Default)]
struct ReplayStage {
    after_cursor: Option<String>,
    events: Vec<SessionEvent>,
    durable_events: Vec<DurableTimelineEvent>,
    event_indexes: BTreeMap<String, usize>,
    cursor_indexes: BTreeMap<String, usize>,
    stream_id: Option<String>,
    head_cursor: Option<String>,
    last_sequence: Option<u64>,
    received: u64,
}

#[derive(Debug, Default)]
struct VerifiedTimeline {
    events: Vec<SessionEvent>,
    durable_events: Vec<DurableTimelineEvent>,
    event_indexes: BTreeMap<String, usize>,
    cursor_indexes: BTreeMap<String, usize>,
    stream_id: Option<String>,
    head_cursor: Option<String>,
    last_sequence: Option<u64>,
    replay_state: ReplayState,
    replay_stage: Option<ReplayStage>,
    failure: Option<TimelineFailure>,
    draft: String,
}

impl VerifiedTimeline {
    fn begin_replay(&mut self, after_cursor: Option<&str>) -> Result<(), TimelineFailure> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        let actual = after_cursor.map(str::to_owned);
        if self.head_cursor != actual {
            let failure = TimelineFailure::ReplayStartMismatch {
                expected_cursor: self.head_cursor.clone(),
                actual_cursor: actual,
            };
            self.freeze(failure.clone());
            return Err(failure);
        }
        self.replay_stage = Some(ReplayStage {
            after_cursor: self.head_cursor.clone(),
            stream_id: self.stream_id.clone(),
            head_cursor: self.head_cursor.clone(),
            last_sequence: self.last_sequence,
            ..ReplayStage::default()
        });
        self.replay_state = ReplayState::Restoring {
            after_cursor: self.head_cursor.clone(),
            received: 0,
        };
        Ok(())
    }

    fn retry_replay(&mut self) -> Result<(), TimelineFailure> {
        self.failure = None;
        self.replay_stage = None;
        let after_cursor = self.head_cursor.clone();
        self.begin_replay(after_cursor.as_deref())
    }

    fn ingest(
        &mut self,
        event: DurableTimelineEvent,
    ) -> Result<TimelineIngestOutcome, TimelineFailure> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }

        if let Some(index) = self.event_indexes.get(event.event_id()).copied() {
            if self.durable_events[index] == event {
                self.note_replay_frame();
                return Ok(TimelineIngestOutcome::Duplicate);
            }
            let failure = TimelineFailure::EventIdConflict {
                event_id: event.event_id().to_owned(),
            };
            self.freeze(failure.clone());
            return Err(failure);
        }
        if let Some(index) = self.cursor_indexes.get(event.cursor()).copied() {
            if self.durable_events[index] == event {
                self.note_replay_frame();
                return Ok(TimelineIngestOutcome::Duplicate);
            }
            let failure = TimelineFailure::CursorConflict {
                cursor: event.cursor().to_owned(),
            };
            self.freeze(failure.clone());
            return Err(failure);
        }
        if let Some(stage) = &self.replay_stage {
            if let Some(index) = stage.event_indexes.get(event.event_id()).copied() {
                if stage.durable_events[index] == event {
                    self.note_replay_frame();
                    return Ok(TimelineIngestOutcome::Duplicate);
                }
                let failure = TimelineFailure::EventIdConflict {
                    event_id: event.event_id().to_owned(),
                };
                self.freeze(failure.clone());
                return Err(failure);
            }
            if let Some(index) = stage.cursor_indexes.get(event.cursor()).copied() {
                if stage.durable_events[index] == event {
                    self.note_replay_frame();
                    return Ok(TimelineIngestOutcome::Duplicate);
                }
                let failure = TimelineFailure::CursorConflict {
                    cursor: event.cursor().to_owned(),
                };
                self.freeze(failure.clone());
                return Err(failure);
            }
        }

        let stream_id = self
            .replay_stage
            .as_ref()
            .and_then(|stage| stage.stream_id.as_ref())
            .or(self.stream_id.as_ref());
        if let Some(stream_id) = stream_id
            && stream_id != event.stream_id()
        {
            let failure = TimelineFailure::StreamConflict {
                event_id: event.event_id().to_owned(),
                expected_stream_id: stream_id.clone(),
                actual_stream_id: event.stream_id().to_owned(),
            };
            self.freeze(failure.clone());
            return Err(failure);
        }

        let last_sequence = self
            .replay_stage
            .as_ref()
            .and_then(|stage| stage.last_sequence)
            .or(self.last_sequence);
        let head_cursor = self
            .replay_stage
            .as_ref()
            .map(|stage| stage.head_cursor.clone())
            .unwrap_or_else(|| self.head_cursor.clone());
        let expected_sequence = match last_sequence {
            None => 1,
            Some(sequence) => {
                let Some(expected) = sequence.checked_add(1) else {
                    let failure = TimelineFailure::SequenceExhausted {
                        stream_id: self.stream_id.clone(),
                    };
                    self.freeze(failure.clone());
                    return Err(failure);
                };
                expected
            }
        };
        if event.sequence() != expected_sequence
            || event.previous_cursor() != head_cursor.as_deref()
        {
            let failure = TimelineFailure::Gap {
                event_id: event.event_id().to_owned(),
                expected_sequence,
                actual_sequence: event.sequence(),
                expected_previous_cursor: head_cursor,
                actual_previous_cursor: event.previous_cursor().map(str::to_owned),
            };
            self.freeze(failure.clone());
            return Err(failure);
        }

        if let Some(stage) = self.replay_stage.as_mut() {
            stage
                .stream_id
                .get_or_insert_with(|| event.stream_id().to_owned());
            stage.last_sequence = Some(event.sequence());
            stage.head_cursor = Some(event.cursor().to_owned());
            let index = stage.durable_events.len();
            stage
                .event_indexes
                .insert(event.event_id().to_owned(), index);
            stage
                .cursor_indexes
                .insert(event.cursor().to_owned(), index);
            if let Some(semantic_event) = event.semantic_session_event() {
                stage.events.push(semantic_event);
            }
            stage.durable_events.push(event);
        } else {
            self.stream_id
                .get_or_insert_with(|| event.stream_id().to_owned());
            self.last_sequence = Some(event.sequence());
            self.head_cursor = Some(event.cursor().to_owned());
            let index = self.durable_events.len();
            self.event_indexes
                .insert(event.event_id().to_owned(), index);
            self.cursor_indexes.insert(event.cursor().to_owned(), index);
            if let Some(semantic_event) = event.semantic_session_event() {
                self.events.push(semantic_event);
            }
            self.durable_events.push(event);
        }
        self.note_replay_frame();
        Ok(TimelineIngestOutcome::Appended)
    }

    fn complete_replay(&mut self, cursor: Option<&str>, count: u64) -> Result<(), TimelineFailure> {
        if let Some(failure) = &self.failure {
            return Err(failure.clone());
        }
        let Some(stage) = self.replay_stage.take() else {
            let failure = TimelineFailure::ReplayStartMismatch {
                expected_cursor: self.head_cursor.clone(),
                actual_cursor: cursor.map(str::to_owned),
            };
            self.freeze(failure.clone());
            return Err(failure);
        };
        let actual_cursor = cursor.map(str::to_owned);
        if stage.head_cursor != actual_cursor || stage.received != count {
            let failure = TimelineFailure::ReplayCompleteMismatch(Box::new(ReplayHeadMismatch {
                expected_stream_id: stage.stream_id.clone(),
                actual_stream_id: stage.stream_id.clone(),
                expected_sequence: stage.last_sequence.unwrap_or(0),
                actual_sequence: stage.last_sequence.unwrap_or(0),
                expected_cursor: stage.head_cursor.clone(),
                actual_cursor,
                expected_count: stage.received,
                actual_count: count,
            }));
            self.freeze(failure.clone());
            return Err(failure);
        }
        let base_index = self.durable_events.len();
        for (offset, event) in stage.durable_events.iter().enumerate() {
            self.event_indexes
                .insert(event.event_id().to_owned(), base_index + offset);
            self.cursor_indexes
                .insert(event.cursor().to_owned(), base_index + offset);
        }
        self.events.extend(stage.events);
        self.durable_events.extend(stage.durable_events);
        self.stream_id = stage.stream_id;
        self.head_cursor = stage.head_cursor;
        self.last_sequence = stage.last_sequence;
        self.replay_state = ReplayState::Live;
        Ok(())
    }

    fn freeze(&mut self, failure: TimelineFailure) {
        self.replay_stage = None;
        self.failure = Some(failure.clone());
        self.replay_state = ReplayState::Failed(failure);
    }

    fn note_replay_frame(&mut self) {
        if let Some(stage) = self.replay_stage.as_mut() {
            stage.received = stage.received.saturating_add(1);
            self.replay_state = ReplayState::Restoring {
                after_cursor: stage.after_cursor.clone(),
                received: stage.received,
            };
        }
    }
}

#[derive(Debug, Default)]
pub struct SessionTimelineStore {
    selected: Option<SessionTimelineKey>,
    timelines: BTreeMap<SessionTimelineKey, VerifiedTimeline>,
    last_rejection: Option<TimelineRejection>,
    orphan_failure: Option<TimelineFailure>,
}

impl SessionTimelineStore {
    pub fn select_session(&mut self, context: Option<&SelectedSessionContext>) {
        self.selected = context.map(SessionTimelineKey::from);
        if let Some(selected) = &self.selected {
            self.timelines.entry(selected.clone()).or_default();
        }
    }

    pub fn begin_replay(&mut self, after_cursor: Option<&str>) -> Result<(), TimelineFailure> {
        let Some(timeline) = self.selected_timeline_mut() else {
            return Err(self.no_selected_failure("replay"));
        };
        timeline.begin_replay(after_cursor)
    }

    pub fn retry_replay(&mut self) -> Result<(), TimelineFailure> {
        let Some(timeline) = self.selected_timeline_mut() else {
            return Err(self.no_selected_failure("replay retry"));
        };
        timeline.retry_replay()
    }

    pub fn ingest_durable<E>(&mut self, event: E) -> Result<TimelineIngestOutcome, TimelineFailure>
    where
        E: Into<DurableTimelineEvent>,
    {
        let event = event.into();
        let Some(selected) = self.selected.clone() else {
            let failure = TimelineFailure::NoSelectedSession {
                event_id: event.event_id().to_owned(),
                actual_plot_id: event.plot_id().to_owned(),
                actual_session_id: event.session_id().to_owned(),
            };
            self.orphan_failure = Some(failure.clone());
            return Err(failure);
        };
        if event.plot_id() != selected.plot_id || event.session_id() != selected.session_id {
            let rejection = TimelineRejection {
                event_id: event.event_id().to_owned(),
                expected_plot_id: selected.plot_id.clone(),
                expected_session_id: selected.session_id.clone(),
                actual_plot_id: event.plot_id().to_owned(),
                actual_session_id: event.session_id().to_owned(),
            };
            self.last_rejection = Some(rejection.clone());
            let failure = TimelineFailure::ForeignIdentity {
                event_id: rejection.event_id,
                expected_plot_id: rejection.expected_plot_id,
                expected_session_id: rejection.expected_session_id,
                actual_plot_id: rejection.actual_plot_id,
                actual_session_id: rejection.actual_session_id,
            };
            self.timelines
                .entry(selected)
                .or_default()
                .freeze(failure.clone());
            return Err(failure);
        }
        self.last_rejection = None;
        self.timelines.entry(selected).or_default().ingest(event)
    }

    pub fn complete_replay(
        &mut self,
        cursor: Option<&str>,
        count: u64,
    ) -> Result<(), TimelineFailure> {
        let Some(timeline) = self.selected_timeline_mut() else {
            return Err(self.no_selected_failure("replay-complete"));
        };
        timeline.complete_replay(cursor, count)
    }

    pub fn complete_durable_replay(
        &mut self,
        complete: &SessionReplayComplete,
    ) -> Result<(), TimelineFailure> {
        let Some(selected) = self.selected.clone() else {
            return Err(self.no_selected_failure(&complete.request_id));
        };
        if complete.plot_id != selected.plot_id || complete.session_id != selected.session_id {
            let failure = TimelineFailure::ForeignIdentity {
                event_id: complete.request_id.clone(),
                expected_plot_id: selected.plot_id.clone(),
                expected_session_id: selected.session_id.clone(),
                actual_plot_id: complete.plot_id.clone(),
                actual_session_id: complete.session_id.clone(),
            };
            self.timelines
                .entry(selected)
                .or_default()
                .freeze(failure.clone());
            return Err(failure);
        }
        let timeline = self.timelines.entry(selected).or_default();
        let Some(stage) = timeline.replay_stage.as_mut() else {
            let failure = TimelineFailure::ReplayStartMismatch {
                expected_cursor: timeline.head_cursor.clone(),
                actual_cursor: complete.cursor.clone(),
            };
            timeline.freeze(failure.clone());
            return Err(failure);
        };
        let expected_sequence = stage.last_sequence.unwrap_or(0);
        if stage.stream_id.is_none()
            && expected_sequence == 0
            && stage.head_cursor.is_none()
            && complete.sequence == 0
            && complete.cursor.is_none()
        {
            stage.stream_id = Some(complete.stream_id.clone());
        }
        if stage.stream_id.as_deref() != Some(complete.stream_id.as_str())
            || expected_sequence != complete.sequence
        {
            let failure = TimelineFailure::ReplayCompleteMismatch(Box::new(ReplayHeadMismatch {
                expected_stream_id: stage.stream_id.clone(),
                actual_stream_id: Some(complete.stream_id.clone()),
                expected_sequence,
                actual_sequence: complete.sequence,
                expected_cursor: stage.head_cursor.clone(),
                actual_cursor: complete.cursor.clone(),
                expected_count: stage.received,
                actual_count: complete.event_count,
            }));
            timeline.freeze(failure.clone());
            return Err(failure);
        }
        timeline.complete_replay(complete.cursor.as_deref(), complete.event_count)
    }

    pub fn mark_reconnecting(&mut self, attempt: u32, detail: impl Into<String>) {
        if let Some(timeline) = self.selected_timeline_mut()
            && timeline.failure.is_none()
        {
            timeline.replay_stage = None;
            timeline.replay_state = ReplayState::Reconnecting {
                attempt,
                detail: detail.into(),
            };
        }
    }

    pub fn fail_feed(&mut self, code: impl Into<String>, message: impl Into<String>) {
        let failure = TimelineFailure::Feed {
            code: code.into(),
            message: message.into(),
        };
        if let Some(timeline) = self.selected_timeline_mut() {
            timeline.freeze(failure);
        } else {
            self.orphan_failure = Some(failure);
        }
    }

    pub fn current_events(&self) -> &[SessionEvent] {
        self.selected_timeline()
            .map(|timeline| timeline.events.as_slice())
            .unwrap_or_default()
    }

    pub fn current_verified_events(&self) -> &[VerifiedSessionEvent] {
        self.selected_timeline()
            .map(|timeline| timeline.durable_events.as_slice())
            .unwrap_or_default()
    }

    pub fn current_cursor(&self) -> Option<&str> {
        self.selected_timeline()
            .and_then(|timeline| timeline.head_cursor.as_deref())
    }

    pub fn current_sequence(&self) -> Option<u64> {
        self.selected_timeline()
            .and_then(|timeline| timeline.last_sequence)
    }

    pub fn current_stream_id(&self) -> Option<&str> {
        self.selected_timeline()
            .and_then(|timeline| timeline.stream_id.as_deref())
    }

    pub fn current_contains_command(&self, command_id: &str) -> bool {
        self.selected_timeline().is_some_and(|timeline| {
            timeline
                .durable_events
                .iter()
                .any(|event| event.command_id() == Some(command_id))
        })
    }

    pub fn current_replay_state(&self) -> ReplayState {
        self.selected_timeline()
            .map(|timeline| timeline.replay_state.clone())
            .unwrap_or_default()
    }

    pub fn current_failure(&self) -> Option<&TimelineFailure> {
        self.selected_timeline()
            .and_then(|timeline| timeline.failure.as_ref())
            .or(self.orphan_failure.as_ref())
    }

    pub fn set_current_draft(&mut self, draft: impl Into<String>) {
        if let Some(timeline) = self.selected_timeline_mut() {
            timeline.draft = draft.into();
        }
    }

    pub fn current_draft(&self) -> &str {
        self.selected_timeline()
            .map(|timeline| timeline.draft.as_str())
            .unwrap_or_default()
    }

    pub fn last_rejection(&self) -> Option<&TimelineRejection> {
        self.last_rejection.as_ref()
    }

    fn selected_timeline(&self) -> Option<&VerifiedTimeline> {
        self.selected
            .as_ref()
            .and_then(|selected| self.timelines.get(selected))
    }

    fn selected_timeline_mut(&mut self) -> Option<&mut VerifiedTimeline> {
        let selected = self.selected.clone()?;
        Some(self.timelines.entry(selected).or_default())
    }

    fn no_selected_failure(&mut self, operation: &str) -> TimelineFailure {
        let failure = TimelineFailure::NoSelectedSession {
            event_id: operation.to_owned(),
            actual_plot_id: String::new(),
            actual_session_id: String::new(),
        };
        self.orphan_failure = Some(failure.clone());
        failure
    }
}

#[cfg(test)]
mod tests {
    use nopal_feed_client::session::{
        DurableSessionEvent, SESSION_REPLAY_COMPLETE_KIND, SessionReplayComplete,
        parse_durable_session_event,
    };
    use nopal_feed_client::session_activity::{
        DurableSessionActivityEvent, parse_session_activity_event,
    };

    use crate::model::SelectedSessionContext;

    use super::{
        ReplayState, SessionTimelineStore, TimelineFailure, TimelineIngestOutcome,
        VerifiedSessionEvent,
    };

    fn context(plot_id: &str, session_id: &str) -> SelectedSessionContext {
        SelectedSessionContext {
            plot_id: plot_id.to_owned(),
            session_id: session_id.to_owned(),
            host_pane: None,
            protocol: None,
        }
    }

    fn durable_event(
        event_id: &str,
        plot_id: &str,
        session_id: &str,
        sequence: u64,
        previous_cursor: Option<&str>,
        cursor: &str,
        text: &str,
    ) -> DurableSessionEvent {
        parse_durable_session_event(
            &serde_json::json!({
                "kind": "nopal.session.event/v2",
                "event_id": event_id,
                "plot_id": plot_id,
                "session_id": session_id,
                "stream_id": "stream-a",
                "sequence": sequence,
                "previous_cursor": previous_cursor,
                "cursor": cursor,
                "command_id": "command-01",
                "event": {"type": "assistant_message", "text": text}
            })
            .to_string(),
        )
        .expect("valid durable event fixture")
    }

    fn activity_event(
        event_id: &str,
        plot_id: &str,
        session_id: &str,
        sequence: u64,
        previous_cursor: Option<&str>,
        cursor: &str,
        activity_id: &str,
    ) -> DurableSessionActivityEvent {
        parse_session_activity_event(
            &serde_json::json!({
                "kind": "nopal.session.event/v3",
                "event_id": event_id,
                "plot_id": plot_id,
                "session_id": session_id,
                "stream_id": "stream-a",
                "sequence": sequence,
                "previous_cursor": previous_cursor,
                "cursor": cursor,
                "command_id": "command-01",
                "event": {
                    "type": "command_started",
                    "activity_id": activity_id,
                    "tool_call_id": format!("call-{activity_id}"),
                    "command": "printf exact",
                    "started_at": "2026-07-13T17:00:00Z"
                },
                "future_envelope": {"preserved": true}
            })
            .to_string(),
        )
        .expect("valid durable activity fixture")
    }

    fn v3_message_event(
        event_id: &str,
        sequence: u64,
        previous_cursor: Option<&str>,
        cursor: &str,
        text: &str,
    ) -> DurableSessionActivityEvent {
        parse_session_activity_event(
            &serde_json::json!({
                "kind": "nopal.session.event/v3",
                "event_id": event_id,
                "plot_id": "plot-a",
                "session_id": "session-a",
                "stream_id": "stream-a",
                "sequence": sequence,
                "previous_cursor": previous_cursor,
                "cursor": cursor,
                "event": {"type": "assistant_message", "text": text},
                "future_envelope": {"preserved": true}
            })
            .to_string(),
        )
        .expect("valid v3 message fixture")
    }

    #[test]
    fn v3_messages_keep_legacy_semantic_parity_while_activity_stays_out_of_message_output() {
        let session = context("plot-a", "session-a");
        let message = v3_message_event("message-v3", 1, None, "cursor-1", "Exact v3");
        let command = activity_event(
            "command-v3",
            "plot-a",
            "session-a",
            2,
            Some("cursor-1"),
            "cursor-2",
            "activity-1",
        );
        let mut timelines = SessionTimelineStore::default();
        timelines.select_session(Some(&session));
        timelines.begin_replay(None).unwrap();
        timelines.ingest_durable(message.clone()).unwrap();
        timelines.ingest_durable(command.clone()).unwrap();
        timelines.complete_replay(Some("cursor-2"), 2).unwrap();

        assert_eq!(timelines.current_events().len(), 1);
        assert_eq!(timelines.current_events()[0].event_id, "message-v3");
        assert!(matches!(
            &timelines.current_events()[0].event,
            nopal_feed_client::session::SessionEventPayload::AssistantMessage { text, .. }
                if text == "Exact v3"
        ));
        assert_eq!(
            timelines.current_verified_events(),
            &[
                VerifiedSessionEvent::V3(message),
                VerifiedSessionEvent::V3(command),
            ]
        );
    }

    #[test]
    fn old_v2_cursor_resume_retains_an_exact_mixed_verified_prefix() {
        let session = context("plot-a", "session-a");
        let v2 = durable_event(
            "event-v2",
            "plot-a",
            "session-a",
            1,
            None,
            "cursor-v2",
            "Exact v2",
        );
        let v3 = activity_event(
            "event-v3",
            "plot-a",
            "session-a",
            2,
            Some("cursor-v2"),
            "cursor-v3",
            "activity-1",
        );
        let mut timelines = SessionTimelineStore::default();
        timelines.select_session(Some(&session));
        timelines.begin_replay(None).unwrap();
        timelines.ingest_durable(v2.clone()).unwrap();
        timelines.complete_replay(Some("cursor-v2"), 1).unwrap();

        timelines.begin_replay(Some("cursor-v2")).unwrap();
        timelines.ingest_durable(v3.clone()).unwrap();
        assert_eq!(
            timelines.current_verified_events(),
            &[VerifiedSessionEvent::V2(v2.clone())],
            "a staged suffix must not replace the last verified prefix"
        );
        timelines.complete_replay(Some("cursor-v3"), 1).unwrap();

        assert_eq!(
            timelines.current_verified_events(),
            &[VerifiedSessionEvent::V2(v2), VerifiedSessionEvent::V3(v3),]
        );
        assert_eq!(timelines.current_cursor(), Some("cursor-v3"));
        assert_eq!(timelines.current_sequence(), Some(2));
    }

    #[test]
    fn v3_identity_gap_and_cross_version_conflicts_preserve_the_last_verified_prefix() {
        let session = context("plot-a", "session-a");
        let verified = durable_event(
            "event-1",
            "plot-a",
            "session-a",
            1,
            None,
            "cursor-1",
            "Verified",
        );

        for rejected in [
            activity_event(
                "foreign",
                "plot-other",
                "session-other",
                2,
                Some("cursor-1"),
                "cursor-2",
                "activity-foreign",
            ),
            activity_event(
                "gap",
                "plot-a",
                "session-a",
                3,
                Some("cursor-2"),
                "cursor-3",
                "activity-gap",
            ),
            activity_event(
                "event-1",
                "plot-a",
                "session-a",
                2,
                Some("cursor-1"),
                "cursor-2",
                "activity-conflict",
            ),
        ] {
            let mut timelines = SessionTimelineStore::default();
            timelines.select_session(Some(&session));
            timelines.begin_replay(None).unwrap();
            timelines.ingest_durable(verified.clone()).unwrap();
            timelines.complete_replay(Some("cursor-1"), 1).unwrap();
            timelines.begin_replay(Some("cursor-1")).unwrap();

            assert!(timelines.ingest_durable(rejected).is_err());
            assert_eq!(
                timelines.current_verified_events(),
                &[VerifiedSessionEvent::V2(verified.clone())]
            );
            assert_eq!(timelines.current_cursor(), Some("cursor-1"));
            assert_eq!(timelines.current_sequence(), Some(1));
        }
    }

    #[test]
    fn cold_replay_builds_one_verified_timeline_and_suppresses_exact_overlap() {
        let session = context("plot-a", "session-a");
        let mut timelines = SessionTimelineStore::default();
        timelines.select_session(Some(&session));
        timelines.begin_replay(None).unwrap();

        let first = durable_event(
            "event-1",
            "plot-a",
            "session-a",
            1,
            None,
            "cursor-1",
            "First",
        );
        let second = durable_event(
            "event-2",
            "plot-a",
            "session-a",
            2,
            Some("cursor-1"),
            "cursor-2",
            "Second",
        );
        assert_eq!(
            timelines.ingest_durable(first.clone()).unwrap(),
            TimelineIngestOutcome::Appended
        );
        assert_eq!(
            timelines.ingest_durable(second).unwrap(),
            TimelineIngestOutcome::Appended
        );
        assert_eq!(
            timelines.ingest_durable(first).unwrap(),
            TimelineIngestOutcome::Duplicate
        );
        assert!(
            timelines.current_events().is_empty(),
            "replay frames must remain staged until replay_complete validates the head"
        );
        assert_eq!(timelines.current_cursor(), None);
        timelines.complete_replay(Some("cursor-2"), 3).unwrap();

        assert_eq!(timelines.current_events().len(), 2);
        assert_eq!(timelines.current_cursor(), Some("cursor-2"));
        assert_eq!(timelines.current_sequence(), Some(2));
        assert_eq!(timelines.current_replay_state(), ReplayState::Live);
    }

    #[test]
    fn gap_freezes_the_verified_prefix_and_refuses_later_events() {
        let session = context("plot-a", "session-a");
        let mut timelines = SessionTimelineStore::default();
        timelines.select_session(Some(&session));
        timelines.begin_replay(None).unwrap();
        timelines
            .ingest_durable(durable_event(
                "event-1",
                "plot-a",
                "session-a",
                1,
                None,
                "cursor-1",
                "First",
            ))
            .unwrap();

        let gap = timelines
            .ingest_durable(durable_event(
                "event-3",
                "plot-a",
                "session-a",
                3,
                Some("cursor-2"),
                "cursor-3",
                "Third",
            ))
            .unwrap_err();
        assert!(matches!(
            gap,
            TimelineFailure::Gap {
                expected_sequence: 2,
                actual_sequence: 3,
                ..
            }
        ));
        assert!(timelines.current_events().is_empty());
        assert_eq!(timelines.current_cursor(), None);
        assert_eq!(
            timelines
                .ingest_durable(durable_event(
                    "event-2",
                    "plot-a",
                    "session-a",
                    2,
                    Some("cursor-1"),
                    "cursor-2",
                    "Second",
                ))
                .unwrap_err(),
            gap
        );
        assert_eq!(timelines.current_replay_state(), ReplayState::Failed(gap));
    }

    #[test]
    fn conflicting_event_or_cursor_never_changes_the_verified_prefix() {
        for conflict_by_event_id in [true, false] {
            let session = context("plot-a", "session-a");
            let mut timelines = SessionTimelineStore::default();
            timelines.select_session(Some(&session));
            timelines.begin_replay(None).unwrap();
            timelines
                .ingest_durable(durable_event(
                    "event-1",
                    "plot-a",
                    "session-a",
                    1,
                    None,
                    "cursor-1",
                    "First",
                ))
                .unwrap();
            let mut conflict = durable_event(
                if conflict_by_event_id {
                    "event-1"
                } else {
                    "event-2"
                },
                "plot-a",
                "session-a",
                2,
                Some("cursor-1"),
                "cursor-2",
                "Conflicting",
            );
            if !conflict_by_event_id {
                // The contract parser already rejects a cursor equal to its
                // predecessor. Mutate after parsing to prove the store still
                // defends its own index against a compromised producer.
                conflict.cursor = "cursor-1".to_owned();
            }
            let error = timelines.ingest_durable(conflict).unwrap_err();
            assert!(matches!(
                error,
                TimelineFailure::EventIdConflict { .. } | TimelineFailure::CursorConflict { .. }
            ));
            assert!(timelines.current_events().is_empty());
        }
    }

    #[test]
    fn foreign_identity_and_replay_complete_mismatch_fail_visibly() {
        let session = context("plot-a", "session-a");
        let mut foreign = SessionTimelineStore::default();
        foreign.select_session(Some(&session));
        foreign.begin_replay(None).unwrap();
        let error = foreign
            .ingest_durable(durable_event(
                "event-b",
                "plot-b",
                "session-b",
                1,
                None,
                "cursor-b",
                "Foreign",
            ))
            .unwrap_err();
        assert!(matches!(error, TimelineFailure::ForeignIdentity { .. }));
        assert!(foreign.current_events().is_empty());

        let mut mismatched = SessionTimelineStore::default();
        mismatched.select_session(Some(&session));
        mismatched.begin_replay(None).unwrap();
        mismatched
            .ingest_durable(durable_event(
                "event-1",
                "plot-a",
                "session-a",
                1,
                None,
                "cursor-1",
                "First",
            ))
            .unwrap();
        let error = mismatched
            .complete_replay(Some("cursor-other"), 1)
            .unwrap_err();
        assert!(matches!(error, TimelineFailure::ReplayCompleteMismatch(_)));
        assert!(mismatched.current_events().is_empty());
    }

    #[test]
    fn durable_replay_completion_validates_stream_and_sequence_and_accepts_empty_history() {
        let session = context("plot-a", "session-a");
        let complete =
            |stream_id: &str, cursor: Option<&str>, sequence, event_count| SessionReplayComplete {
                kind: SESSION_REPLAY_COMPLETE_KIND.to_owned(),
                request_id: "request-1".to_owned(),
                plot_id: "plot-a".to_owned(),
                session_id: "session-a".to_owned(),
                stream_id: stream_id.to_owned(),
                cursor: cursor.map(str::to_owned),
                sequence,
                event_count,
                extra: Default::default(),
            };

        let mut empty = SessionTimelineStore::default();
        empty.select_session(Some(&session));
        empty.begin_replay(None).unwrap();
        empty
            .complete_durable_replay(&complete("stream-a", None, 0, 0))
            .unwrap();
        assert_eq!(empty.current_stream_id(), Some("stream-a"));
        assert_eq!(empty.current_replay_state(), ReplayState::Live);

        let mut mismatch = SessionTimelineStore::default();
        mismatch.select_session(Some(&session));
        mismatch.begin_replay(None).unwrap();
        mismatch
            .ingest_durable(durable_event(
                "event-1",
                "plot-a",
                "session-a",
                1,
                None,
                "cursor-1",
                "First",
            ))
            .unwrap();
        let error = mismatch
            .complete_durable_replay(&complete("stream-other", Some("cursor-1"), 1, 1))
            .unwrap_err();
        assert!(matches!(
            error,
            TimelineFailure::ReplayCompleteMismatch(detail)
                if detail.expected_stream_id.as_deref() == Some("stream-a")
                    && detail.actual_stream_id.as_deref() == Some("stream-other")
        ));
        assert!(mismatch.current_events().is_empty());
    }

    #[test]
    fn a_b_a_selection_retains_independent_cursor_state_and_drafts() {
        let session_a = context("plot-a", "session-a");
        let session_b = context("plot-b", "session-b");
        let mut timelines = SessionTimelineStore::default();

        timelines.select_session(Some(&session_a));
        timelines.begin_replay(None).unwrap();
        timelines
            .ingest_durable(durable_event(
                "event-a",
                "plot-a",
                "session-a",
                1,
                None,
                "cursor-a",
                "Answer A",
            ))
            .unwrap();
        timelines.complete_replay(Some("cursor-a"), 1).unwrap();
        timelines.set_current_draft("draft a");

        timelines.select_session(Some(&session_b));
        timelines.begin_replay(None).unwrap();
        timelines
            .ingest_durable(DurableSessionEvent {
                stream_id: "stream-b".to_owned(),
                ..durable_event(
                    "event-b",
                    "plot-b",
                    "session-b",
                    1,
                    None,
                    "cursor-b",
                    "Answer B",
                )
            })
            .unwrap();
        timelines.complete_replay(Some("cursor-b"), 1).unwrap();
        timelines.set_current_draft("draft b");

        timelines.select_session(Some(&session_a));
        assert_eq!(timelines.current_cursor(), Some("cursor-a"));
        assert_eq!(timelines.current_draft(), "draft a");
        assert_eq!(timelines.current_events()[0].event_id, "event-a");
        timelines.begin_replay(Some("cursor-a")).unwrap();
        timelines.complete_replay(Some("cursor-a"), 0).unwrap();
        assert_eq!(timelines.current_replay_state(), ReplayState::Live);

        timelines.select_session(Some(&session_b));
        assert_eq!(timelines.current_cursor(), Some("cursor-b"));
        assert_eq!(timelines.current_draft(), "draft b");
    }
}
