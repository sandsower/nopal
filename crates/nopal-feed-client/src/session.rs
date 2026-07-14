//! Typed consumer contract for Nopal's structured Session NDJSON stream.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::session_activity::{
    DURABLE_SESSION_ACTIVITY_EVENT_KIND, DurableSessionActivityEvent,
    parse_session_activity_event_value,
};

pub const SESSION_COMMAND_KIND: &str = "nopal.session.command/v1";
pub const SESSION_EVENT_KIND: &str = "nopal.session.event/v1";
pub const DURABLE_SESSION_EVENT_KIND: &str = "nopal.session.event/v2";
pub const SESSION_SUBSCRIBE_KIND: &str = "nopal.session.subscribe/v1";
pub const SESSION_REPLAY_COMPLETE_KIND: &str = "nopal.session.replay_complete/v1";
pub const SESSION_FEED_ERROR_KIND: &str = "nopal.session.feed_error/v1";
pub const MAX_SESSION_LINE_BYTES: usize = 1024 * 1024;
pub const MAX_SESSION_IDENTITY_BYTES: usize = 4096;
pub const DEFAULT_REPLAY_PAGE_LIMIT: u32 = 256;
pub const MAX_REPLAY_PAGE_LIMIT: u32 = 1024;
pub const MAX_FEED_ERROR_MESSAGE_BYTES: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionCommand {
    pub kind: String,
    pub command_id: String,
    pub plot_id: String,
    pub session_id: String,
    pub command: SessionCommandPayload,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionCommandPayload {
    Prompt {
        text: String,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEvent {
    pub kind: String,
    pub event_id: String,
    pub plot_id: String,
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    pub event: SessionEventPayload,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSubscribe {
    pub kind: String,
    pub request_id: String,
    pub plot_id: String,
    pub session_id: String,
    pub after_cursor: Option<String>,
    #[serde(default = "default_replay_page_limit")]
    pub page_limit: u32,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableSessionEvent {
    pub kind: String,
    pub event_id: String,
    pub plot_id: String,
    pub session_id: String,
    pub stream_id: String,
    pub sequence: u64,
    pub previous_cursor: Option<String>,
    pub cursor: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command_id: Option<String>,
    pub event: SessionEventPayload,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

impl DurableSessionEvent {
    pub fn semantic_event(&self) -> SessionEvent {
        SessionEvent {
            kind: SESSION_EVENT_KIND.to_owned(),
            event_id: self.event_id.clone(),
            plot_id: self.plot_id.clone(),
            session_id: self.session_id.clone(),
            command_id: self.command_id.clone(),
            event: self.event.clone(),
            extra: self.extra.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionReplayComplete {
    pub kind: String,
    pub request_id: String,
    pub plot_id: String,
    pub session_id: String,
    pub stream_id: String,
    pub cursor: Option<String>,
    pub sequence: u64,
    pub event_count: u64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionFeedErrorCode {
    HistoryGap,
    HistoryCorrupt,
    ForeignSession,
    BranchDiverged,
    HistoryTooLarge,
    CursorConflict,
    CommandConflict,
    ReplayBufferOverflow,
    ProtocolViolation,
    Unavailable,
    Internal,
}

impl SessionFeedErrorCode {
    const fn canonical_retryable(self) -> bool {
        matches!(self, Self::ReplayBufferOverflow | Self::Unavailable)
    }

    const fn wire_name(self) -> &'static str {
        match self {
            Self::HistoryGap => "history_gap",
            Self::HistoryCorrupt => "history_corrupt",
            Self::ForeignSession => "foreign_session",
            Self::BranchDiverged => "branch_diverged",
            Self::HistoryTooLarge => "history_too_large",
            Self::CursorConflict => "cursor_conflict",
            Self::CommandConflict => "command_conflict",
            Self::ReplayBufferOverflow => "replay_buffer_overflow",
            Self::ProtocolViolation => "protocol_violation",
            Self::Unavailable => "unavailable",
            Self::Internal => "internal",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionFeedError {
    pub kind: String,
    pub request_id: Option<String>,
    pub plot_id: Option<String>,
    pub session_id: Option<String>,
    pub code: SessionFeedErrorCode,
    pub retryable: bool,
    pub message: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionServerFrame {
    Event(DurableSessionEvent),
    ReplayComplete(SessionReplayComplete),
    FeedError(SessionFeedError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionV3ServerFrame {
    Event(DurableSessionEvent),
    ActivityEvent(DurableSessionActivityEvent),
    ReplayComplete(SessionReplayComplete),
    FeedError(SessionFeedError),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionEventPayload {
    SessionReady {
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    UserMessage {
        text: String,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    AssistantMessage {
        text: String,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    SessionError {
        message: String,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExpectedSessionContext {
    pub plot_id: String,
    pub session_id: String,
}

impl ExpectedSessionContext {
    pub fn new(
        plot_id: impl Into<String>,
        session_id: impl Into<String>,
    ) -> Result<Self, SessionContractError> {
        let context = Self {
            plot_id: plot_id.into(),
            session_id: session_id.into(),
        };
        validate_identity("plot_id", &context.plot_id)?;
        validate_identity("session_id", &context.session_id)?;
        Ok(context)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionContractError {
    LineTooLong {
        bytes: usize,
        max_bytes: usize,
    },
    Json(String),
    Kind {
        expected: &'static str,
        actual: String,
    },
    Identity {
        field: &'static str,
    },
    ContextMismatch {
        expected_plot_id: String,
        expected_session_id: String,
        actual_plot_id: String,
        actual_session_id: String,
    },
    MissingField {
        field: &'static str,
    },
    Sequence {
        actual: u64,
    },
    CursorChain {
        sequence: u64,
        has_previous: bool,
    },
    PageLimit {
        actual: u32,
        max: u32,
    },
    ReplayHead {
        sequence: u64,
        has_cursor: bool,
    },
    Retryability {
        code: SessionFeedErrorCode,
        expected: bool,
        actual: bool,
    },
    PartialContext,
    Message {
        reason: &'static str,
    },
}

impl fmt::Display for SessionContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::LineTooLong { bytes, max_bytes } => {
                write!(
                    formatter,
                    "Session line is {bytes} bytes; limit is {max_bytes}"
                )
            }
            Self::Json(error) => write!(formatter, "invalid Session JSON: {error}"),
            Self::Kind { expected, actual } => {
                write!(
                    formatter,
                    "expected Session kind {expected:?}, got {actual:?}"
                )
            }
            Self::Identity { field } => write!(formatter, "invalid Session identity {field}"),
            Self::ContextMismatch {
                expected_plot_id,
                expected_session_id,
                actual_plot_id,
                actual_session_id,
            } => write!(
                formatter,
                "expected Plot/Session {expected_plot_id:?}/{expected_session_id:?}, got {actual_plot_id:?}/{actual_session_id:?}"
            ),
            Self::MissingField { field } => {
                write!(
                    formatter,
                    "Session frame is missing required field {field:?}"
                )
            }
            Self::Sequence { actual } => {
                write!(
                    formatter,
                    "Session event sequence must be positive, got {actual}"
                )
            }
            Self::CursorChain {
                sequence,
                has_previous,
            } => write!(
                formatter,
                "Session event sequence {sequence} has invalid previous_cursor presence {has_previous}"
            ),
            Self::PageLimit { actual, max } => {
                write!(
                    formatter,
                    "Session replay page_limit {actual} is outside 1..={max}"
                )
            }
            Self::ReplayHead {
                sequence,
                has_cursor,
            } => write!(
                formatter,
                "Session replay head sequence {sequence} has invalid cursor presence {has_cursor}"
            ),
            Self::Retryability {
                code,
                expected,
                actual,
            } => write!(
                formatter,
                "Session feed error code {} requires retryable={expected}, got {actual}",
                code.wire_name()
            ),
            Self::PartialContext => write!(
                formatter,
                "Session feed error must provide both Plot and Session identity or neither"
            ),
            Self::Message { reason } => write!(formatter, "invalid Session text: {reason}"),
        }
    }
}

impl std::error::Error for SessionContractError {}

pub fn parse_session_command(line: &str) -> Result<SessionCommand, SessionContractError> {
    let value = parse_value(line)?;
    validate_kind(&value, SESSION_COMMAND_KIND)?;
    let command: SessionCommand = serde_json::from_value(value)
        .map_err(|error| SessionContractError::Json(error.to_string()))?;
    validate_identity("command_id", &command.command_id)?;
    validate_identity("plot_id", &command.plot_id)?;
    validate_identity("session_id", &command.session_id)?;
    let SessionCommandPayload::Prompt { text, .. } = &command.command;
    if text.trim().is_empty() {
        return Err(SessionContractError::Message {
            reason: "prompt text is empty or whitespace-only",
        });
    }
    Ok(command)
}

pub fn parse_session_event(line: &str) -> Result<SessionEvent, SessionContractError> {
    let value = parse_value(line)?;
    validate_kind(&value, SESSION_EVENT_KIND)?;
    let event: SessionEvent = serde_json::from_value(value)
        .map_err(|error| SessionContractError::Json(error.to_string()))?;
    validate_identity("event_id", &event.event_id)?;
    validate_identity("plot_id", &event.plot_id)?;
    validate_identity("session_id", &event.session_id)?;
    if let Some(command_id) = &event.command_id {
        validate_identity("command_id", command_id)?;
    }
    Ok(event)
}

pub fn parse_session_subscribe(line: &str) -> Result<SessionSubscribe, SessionContractError> {
    let value = parse_value(line)?;
    parse_session_subscribe_value(value)
}

pub fn parse_durable_session_event(
    line: &str,
) -> Result<DurableSessionEvent, SessionContractError> {
    let value = parse_value(line)?;
    parse_durable_session_event_value(value)
}

pub fn parse_session_replay_complete(
    line: &str,
) -> Result<SessionReplayComplete, SessionContractError> {
    let value = parse_value(line)?;
    parse_session_replay_complete_value(value)
}

pub fn parse_session_feed_error(line: &str) -> Result<SessionFeedError, SessionContractError> {
    let value = parse_value(line)?;
    parse_session_feed_error_value(value)
}

pub fn parse_session_server_frame(line: &str) -> Result<SessionServerFrame, SessionContractError> {
    let value = parse_value(line)?;
    match value.get("kind").and_then(serde_json::Value::as_str) {
        Some(DURABLE_SESSION_EVENT_KIND) => {
            parse_durable_session_event_value(value).map(SessionServerFrame::Event)
        }
        Some(SESSION_REPLAY_COMPLETE_KIND) => {
            parse_session_replay_complete_value(value).map(SessionServerFrame::ReplayComplete)
        }
        Some(SESSION_FEED_ERROR_KIND) => {
            parse_session_feed_error_value(value).map(SessionServerFrame::FeedError)
        }
        _ => Err(SessionContractError::Kind {
            expected: "a durable Session server frame kind",
            actual: actual_kind(&value),
        }),
    }
}

/// Parse frames advertised by a `nopal.session/v3` endpoint.
///
/// V3 endpoints preserve exact persisted v2 envelopes and may append typed v3
/// envelopes to the same stream. The existing v2 parser above intentionally
/// remains v2-only.
pub fn parse_session_v3_server_frame(
    line: &str,
) -> Result<SessionV3ServerFrame, SessionContractError> {
    let value = parse_value(line)?;
    match value.get("kind").and_then(serde_json::Value::as_str) {
        Some(DURABLE_SESSION_EVENT_KIND) => {
            parse_durable_session_event_value(value).map(SessionV3ServerFrame::Event)
        }
        Some(DURABLE_SESSION_ACTIVITY_EVENT_KIND) => {
            parse_session_activity_event_value(value).map(SessionV3ServerFrame::ActivityEvent)
        }
        Some(SESSION_REPLAY_COMPLETE_KIND) => {
            parse_session_replay_complete_value(value).map(SessionV3ServerFrame::ReplayComplete)
        }
        Some(SESSION_FEED_ERROR_KIND) => {
            parse_session_feed_error_value(value).map(SessionV3ServerFrame::FeedError)
        }
        _ => Err(SessionContractError::Kind {
            expected: "a v3 durable Session server frame kind",
            actual: actual_kind(&value),
        }),
    }
}

pub fn validate_command_context(
    command: &SessionCommand,
    expected: &ExpectedSessionContext,
) -> Result<(), SessionContractError> {
    validate_context(&command.plot_id, &command.session_id, expected)
}

pub fn validate_event_context(
    event: &SessionEvent,
    expected: &ExpectedSessionContext,
) -> Result<(), SessionContractError> {
    validate_context(&event.plot_id, &event.session_id, expected)
}

pub fn validate_durable_event_context(
    event: &DurableSessionEvent,
    expected: &ExpectedSessionContext,
) -> Result<(), SessionContractError> {
    validate_context(&event.plot_id, &event.session_id, expected)
}

pub fn validate_session_activity_event_context(
    event: &DurableSessionActivityEvent,
    expected: &ExpectedSessionContext,
) -> Result<(), SessionContractError> {
    validate_context(&event.plot_id, &event.session_id, expected)
}

pub fn validate_replay_complete_context(
    complete: &SessionReplayComplete,
    expected: &ExpectedSessionContext,
) -> Result<(), SessionContractError> {
    validate_context(&complete.plot_id, &complete.session_id, expected)
}

fn parse_session_subscribe_value(
    value: serde_json::Value,
) -> Result<SessionSubscribe, SessionContractError> {
    validate_kind(&value, SESSION_SUBSCRIBE_KIND)?;
    require_field(&value, "after_cursor")?;
    let subscribe: SessionSubscribe = serde_json::from_value(value)
        .map_err(|error| SessionContractError::Json(error.to_string()))?;
    validate_identity("request_id", &subscribe.request_id)?;
    validate_identity("plot_id", &subscribe.plot_id)?;
    validate_identity("session_id", &subscribe.session_id)?;
    if let Some(cursor) = &subscribe.after_cursor {
        validate_identity("after_cursor", cursor)?;
    }
    if subscribe.page_limit == 0 || subscribe.page_limit > MAX_REPLAY_PAGE_LIMIT {
        return Err(SessionContractError::PageLimit {
            actual: subscribe.page_limit,
            max: MAX_REPLAY_PAGE_LIMIT,
        });
    }
    Ok(subscribe)
}

fn parse_durable_session_event_value(
    value: serde_json::Value,
) -> Result<DurableSessionEvent, SessionContractError> {
    validate_kind(&value, DURABLE_SESSION_EVENT_KIND)?;
    require_field(&value, "previous_cursor")?;
    let event: DurableSessionEvent = serde_json::from_value(value)
        .map_err(|error| SessionContractError::Json(error.to_string()))?;
    validate_identity("event_id", &event.event_id)?;
    validate_identity("plot_id", &event.plot_id)?;
    validate_identity("session_id", &event.session_id)?;
    validate_identity("stream_id", &event.stream_id)?;
    validate_identity("cursor", &event.cursor)?;
    if let Some(command_id) = &event.command_id {
        validate_identity("command_id", command_id)?;
    }
    if let Some(previous_cursor) = &event.previous_cursor {
        validate_identity("previous_cursor", previous_cursor)?;
        if previous_cursor == &event.cursor {
            return Err(SessionContractError::CursorChain {
                sequence: event.sequence,
                has_previous: true,
            });
        }
    }
    if event.sequence == 0 {
        return Err(SessionContractError::Sequence { actual: 0 });
    }
    let should_have_previous = event.sequence > 1;
    if event.previous_cursor.is_some() != should_have_previous {
        return Err(SessionContractError::CursorChain {
            sequence: event.sequence,
            has_previous: event.previous_cursor.is_some(),
        });
    }
    Ok(event)
}

fn parse_session_replay_complete_value(
    value: serde_json::Value,
) -> Result<SessionReplayComplete, SessionContractError> {
    validate_kind(&value, SESSION_REPLAY_COMPLETE_KIND)?;
    require_field(&value, "cursor")?;
    let complete: SessionReplayComplete = serde_json::from_value(value)
        .map_err(|error| SessionContractError::Json(error.to_string()))?;
    validate_identity("request_id", &complete.request_id)?;
    validate_identity("plot_id", &complete.plot_id)?;
    validate_identity("session_id", &complete.session_id)?;
    validate_identity("stream_id", &complete.stream_id)?;
    if let Some(cursor) = &complete.cursor {
        validate_identity("cursor", cursor)?;
    }
    if (complete.sequence == 0) != complete.cursor.is_none()
        || complete.event_count > complete.sequence
    {
        return Err(SessionContractError::ReplayHead {
            sequence: complete.sequence,
            has_cursor: complete.cursor.is_some(),
        });
    }
    Ok(complete)
}

fn parse_session_feed_error_value(
    value: serde_json::Value,
) -> Result<SessionFeedError, SessionContractError> {
    validate_kind(&value, SESSION_FEED_ERROR_KIND)?;
    for field in ["request_id", "plot_id", "session_id"] {
        require_field(&value, field)?;
    }
    let error: SessionFeedError = serde_json::from_value(value)
        .map_err(|error| SessionContractError::Json(error.to_string()))?;
    let expected_retryable = error.code.canonical_retryable();
    if error.retryable != expected_retryable {
        return Err(SessionContractError::Retryability {
            code: error.code,
            expected: expected_retryable,
            actual: error.retryable,
        });
    }
    if let Some(request_id) = &error.request_id {
        validate_identity("request_id", request_id)?;
    }
    match (&error.plot_id, &error.session_id) {
        (Some(plot_id), Some(session_id)) => {
            validate_identity("plot_id", plot_id)?;
            validate_identity("session_id", session_id)?;
        }
        (None, None) => {}
        _ => return Err(SessionContractError::PartialContext),
    }
    if error.message.trim().is_empty() {
        return Err(SessionContractError::Message {
            reason: "message is empty",
        });
    }
    if error.message.len() > MAX_FEED_ERROR_MESSAGE_BYTES {
        return Err(SessionContractError::Message {
            reason: "message exceeds 4096 bytes",
        });
    }
    Ok(error)
}

const fn default_replay_page_limit() -> u32 {
    DEFAULT_REPLAY_PAGE_LIMIT
}

fn require_field(
    value: &serde_json::Value,
    field: &'static str,
) -> Result<(), SessionContractError> {
    if value
        .as_object()
        .is_some_and(|object| object.contains_key(field))
    {
        Ok(())
    } else {
        Err(SessionContractError::MissingField { field })
    }
}

fn parse_value(line: &str) -> Result<serde_json::Value, SessionContractError> {
    if line.len() > MAX_SESSION_LINE_BYTES {
        return Err(SessionContractError::LineTooLong {
            bytes: line.len(),
            max_bytes: MAX_SESSION_LINE_BYTES,
        });
    }
    serde_json::from_str(line).map_err(|error| SessionContractError::Json(error.to_string()))
}

fn validate_kind(
    value: &serde_json::Value,
    expected: &'static str,
) -> Result<(), SessionContractError> {
    let actual = actual_kind(value);
    if actual == expected {
        Ok(())
    } else {
        Err(SessionContractError::Kind { expected, actual })
    }
}

fn actual_kind(value: &serde_json::Value) -> String {
    value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .map(str::to_owned)
        .unwrap_or_else(|| {
            value
                .get("kind")
                .map(serde_json::Value::to_string)
                .unwrap_or_else(|| "<missing>".to_owned())
        })
}

fn validate_identity(field: &'static str, value: &str) -> Result<(), SessionContractError> {
    if value.trim().is_empty()
        || value.len() > MAX_SESSION_IDENTITY_BYTES
        || value.chars().any(char::is_control)
    {
        Err(SessionContractError::Identity { field })
    } else {
        Ok(())
    }
}

fn validate_context(
    actual_plot_id: &str,
    actual_session_id: &str,
    expected: &ExpectedSessionContext,
) -> Result<(), SessionContractError> {
    if actual_plot_id == expected.plot_id && actual_session_id == expected.session_id {
        Ok(())
    } else {
        Err(SessionContractError::ContextMismatch {
            expected_plot_id: expected.plot_id.clone(),
            expected_session_id: expected.session_id.clone(),
            actual_plot_id: actual_plot_id.to_owned(),
            actual_session_id: actual_session_id.to_owned(),
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        DEFAULT_REPLAY_PAGE_LIMIT, ExpectedSessionContext, MAX_REPLAY_PAGE_LIMIT,
        MAX_SESSION_IDENTITY_BYTES, MAX_SESSION_LINE_BYTES, SessionCommandPayload,
        SessionContractError, SessionEventPayload, SessionFeedErrorCode, SessionServerFrame,
        SessionV3ServerFrame, parse_durable_session_event, parse_session_command,
        parse_session_event, parse_session_server_frame, parse_session_subscribe,
        parse_session_v3_server_frame, validate_command_context, validate_durable_event_context,
        validate_event_context, validate_session_activity_event_context,
    };

    const PLOT_ID: &str = "plot-01";
    const SESSION_ID: &str = "session-01";

    fn prompt() -> serde_json::Value {
        json!({
            "kind": "nopal.session.command/v1",
            "command_id": "command-01",
            "plot_id": PLOT_ID,
            "session_id": SESSION_ID,
            "command": {
                "type": "prompt",
                "text": "Explain the failing test",
                "future_command_fact": true
            },
            "future_envelope_fact": {"version": 2}
        })
    }

    fn assistant_event() -> serde_json::Value {
        json!({
            "kind": "nopal.session.event/v1",
            "event_id": "event-02",
            "plot_id": PLOT_ID,
            "session_id": SESSION_ID,
            "command_id": "command-01",
            "event": {
                "type": "assistant_message",
                "text": "The assertion is inverted.",
                "future_event_fact": ["citation-01"]
            },
            "future_envelope_fact": {"cursor": 2}
        })
    }

    #[test]
    fn parses_prompt_and_preserves_additive_fields() {
        let command = parse_session_command(&prompt().to_string()).expect("valid prompt");

        assert_eq!(command.command_id, "command-01");
        assert_eq!(command.extra["future_envelope_fact"]["version"], 2);
        let SessionCommandPayload::Prompt { text, extra } = command.command;
        assert_eq!(text, "Explain the failing test");
        assert_eq!(extra["future_command_fact"], true);
    }

    #[test]
    fn parses_every_event_variant_and_preserves_additive_fields() {
        let cases = [
            (
                json!({"type": "session_ready", "capability": "structured"}),
                "ready",
            ),
            (json!({"type": "user_message", "text": "Question"}), "user"),
            (
                json!({"type": "assistant_message", "text": "Answer"}),
                "assistant",
            ),
            (
                json!({"type": "session_error", "message": "Pi stopped"}),
                "error",
            ),
        ];

        for (event_payload, expected) in cases {
            let value = json!({
                "kind": "nopal.session.event/v1",
                "event_id": format!("event-{expected}"),
                "plot_id": PLOT_ID,
                "session_id": SESSION_ID,
                "event": event_payload,
                "future_envelope_fact": expected
            });
            let event = parse_session_event(&value.to_string()).expect("valid event");
            assert_eq!(event.extra["future_envelope_fact"], expected);
            match (event.event, expected) {
                (SessionEventPayload::SessionReady { extra }, "ready") => {
                    assert_eq!(extra["capability"], "structured");
                }
                (SessionEventPayload::UserMessage { text, .. }, "user") => {
                    assert_eq!(text, "Question");
                }
                (SessionEventPayload::AssistantMessage { text, .. }, "assistant") => {
                    assert_eq!(text, "Answer");
                }
                (SessionEventPayload::SessionError { message, .. }, "error") => {
                    assert_eq!(message, "Pi stopped");
                }
                (actual, _) => panic!("unexpected event variant: {actual:?}"),
            }
        }
    }

    #[test]
    fn event_command_identity_is_optional() {
        let ready = json!({
            "kind": "nopal.session.event/v1",
            "event_id": "event-ready",
            "plot_id": PLOT_ID,
            "session_id": SESSION_ID,
            "event": {"type": "session_ready"}
        });
        assert_eq!(
            parse_session_event(&ready.to_string())
                .expect("ready event")
                .command_id,
            None
        );
        assert_eq!(
            parse_session_event(&assistant_event().to_string())
                .expect("assistant event")
                .command_id
                .as_deref(),
            Some("command-01")
        );
    }

    #[test]
    fn rejects_wrong_or_stale_wire_kinds() {
        for kind in ["nopal.session.command/v2", "nopal.session.event/v1", ""] {
            let mut value = prompt();
            value["kind"] = json!(kind);
            assert!(matches!(
                parse_session_command(&value.to_string()),
                Err(SessionContractError::Kind { .. })
            ));
        }
        for kind in ["nopal.session.event/v2", "nopal.session.command/v1", ""] {
            let mut value = assistant_event();
            value["kind"] = json!(kind);
            assert!(matches!(
                parse_session_event(&value.to_string()),
                Err(SessionContractError::Kind { .. })
            ));
        }
    }

    #[test]
    fn rejects_empty_whitespace_or_control_bearing_identities() {
        for field in ["command_id", "plot_id", "session_id"] {
            for invalid in ["", "   ", "bad\nid"] {
                let mut value = prompt();
                value[field] = json!(invalid);
                assert_eq!(
                    parse_session_command(&value.to_string()),
                    Err(SessionContractError::Identity { field })
                );
            }
        }
        for field in ["event_id", "plot_id", "session_id", "command_id"] {
            for invalid in ["", "\t", "bad\u{7f}id"] {
                let mut value = assistant_event();
                value[field] = json!(invalid);
                assert_eq!(
                    parse_session_event(&value.to_string()),
                    Err(SessionContractError::Identity { field })
                );
            }
        }
    }

    #[test]
    fn rejects_lines_over_the_transport_limit_before_json_parsing() {
        let oversized = " ".repeat(MAX_SESSION_LINE_BYTES + 1);
        assert_eq!(
            parse_session_command(&oversized),
            Err(SessionContractError::LineTooLong {
                bytes: MAX_SESSION_LINE_BYTES + 1,
                max_bytes: MAX_SESSION_LINE_BYTES,
            })
        );
        assert!(matches!(
            parse_session_event("not json"),
            Err(SessionContractError::Json(_))
        ));
    }

    #[test]
    fn expected_context_rejects_cross_plot_and_cross_session_messages() {
        let expected = ExpectedSessionContext::new(PLOT_ID, SESSION_ID).expect("valid context");
        let command = parse_session_command(&prompt().to_string()).expect("valid prompt");
        let event = parse_session_event(&assistant_event().to_string()).expect("valid event");
        assert_eq!(validate_command_context(&command, &expected), Ok(()));
        assert_eq!(validate_event_context(&event, &expected), Ok(()));

        let other_plot = ExpectedSessionContext::new("plot-02", SESSION_ID).expect("valid context");
        assert_eq!(
            validate_event_context(&event, &other_plot),
            Err(SessionContractError::ContextMismatch {
                expected_plot_id: "plot-02".to_owned(),
                expected_session_id: SESSION_ID.to_owned(),
                actual_plot_id: PLOT_ID.to_owned(),
                actual_session_id: SESSION_ID.to_owned(),
            })
        );
        let other_session =
            ExpectedSessionContext::new(PLOT_ID, "session-02").expect("valid context");
        assert!(matches!(
            validate_command_context(&command, &other_session),
            Err(SessionContractError::ContextMismatch { .. })
        ));
    }

    #[test]
    fn expected_context_itself_must_have_safe_identities() {
        assert_eq!(
            ExpectedSessionContext::new("", SESSION_ID),
            Err(SessionContractError::Identity { field: "plot_id" })
        );
        assert_eq!(
            ExpectedSessionContext::new(PLOT_ID, "bad\nsession"),
            Err(SessionContractError::Identity {
                field: "session_id"
            })
        );
    }

    #[test]
    fn checked_in_conformance_fixtures_match_the_contract() {
        let command_line =
            include_str!("../../../conformance/surface/session/valid-prompt-command.jsonl");
        let command = parse_session_command(command_line).expect("valid command fixture");
        let expected = ExpectedSessionContext::new("plot-fixture", "session-fixture")
            .expect("valid fixture context");
        validate_command_context(&command, &expected).expect("matching command identity");

        let blank_prompt =
            include_str!("../../../conformance/surface/session/invalid-prompt-text.jsonl");
        assert!(matches!(
            parse_session_command(blank_prompt),
            Err(SessionContractError::Message { .. })
        ));

        let events = include_str!("../../../conformance/surface/session/valid-events.jsonl");
        let parsed = events
            .lines()
            .map(|line| parse_session_event(line).expect("valid event fixture"))
            .collect::<Vec<_>>();
        assert_eq!(parsed.len(), 4);
        for event in &parsed {
            validate_event_context(event, &expected).expect("matching event identity");
        }

        let stale_kind =
            include_str!("../../../conformance/surface/session/invalid-command-kind.jsonl");
        assert!(matches!(
            parse_session_command(stale_kind),
            Err(SessionContractError::Kind { .. })
        ));

        let foreign =
            include_str!("../../../conformance/surface/session/foreign-session-event.jsonl");
        let foreign = parse_session_event(foreign).expect("structurally valid foreign event");
        assert!(matches!(
            validate_event_context(&foreign, &expected),
            Err(SessionContractError::ContextMismatch { .. })
        ));
    }

    #[test]
    fn shared_identity_fixtures_freeze_the_utf8_byte_boundary() {
        let lines = include_str!("../../../conformance/surface/session/identity-bounds-v1.jsonl")
            .lines()
            .collect::<Vec<_>>();
        let Ok(at_limit) = serde_json::from_str::<serde_json::Value>(lines[0]) else {
            panic!("at-limit fixture must be valid JSON");
        };
        let Ok(beyond_limit) = serde_json::from_str::<serde_json::Value>(lines[1]) else {
            panic!("beyond-limit fixture must be valid JSON");
        };
        let Some(at_limit_identity) = at_limit["command_id"].as_str() else {
            panic!("at-limit fixture must contain a command identity");
        };
        let Some(beyond_limit_identity) = beyond_limit["command_id"].as_str() else {
            panic!("beyond-limit fixture must contain a command identity");
        };
        assert_eq!(at_limit_identity.len(), MAX_SESSION_IDENTITY_BYTES);
        assert_eq!(beyond_limit_identity.len(), MAX_SESSION_IDENTITY_BYTES + 1);
        assert!(parse_session_command(lines[0]).is_ok());
        assert_eq!(
            parse_session_command(lines[1]),
            Err(SessionContractError::Identity {
                field: "command_id"
            })
        );
    }

    #[test]
    fn cold_replay_fixture_parses_into_strict_typed_frames() {
        let lines = include_str!("../../../conformance/surface/session/cold-replay-v2.jsonl")
            .lines()
            .collect::<Vec<_>>();

        let subscribe = parse_session_subscribe(lines[0]).expect("valid cold subscribe");
        assert_eq!(subscribe.request_id, "request-cold");
        assert_eq!(subscribe.after_cursor, None);
        assert_eq!(subscribe.page_limit, DEFAULT_REPLAY_PAGE_LIMIT);
        assert_eq!(subscribe.extra["future_subscribe_fact"], true);

        let first = parse_session_server_frame(lines[1]).expect("valid first durable event");
        let SessionServerFrame::Event(first) = first else {
            panic!("expected durable event frame");
        };
        assert_eq!(first.stream_id, "stream-fixture");
        assert_eq!(first.sequence, 1);
        assert_eq!(first.previous_cursor, None);
        assert_eq!(first.cursor, "cursor-fixture-1");
        assert_eq!(first.extra["future_event_fact"]["source"], "active-branch");

        for line in &lines[2..4] {
            assert!(matches!(
                parse_session_server_frame(line),
                Ok(SessionServerFrame::Event(_))
            ));
        }
        let complete = parse_session_server_frame(lines[4]).expect("valid completion");
        let SessionServerFrame::ReplayComplete(complete) = complete else {
            panic!("expected replay completion frame");
        };
        assert_eq!(complete.request_id, "request-cold");
        assert_eq!(complete.cursor.as_deref(), Some("cursor-fixture-3"));
        assert_eq!(complete.sequence, 3);
        assert_eq!(complete.event_count, 3);
        assert_eq!(complete.extra["future_completion_fact"], true);
    }

    #[test]
    fn v3_endpoint_parses_an_exact_v2_prefix_and_typed_v3_suffix() {
        use crate::session_activity::{CommandExit, CommandOutcome, SessionActivityEventPayload};

        let lines = include_str!("../../../conformance/surface/session/mixed-replay-v3.jsonl")
            .lines()
            .collect::<Vec<_>>();

        let subscribe = parse_session_subscribe(lines[0]).expect("valid mixed subscribe");
        assert_eq!(subscribe.after_cursor, None);

        let SessionV3ServerFrame::Event(ready) =
            parse_session_v3_server_frame(lines[1]).expect("exact v2 ready")
        else {
            panic!("expected exact v2 event");
        };
        assert_eq!(ready.kind, "nopal.session.event/v2");
        assert_eq!(
            ready.cursor,
            "nopal.session.cursor/v1:7d2523cb87216913e043db7747ba4f2686387cb54eb0c4192768fb442948956b:1:d877de0223d49fc967a4b240b3b8fbc842b603700be4be0b279f5feb95d2eabe"
        );
        assert_eq!(ready.extra["persisted_v2_fact"]["must_remain"], true);

        let SessionV3ServerFrame::Event(user) =
            parse_session_v3_server_frame(lines[2]).expect("exact v2 user")
        else {
            panic!("expected exact v2 event");
        };
        assert_eq!(user.sequence, 2);
        assert_eq!(
            user.cursor,
            "nopal.session.cursor/v1:7d2523cb87216913e043db7747ba4f2686387cb54eb0c4192768fb442948956b:2:18d80e4953035e41eca8498a1b984675792ae39a0a3afba4beaacdcba5ea1fea"
        );

        let SessionV3ServerFrame::ActivityEvent(started) =
            parse_session_v3_server_frame(lines[3]).expect("typed v3 command start")
        else {
            panic!("expected v3 activity event");
        };
        assert_eq!(started.sequence, 3);
        assert_eq!(
            started.previous_cursor.as_deref(),
            Some(user.cursor.as_str())
        );
        assert_eq!(started.extra["future_activity_fact"]["source"], "pi-hook");
        let expected = ExpectedSessionContext::new("plot-fixture", "session-fixture")
            .expect("valid mixed context");
        validate_session_activity_event_context(&started, &expected).expect("matching v3 context");
        let foreign = ExpectedSessionContext::new("plot-fixture", "session-foreign")
            .expect("valid foreign context");
        assert!(matches!(
            validate_session_activity_event_context(&started, &foreign),
            Err(SessionContractError::ContextMismatch { .. })
        ));
        let SessionActivityEventPayload::CommandStarted {
            activity_id,
            tool_call_id,
            command,
            ..
        } = started.event
        else {
            panic!("expected command start payload");
        };
        assert_eq!(activity_id, "activity-shell-01");
        assert_eq!(tool_call_id, "tool-call-shell-01");
        assert_eq!(command, "cargo test -p nopal-feed-client");

        let SessionV3ServerFrame::ActivityEvent(finished) =
            parse_session_v3_server_frame(lines[4]).expect("typed v3 command finish")
        else {
            panic!("expected v3 activity event");
        };
        assert_eq!(finished.sequence, 4);
        assert_eq!(
            finished.previous_cursor.as_deref(),
            Some(
                "nopal.session.cursor/v1:7d2523cb87216913e043db7747ba4f2686387cb54eb0c4192768fb442948956b:3:cce61ef61e86c06af28dffeb0ba941061e7056691ab48af79c2dced3e2e9ae17"
            )
        );
        let SessionActivityEventPayload::CommandFinished {
            duration_ms,
            exit,
            outcome,
            output,
            ..
        } = finished.event
        else {
            panic!("expected command finish payload");
        };
        assert_eq!(duration_ms, 418);
        assert_eq!(exit, CommandExit::Code { code: 0 });
        assert_eq!(outcome, CommandOutcome::Succeeded);
        assert_eq!(output.expect("bounded output").text, "test result: ok");

        assert!(matches!(
            parse_session_v3_server_frame(lines[5]),
            Ok(SessionV3ServerFrame::ReplayComplete(_))
        ));
        assert!(matches!(
            parse_session_server_frame(lines[3]),
            Err(SessionContractError::Kind { .. })
        ));
    }

    #[test]
    fn v3_endpoint_resumes_strictly_after_an_old_v2_cursor() {
        let lines = include_str!("../../../conformance/surface/session/resume-mixed-v3.jsonl")
            .lines()
            .collect::<Vec<_>>();
        let subscribe = parse_session_subscribe(lines[0]).expect("valid v2 cursor resume");
        let old_v2_cursor = subscribe.after_cursor.expect("old verified v2 cursor");

        let SessionV3ServerFrame::ActivityEvent(first) =
            parse_session_v3_server_frame(lines[1]).expect("first v3 suffix event")
        else {
            panic!("expected v3 activity event");
        };
        assert_eq!(first.sequence, 3);
        assert_eq!(
            first.previous_cursor.as_deref(),
            Some(old_v2_cursor.as_str())
        );
        assert!(matches!(
            parse_session_v3_server_frame(lines[2]),
            Ok(SessionV3ServerFrame::ActivityEvent(_))
        ));
        let SessionV3ServerFrame::ReplayComplete(complete) =
            parse_session_v3_server_frame(lines[3]).expect("resume completion")
        else {
            panic!("expected replay completion");
        };
        assert_eq!(complete.sequence, 4);
        assert_eq!(complete.event_count, 2);
    }

    #[test]
    fn resume_and_exact_duplicate_overlap_preserve_stable_durable_identity() {
        let resume = include_str!("../../../conformance/surface/session/resume-replay-v2.jsonl")
            .lines()
            .collect::<Vec<_>>();
        let subscribe = parse_session_subscribe(resume[0]).expect("valid resume subscribe");
        assert_eq!(subscribe.after_cursor.as_deref(), Some("cursor-fixture-2"));
        assert_eq!(subscribe.page_limit, 64);
        assert!(matches!(
            parse_session_server_frame(resume[1]),
            Ok(SessionServerFrame::Event(_))
        ));
        assert!(matches!(
            parse_session_server_frame(resume[2]),
            Ok(SessionServerFrame::ReplayComplete(_))
        ));

        let duplicate =
            include_str!("../../../conformance/surface/session/duplicate-overlap-v2.jsonl")
                .lines()
                .map(|line| parse_durable_session_event(line).expect("valid overlap event"))
                .collect::<Vec<_>>();
        assert_eq!(duplicate[0], duplicate[1]);

        let conflict =
            include_str!("../../../conformance/surface/session/cursor-conflict-v2.jsonl")
                .lines()
                .map(|line| {
                    parse_durable_session_event(line).expect("individually valid conflict event")
                })
                .collect::<Vec<_>>();
        assert_eq!(conflict[0].cursor, conflict[1].cursor);
        assert_ne!(conflict[0], conflict[1]);
    }

    #[test]
    fn durable_event_requires_stream_sequence_and_explicit_cursor_chain() {
        let valid = serde_json::json!({
            "kind": "nopal.session.event/v2",
            "event_id": "event-01",
            "plot_id": PLOT_ID,
            "session_id": SESSION_ID,
            "stream_id": "stream-01",
            "sequence": 1,
            "previous_cursor": null,
            "cursor": "cursor-01",
            "event": {"type": "session_ready"}
        });
        assert!(parse_durable_session_event(&valid.to_string()).is_ok());

        let mut zero = valid.clone();
        zero["sequence"] = json!(0);
        assert!(matches!(
            parse_durable_session_event(&zero.to_string()),
            Err(SessionContractError::Sequence { .. })
        ));

        let mut first_with_previous = valid.clone();
        first_with_previous["previous_cursor"] = json!("cursor-00");
        assert!(matches!(
            parse_durable_session_event(&first_with_previous.to_string()),
            Err(SessionContractError::CursorChain { .. })
        ));

        let mut later_without_previous = valid.clone();
        later_without_previous["sequence"] = json!(2);
        assert!(matches!(
            parse_durable_session_event(&later_without_previous.to_string()),
            Err(SessionContractError::CursorChain { .. })
        ));

        let mut missing_nullable_field = valid;
        missing_nullable_field
            .as_object_mut()
            .expect("object")
            .remove("previous_cursor");
        assert_eq!(
            parse_durable_session_event(&missing_nullable_field.to_string()),
            Err(SessionContractError::MissingField {
                field: "previous_cursor"
            })
        );
    }

    #[test]
    fn foreign_durable_event_is_valid_data_but_fails_context_binding() {
        let line =
            include_str!("../../../conformance/surface/session/foreign-durable-event-v2.jsonl");
        let event = parse_durable_session_event(line).expect("structurally valid foreign event");
        let expected = ExpectedSessionContext::new("plot-fixture", "session-fixture")
            .expect("valid expected context");
        assert!(matches!(
            validate_durable_event_context(&event, &expected),
            Err(SessionContractError::ContextMismatch { .. })
        ));
    }

    #[test]
    fn malformed_control_fixtures_fail_closed() {
        let lines =
            include_str!("../../../conformance/surface/session/malformed-controls-v1.jsonl")
                .lines()
                .collect::<Vec<_>>();
        assert_eq!(
            parse_session_subscribe(lines[0]),
            Err(SessionContractError::MissingField {
                field: "after_cursor"
            })
        );
        assert_eq!(
            parse_session_subscribe(lines[1]),
            Err(SessionContractError::PageLimit {
                actual: MAX_REPLAY_PAGE_LIMIT + 1,
                max: MAX_REPLAY_PAGE_LIMIT,
            })
        );
        assert!(matches!(
            parse_session_server_frame(lines[2]),
            Err(SessionContractError::ReplayHead { .. })
        ));
        assert!(matches!(
            parse_session_server_frame(lines[3]),
            Err(SessionContractError::PartialContext)
        ));
    }

    #[test]
    fn subscribe_defaults_page_size_and_empty_replay_has_an_explicit_null_head() {
        let subscribe = json!({
            "kind": "nopal.session.subscribe/v1",
            "request_id": "request-empty",
            "plot_id": PLOT_ID,
            "session_id": SESSION_ID,
            "after_cursor": null
        });
        assert_eq!(
            parse_session_subscribe(&subscribe.to_string())
                .expect("valid defaulted subscribe")
                .page_limit,
            DEFAULT_REPLAY_PAGE_LIMIT
        );

        let complete = json!({
            "kind": "nopal.session.replay_complete/v1",
            "request_id": "request-empty",
            "plot_id": PLOT_ID,
            "session_id": SESSION_ID,
            "stream_id": "stream-empty",
            "cursor": null,
            "sequence": 0,
            "event_count": 0
        });
        let SessionServerFrame::ReplayComplete(complete) =
            parse_session_server_frame(&complete.to_string()).expect("valid empty replay")
        else {
            panic!("expected completion");
        };
        assert_eq!(complete.cursor, None);
        assert_eq!(complete.sequence, 0);
    }

    #[test]
    fn server_frame_dispatch_rejects_non_server_kinds_without_panicking() {
        assert!(matches!(
            parse_session_server_frame(&prompt().to_string()),
            Err(SessionContractError::Kind { .. })
        ));
        assert!(matches!(
            parse_session_server_frame(r#"{"kind":"a durable Session server frame kind"}"#),
            Err(SessionContractError::Kind { .. })
        ));
    }

    #[test]
    fn feed_errors_are_operational_typed_frames_with_bounded_messages() {
        let errors = include_str!("../../../conformance/surface/session/feed-errors-v1.jsonl")
            .lines()
            .map(|line| parse_session_server_frame(line).expect("valid feed error"))
            .collect::<Vec<_>>();
        let SessionServerFrame::FeedError(gap) = &errors[0] else {
            panic!("expected feed error");
        };
        assert_eq!(gap.code, SessionFeedErrorCode::HistoryGap);
        assert!(!gap.retryable);
        assert_eq!(gap.extra["future_error_fact"], true);
        let SessionServerFrame::FeedError(unavailable) = &errors[1] else {
            panic!("expected feed error");
        };
        assert_eq!(unavailable.code, SessionFeedErrorCode::Unavailable);
        assert!(unavailable.retryable);
        assert_eq!(unavailable.plot_id, None);
        assert_eq!(unavailable.session_id, None);
        let SessionServerFrame::FeedError(command_conflict) = &errors[2] else {
            panic!("expected command conflict feed error");
        };
        assert_eq!(command_conflict.code, SessionFeedErrorCode::CommandConflict);
        assert!(!command_conflict.retryable);
        let SessionServerFrame::FeedError(replay_overflow) = &errors[3] else {
            panic!("expected replay overflow feed error");
        };
        assert_eq!(
            replay_overflow.code,
            SessionFeedErrorCode::ReplayBufferOverflow
        );
        assert!(replay_overflow.retryable);

        let oversized = json!({
            "kind": "nopal.session.feed_error/v1",
            "request_id": null,
            "plot_id": null,
            "session_id": null,
            "code": "internal",
            "retryable": false,
            "message": "x".repeat(super::MAX_FEED_ERROR_MESSAGE_BYTES + 1)
        });
        assert!(matches!(
            parse_session_server_frame(&oversized.to_string()),
            Err(SessionContractError::Message { .. })
        ));
    }

    #[test]
    fn terminal_feed_error_codes_reject_retryable_true() {
        for (code, expected_code) in [
            ("history_gap", SessionFeedErrorCode::HistoryGap),
            ("history_corrupt", SessionFeedErrorCode::HistoryCorrupt),
            ("foreign_session", SessionFeedErrorCode::ForeignSession),
            ("branch_diverged", SessionFeedErrorCode::BranchDiverged),
            ("history_too_large", SessionFeedErrorCode::HistoryTooLarge),
            ("cursor_conflict", SessionFeedErrorCode::CursorConflict),
            ("command_conflict", SessionFeedErrorCode::CommandConflict),
            (
                "protocol_violation",
                SessionFeedErrorCode::ProtocolViolation,
            ),
            ("internal", SessionFeedErrorCode::Internal),
        ] {
            let frame = json!({
                "kind": "nopal.session.feed_error/v1",
                "request_id": "request-contradiction",
                "plot_id": PLOT_ID,
                "session_id": SESSION_ID,
                "code": code,
                "retryable": true,
                "message": "contradictory terminal error"
            });

            let error = parse_session_server_frame(&frame.to_string())
                .expect_err("terminal feed error must reject retryable=true");
            assert_eq!(
                error,
                SessionContractError::Retryability {
                    code: expected_code,
                    expected: false,
                    actual: true,
                }
            );
            assert!(
                error.to_string().contains("retryable"),
                "retryability contradiction was unclear for {code}: {error}"
            );
        }
    }

    #[test]
    fn retryable_feed_error_codes_reject_retryable_false() {
        for (code, expected_code) in [
            ("unavailable", SessionFeedErrorCode::Unavailable),
            (
                "replay_buffer_overflow",
                SessionFeedErrorCode::ReplayBufferOverflow,
            ),
        ] {
            let frame = json!({
                "kind": "nopal.session.feed_error/v1",
                "request_id": "request-contradiction",
                "plot_id": PLOT_ID,
                "session_id": SESSION_ID,
                "code": code,
                "retryable": false,
                "message": "contradictory retryable error"
            });

            let error = parse_session_server_frame(&frame.to_string())
                .expect_err("retryable feed error must reject retryable=false");
            assert_eq!(
                error,
                SessionContractError::Retryability {
                    code: expected_code,
                    expected: true,
                    actual: false,
                }
            );
            assert!(
                error.to_string().contains("retryable"),
                "retryability contradiction was unclear for {code}: {error}"
            );
        }
    }

    #[test]
    fn every_v2_parser_rejects_oversize_lines_before_json() {
        let oversized = " ".repeat(MAX_SESSION_LINE_BYTES + 1);
        for result in [
            parse_session_subscribe(&oversized).map(|_| ()),
            parse_durable_session_event(&oversized).map(|_| ()),
            parse_session_server_frame(&oversized).map(|_| ()),
        ] {
            assert_eq!(
                result,
                Err(SessionContractError::LineTooLong {
                    bytes: MAX_SESSION_LINE_BYTES + 1,
                    max_bytes: MAX_SESSION_LINE_BYTES,
                })
            );
        }
    }
}
