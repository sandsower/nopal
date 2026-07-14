//! Typed v3 durable Session activity envelopes.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::session::{MAX_SESSION_IDENTITY_BYTES, MAX_SESSION_LINE_BYTES, SessionContractError};

pub const DURABLE_SESSION_ACTIVITY_EVENT_KIND: &str = "nopal.session.event/v3";
pub const MAX_TOOL_NAME_BYTES: usize = 256;
pub const MAX_ACTIVITY_DISPLAY_BYTES: usize = 8192;
pub const MAX_ACTIVITY_FAILURE_BYTES: usize = 4096;
pub const MAX_ACTIVITY_OUTPUT_BYTES: usize = 32768;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurableSessionActivityEvent {
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
    pub event: SessionActivityEventPayload,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionActivityEventPayload {
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
    CommandStarted {
        activity_id: String,
        tool_call_id: String,
        command: String,
        started_at: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        working_directory: Option<String>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    CommandFinished {
        activity_id: String,
        tool_call_id: String,
        duration_ms: u64,
        exit: CommandExit,
        outcome: CommandOutcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output: Option<ActivityOutput>,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    CommandFailed {
        activity_id: String,
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        message: String,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    ToolStarted {
        activity_id: String,
        tool_call_id: String,
        tool_name: String,
        summary: ActivitySummary,
        started_at: String,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    ToolFinished {
        activity_id: String,
        tool_call_id: String,
        duration_ms: u64,
        outcome: ToolOutcome,
        summary: ActivitySummary,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
    ToolFailed {
        activity_id: String,
        tool_call_id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration_ms: Option<u64>,
        message: String,
        outcome: ToolFailureOutcome,
        #[serde(flatten)]
        extra: BTreeMap<String, serde_json::Value>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CommandExit {
    Code { code: i32 },
    Signal { signal: String },
    Unavailable { reason: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandOutcome {
    Succeeded,
    Failed,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Succeeded,
    Cancelled,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolFailureOutcome {
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityOutputChannel {
    Stdout,
    Stderr,
    Combined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityOutput {
    pub channel: ActivityOutputChannel,
    pub text: String,
    pub truncated: bool,
    pub original_bytes: u64,
    pub omitted_bytes: u64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivitySummary {
    pub text: String,
    #[serde(default)]
    pub details_unavailable: bool,
    pub truncated: bool,
    pub original_bytes: u64,
    pub omitted_bytes: u64,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

pub fn parse_session_activity_event(
    line: &str,
) -> Result<DurableSessionActivityEvent, SessionContractError> {
    if line.len() > MAX_SESSION_LINE_BYTES {
        return Err(SessionContractError::LineTooLong {
            bytes: line.len(),
            max_bytes: MAX_SESSION_LINE_BYTES,
        });
    }
    let value = serde_json::from_str(line)
        .map_err(|error| SessionContractError::Json(error.to_string()))?;
    parse_session_activity_event_value(value)
}

pub(crate) fn parse_session_activity_event_value(
    value: serde_json::Value,
) -> Result<DurableSessionActivityEvent, SessionContractError> {
    let actual = value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("<missing>");
    if actual != DURABLE_SESSION_ACTIVITY_EVENT_KIND {
        return Err(SessionContractError::Kind {
            expected: DURABLE_SESSION_ACTIVITY_EVENT_KIND,
            actual: actual.to_owned(),
        });
    }
    if !value
        .as_object()
        .is_some_and(|object| object.contains_key("previous_cursor"))
    {
        return Err(SessionContractError::MissingField {
            field: "previous_cursor",
        });
    }
    let event: DurableSessionActivityEvent = serde_json::from_value(value)
        .map_err(|error| SessionContractError::Json(error.to_string()))?;
    for (field, identity) in [
        ("event_id", event.event_id.as_str()),
        ("plot_id", event.plot_id.as_str()),
        ("session_id", event.session_id.as_str()),
        ("stream_id", event.stream_id.as_str()),
        ("cursor", event.cursor.as_str()),
    ] {
        validate_identity(field, identity)?;
    }
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
    if event.previous_cursor.is_some() != (event.sequence > 1) {
        return Err(SessionContractError::CursorChain {
            sequence: event.sequence,
            has_previous: event.previous_cursor.is_some(),
        });
    }
    validate_payload(&event.event)?;
    Ok(event)
}

fn validate_payload(event: &SessionActivityEventPayload) -> Result<(), SessionContractError> {
    use SessionActivityEventPayload::{
        AssistantMessage, CommandFailed, CommandFinished, CommandStarted, SessionError,
        SessionReady, ToolFailed, ToolFinished, ToolStarted, UserMessage,
    };

    match event {
        SessionReady { .. }
        | UserMessage { .. }
        | AssistantMessage { .. }
        | SessionError { .. } => Ok(()),
        CommandStarted {
            activity_id,
            tool_call_id,
            command,
            started_at,
            working_directory,
            ..
        } => {
            validate_activity_identities(activity_id, tool_call_id)?;
            validate_text(command, MAX_ACTIVITY_DISPLAY_BYTES, "command display")?;
            validate_text(started_at, MAX_ACTIVITY_DISPLAY_BYTES, "producer timestamp")?;
            if let Some(working_directory) = working_directory {
                validate_text(
                    working_directory,
                    MAX_ACTIVITY_DISPLAY_BYTES,
                    "working directory",
                )?;
            }
            Ok(())
        }
        CommandFinished {
            activity_id,
            tool_call_id,
            exit,
            output,
            ..
        } => {
            validate_activity_identities(activity_id, tool_call_id)?;
            match exit {
                CommandExit::Code { .. } => {}
                CommandExit::Signal { signal } => {
                    validate_text(signal, MAX_ACTIVITY_FAILURE_BYTES, "exit signal")?;
                }
                CommandExit::Unavailable { reason } => {
                    validate_text(
                        reason,
                        MAX_ACTIVITY_FAILURE_BYTES,
                        "unavailable exit reason",
                    )?;
                }
            }
            if let Some(output) = output {
                validate_bounded_text(
                    &output.text,
                    output.truncated,
                    output.original_bytes,
                    output.omitted_bytes,
                    MAX_ACTIVITY_OUTPUT_BYTES,
                    "command output",
                )?;
            }
            Ok(())
        }
        CommandFailed {
            activity_id,
            tool_call_id,
            message,
            ..
        } => {
            validate_activity_identities(activity_id, tool_call_id)?;
            validate_text(
                message,
                MAX_ACTIVITY_FAILURE_BYTES,
                "activity failure message",
            )
        }
        ToolFailed {
            activity_id,
            tool_call_id,
            message,
            extra,
            ..
        } => {
            reject_raw_tool_payload(extra)?;
            validate_activity_identities(activity_id, tool_call_id)?;
            validate_text(
                message,
                MAX_ACTIVITY_FAILURE_BYTES,
                "activity failure message",
            )
        }
        ToolStarted {
            activity_id,
            tool_call_id,
            tool_name,
            summary,
            started_at,
            extra,
            ..
        } => {
            reject_raw_tool_payload(extra)?;
            validate_activity_identities(activity_id, tool_call_id)?;
            validate_text(tool_name, MAX_TOOL_NAME_BYTES, "tool name")?;
            validate_summary(summary)?;
            validate_text(started_at, MAX_ACTIVITY_DISPLAY_BYTES, "producer timestamp")
        }
        ToolFinished {
            activity_id,
            tool_call_id,
            summary,
            extra,
            ..
        } => {
            reject_raw_tool_payload(extra)?;
            validate_activity_identities(activity_id, tool_call_id)?;
            validate_summary(summary)
        }
    }
}

fn reject_raw_tool_payload(
    extra: &BTreeMap<String, serde_json::Value>,
) -> Result<(), SessionContractError> {
    if ["input", "arguments", "result", "raw_input", "raw_result"]
        .iter()
        .any(|field| extra.contains_key(*field))
    {
        Err(SessionContractError::Message {
            reason: "tool activity contains a reserved raw payload field",
        })
    } else {
        Ok(())
    }
}

fn validate_summary(summary: &ActivitySummary) -> Result<(), SessionContractError> {
    validate_bounded_text(
        &summary.text,
        summary.truncated,
        summary.original_bytes,
        summary.omitted_bytes,
        MAX_ACTIVITY_DISPLAY_BYTES,
        "tool summary",
    )
}

fn validate_activity_identities(
    activity_id: &str,
    tool_call_id: &str,
) -> Result<(), SessionContractError> {
    validate_identity("activity_id", activity_id)?;
    validate_identity("tool_call_id", tool_call_id)
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

fn validate_text(
    value: &str,
    max_bytes: usize,
    reason: &'static str,
) -> Result<(), SessionContractError> {
    if value.trim().is_empty() || value.len() > max_bytes || value.chars().any(char::is_control) {
        Err(SessionContractError::Message { reason })
    } else {
        Ok(())
    }
}

fn validate_bounded_text(
    text: &str,
    truncated: bool,
    original_bytes: u64,
    omitted_bytes: u64,
    max_bytes: usize,
    reason: &'static str,
) -> Result<(), SessionContractError> {
    let retained_bytes = u64::try_from(text.len()).unwrap_or(u64::MAX);
    let counts_match = original_bytes.checked_sub(omitted_bytes) == Some(retained_bytes);
    let truncation_matches = if truncated {
        omitted_bytes > 0
    } else {
        omitted_bytes == 0
    };
    if text.len() > max_bytes || !counts_match || !truncation_matches {
        Err(SessionContractError::Message { reason })
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{
        MAX_ACTIVITY_DISPLAY_BYTES, SessionActivityEventPayload, parse_session_activity_event,
    };
    use crate::session::SessionContractError;

    fn command_started() -> serde_json::Value {
        json!({
            "kind": "nopal.session.event/v3",
            "event_id": "event-started",
            "plot_id": "plot-01",
            "session_id": "session-01",
            "stream_id": "stream-01",
            "sequence": 2,
            "previous_cursor": "cursor-01",
            "cursor": "cursor-02",
            "command_id": "command-01",
            "event": {
                "type": "command_started",
                "activity_id": "activity-01",
                "tool_call_id": "tool-call-01",
                "command": "cargo test",
                "started_at": "2026-07-13T11:00:00Z"
            }
        })
    }

    #[test]
    fn rejects_missing_or_unsafe_activity_identities() {
        let mut missing = command_started();
        missing["event"]
            .as_object_mut()
            .expect("event object")
            .remove("activity_id");
        assert!(matches!(
            parse_session_activity_event(&missing.to_string()),
            Err(SessionContractError::Json(_))
        ));

        for field in ["activity_id", "tool_call_id"] {
            let mut unsafe_identity = command_started();
            unsafe_identity["event"][field] = json!("bad\nidentity");
            assert_eq!(
                parse_session_activity_event(&unsafe_identity.to_string()),
                Err(SessionContractError::Identity { field })
            );
        }
    }

    #[test]
    fn rejects_over_bound_command_displays_and_inconsistent_output_counts() {
        let mut oversized = command_started();
        oversized["event"]["command"] = json!("x".repeat(MAX_ACTIVITY_DISPLAY_BYTES + 1));
        assert!(matches!(
            parse_session_activity_event(&oversized.to_string()),
            Err(SessionContractError::Message { .. })
        ));

        let mut inconsistent = command_started();
        inconsistent["event"] = json!({
            "type": "command_finished",
            "activity_id": "activity-01",
            "tool_call_id": "tool-call-01",
            "duration_ms": 12,
            "exit": {"type": "code", "code": 0},
            "outcome": "succeeded",
            "output": {
                "channel": "stdout",
                "text": "ok",
                "truncated": false,
                "original_bytes": 10,
                "omitted_bytes": 0
            }
        });
        assert!(matches!(
            parse_session_activity_event(&inconsistent.to_string()),
            Err(SessionContractError::Message { .. })
        ));
    }

    #[test]
    fn preserves_additive_fields_in_v3_activity_payloads() {
        let mut value = command_started();
        value["event"]["future_payload_fact"] = json!({"version": 4});
        value["future_envelope_fact"] = json!(true);
        let event = parse_session_activity_event(&value.to_string()).expect("valid activity");
        assert_eq!(event.extra["future_envelope_fact"], true);
        let SessionActivityEventPayload::CommandStarted { extra, .. } = event.event else {
            panic!("expected command start");
        };
        assert_eq!(extra["future_payload_fact"]["version"], 4);
    }

    #[test]
    fn rejects_reserved_raw_fields_on_tool_activity() {
        for field in ["input", "arguments", "result", "raw_input", "raw_result"] {
            let mut value = command_started();
            value["event"] = json!({
                "type": "tool_started",
                "activity_id": "activity-tool-01",
                "tool_call_id": "tool-call-01",
                "tool_name": "unknown-tool",
                "summary": {
                    "text": "Details unavailable",
                    "details_unavailable": true,
                    "truncated": false,
                    "original_bytes": 19,
                    "omitted_bytes": 0
                },
                "started_at": "2026-07-13T11:00:00Z"
            });
            value["event"][field] = json!({"secret": "must-not-persist"});
            assert!(matches!(
                parse_session_activity_event(&value.to_string()),
                Err(SessionContractError::Message { .. })
            ));
        }
    }

    #[test]
    fn v3_legacy_messages_preserve_v2_multiline_and_frame_bound_semantics() {
        let text = format!("first line\n{}\nlast line", "x".repeat(9000));
        let value = json!({
            "kind": "nopal.session.event/v3",
            "event_id": "event-assistant",
            "plot_id": "plot-01",
            "session_id": "session-01",
            "stream_id": "stream-01",
            "sequence": 1,
            "previous_cursor": null,
            "cursor": "cursor-01",
            "command_id": "command-01",
            "event": {
                "type": "assistant_message",
                "text": text
            }
        });
        let event = parse_session_activity_event(&value.to_string()).expect("v2-compatible text");
        let SessionActivityEventPayload::AssistantMessage { text: parsed, .. } = event.event else {
            panic!("expected assistant message");
        };
        assert_eq!(parsed, text);
        assert!(parsed.len() > MAX_ACTIVITY_DISPLAY_BYTES);
        assert!(parsed.contains('\n'));
    }
}
