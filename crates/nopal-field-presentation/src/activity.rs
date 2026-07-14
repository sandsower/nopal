use std::collections::HashMap;

use nopal_feed_client::session::{
    DurableSessionEvent, SESSION_EVENT_KIND, SessionEvent, SessionEventPayload,
};
use nopal_feed_client::session_activity::{
    ActivityOutput, ActivitySummary, CommandExit, CommandOutcome, DurableSessionActivityEvent,
    SessionActivityEventPayload, ToolFailureOutcome, ToolOutcome,
};

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VerifiedSessionEvent {
    V2(DurableSessionEvent),
    V3(DurableSessionActivityEvent),
}

impl VerifiedSessionEvent {
    pub fn event_id(&self) -> &str {
        match self {
            Self::V2(event) => &event.event_id,
            Self::V3(event) => &event.event_id,
        }
    }

    pub fn plot_id(&self) -> &str {
        match self {
            Self::V2(event) => &event.plot_id,
            Self::V3(event) => &event.plot_id,
        }
    }

    pub fn session_id(&self) -> &str {
        match self {
            Self::V2(event) => &event.session_id,
            Self::V3(event) => &event.session_id,
        }
    }

    pub fn stream_id(&self) -> &str {
        match self {
            Self::V2(event) => &event.stream_id,
            Self::V3(event) => &event.stream_id,
        }
    }

    pub fn sequence(&self) -> u64 {
        match self {
            Self::V2(event) => event.sequence,
            Self::V3(event) => event.sequence,
        }
    }

    pub fn previous_cursor(&self) -> Option<&str> {
        match self {
            Self::V2(event) => event.previous_cursor.as_deref(),
            Self::V3(event) => event.previous_cursor.as_deref(),
        }
    }

    pub fn cursor(&self) -> &str {
        match self {
            Self::V2(event) => &event.cursor,
            Self::V3(event) => &event.cursor,
        }
    }

    pub fn command_id(&self) -> Option<&str> {
        match self {
            Self::V2(event) => event.command_id.as_deref(),
            Self::V3(event) => event.command_id.as_deref(),
        }
    }

    pub fn user_message_command_id(&self) -> Option<&str> {
        match self {
            Self::V2(event) if matches!(event.event, SessionEventPayload::UserMessage { .. }) => {
                event.command_id.as_deref()
            }
            Self::V3(event)
                if matches!(event.event, SessionActivityEventPayload::UserMessage { .. }) =>
            {
                event.command_id.as_deref()
            }
            Self::V2(_) | Self::V3(_) => None,
        }
    }

    pub fn semantic_session_event(&self) -> Option<SessionEvent> {
        match self {
            Self::V2(event) => Some(event.semantic_event()),
            Self::V3(event) => {
                let payload = match &event.event {
                    SessionActivityEventPayload::SessionReady { extra } => {
                        SessionEventPayload::SessionReady {
                            extra: extra.clone(),
                        }
                    }
                    SessionActivityEventPayload::UserMessage { text, extra } => {
                        SessionEventPayload::UserMessage {
                            text: text.clone(),
                            extra: extra.clone(),
                        }
                    }
                    SessionActivityEventPayload::AssistantMessage { text, extra } => {
                        SessionEventPayload::AssistantMessage {
                            text: text.clone(),
                            extra: extra.clone(),
                        }
                    }
                    SessionActivityEventPayload::SessionError { message, extra } => {
                        SessionEventPayload::SessionError {
                            message: message.clone(),
                            extra: extra.clone(),
                        }
                    }
                    SessionActivityEventPayload::CommandStarted { .. }
                    | SessionActivityEventPayload::CommandFinished { .. }
                    | SessionActivityEventPayload::CommandFailed { .. }
                    | SessionActivityEventPayload::ToolStarted { .. }
                    | SessionActivityEventPayload::ToolFinished { .. }
                    | SessionActivityEventPayload::ToolFailed { .. } => return None,
                };
                Some(SessionEvent {
                    kind: SESSION_EVENT_KIND.to_owned(),
                    event_id: event.event_id.clone(),
                    plot_id: event.plot_id.clone(),
                    session_id: event.session_id.clone(),
                    command_id: event.command_id.clone(),
                    event: payload,
                    extra: event.extra.clone(),
                })
            }
        }
    }
}

#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ActivityKey {
    Event(String),
    Activity(String),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Direction {
    Previous,
    Next,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MessageRole {
    User,
    Assistant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MessageActivity {
    pub key: ActivityKey,
    pub event_id: String,
    pub command_id: Option<String>,
    pub role: MessageRole,
    pub text: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LifecycleActivity {
    pub key: ActivityKey,
    pub event_id: String,
    pub command_id: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandActivity {
    pub key: ActivityKey,
    pub activity_id: String,
    pub tool_call_id: String,
    pub command_id: Option<String>,
    pub command: String,
    pub started_at: String,
    pub working_directory: Option<String>,
    pub state: CommandActivityState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandActivityState {
    Incomplete,
    Finished {
        duration_ms: u64,
        exit: CommandExit,
        outcome: CommandOutcome,
        output: Option<ActivityOutput>,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ToolActivity {
    pub key: ActivityKey,
    pub activity_id: String,
    pub tool_call_id: String,
    pub command_id: Option<String>,
    pub tool_name: String,
    pub summary: ActivitySummary,
    pub started_at: String,
    pub state: ToolActivityState,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolActivityState {
    Incomplete,
    Finished {
        duration_ms: u64,
        outcome: ToolOutcome,
        summary: ActivitySummary,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FailureActivity {
    pub key: ActivityKey,
    pub command_id: Option<String>,
    pub message: String,
    pub source: FailureSource,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum FailureSource {
    Session {
        event_id: String,
    },
    Command {
        activity_id: String,
        tool_call_id: String,
        command: String,
        started_at: String,
        working_directory: Option<String>,
        duration_ms: Option<u64>,
    },
    Tool {
        activity_id: String,
        tool_call_id: String,
        tool_name: String,
        summary: ActivitySummary,
        started_at: String,
        duration_ms: Option<u64>,
        outcome: ToolFailureOutcome,
    },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivityItem {
    Message(MessageActivity),
    Command(CommandActivity),
    Tool(ToolActivity),
    Failure(FailureActivity),
    Lifecycle(LifecycleActivity),
}

impl ActivityItem {
    pub fn key(&self) -> &ActivityKey {
        match self {
            Self::Message(item) => &item.key,
            Self::Command(item) => &item.key,
            Self::Tool(item) => &item.key,
            Self::Failure(item) => &item.key,
            Self::Lifecycle(item) => &item.key,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityKind {
    Command,
    Tool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivityTerminalKind {
    CommandFinished,
    CommandFailed,
    ToolFinished,
    ToolFailed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ToolCallConflict {
    ActivityMismatch {
        activity_id: String,
        expected_tool_call_id: String,
        actual_tool_call_id: String,
    },
    Reused {
        tool_call_id: String,
        first_activity_id: String,
        conflicting_activity_id: String,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BoundedPayloadField {
    CommandOutput,
    ToolStartSummary,
    ToolFinishSummary,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ActivityProjectionError {
    OrphanTerminal {
        activity_id: String,
        terminal: ActivityTerminalKind,
    },
    ActivityKindReuse {
        activity_id: String,
        first: ActivityKind,
        conflicting: ActivityKind,
    },
    ToolCallConflict(ToolCallConflict),
    CommandIdentityConflict {
        activity_id: String,
        expected_command_id: Option<String>,
        actual_command_id: Option<String>,
    },
    ConflictingTerminalOutcome {
        activity_id: String,
        first: ActivityTerminalKind,
        conflicting: ActivityTerminalKind,
    },
    DuplicateStableKey {
        key: ActivityKey,
    },
    ImpossibleBoundedPayload {
        activity_id: String,
        field: BoundedPayloadField,
    },
}

#[derive(Debug, Eq, PartialEq)]
pub struct ActivityProjection {
    items: Vec<ActivityItem>,
    indexes: HashMap<ActivityKey, usize>,
}

impl ActivityProjection {
    pub fn items(&self) -> &[ActivityItem] {
        &self.items
    }

    pub fn item(&self, key: &ActivityKey) -> Option<&ActivityItem> {
        self.indexes
            .get(key)
            .and_then(|index| self.items.get(*index))
    }

    pub fn first_key(&self) -> Option<&ActivityKey> {
        self.items.first().map(ActivityItem::key)
    }

    pub fn adjacent_key(&self, key: &ActivityKey, direction: Direction) -> Option<&ActivityKey> {
        let index = *self.indexes.get(key)?;
        let adjacent = match direction {
            Direction::Previous => index.checked_sub(1)?,
            Direction::Next => index.checked_add(1)?,
        };
        self.items.get(adjacent).map(ActivityItem::key)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum TerminalFacts {
    CommandFinished {
        duration_ms: u64,
        exit: CommandExit,
        outcome: CommandOutcome,
        output: Option<ActivityOutput>,
    },
    CommandFailed {
        duration_ms: Option<u64>,
        message: String,
    },
    ToolFinished {
        duration_ms: u64,
        outcome: ToolOutcome,
        summary: ActivitySummary,
    },
    ToolFailed {
        duration_ms: Option<u64>,
        message: String,
        outcome: ToolFailureOutcome,
    },
}

impl TerminalFacts {
    fn kind(&self) -> ActivityTerminalKind {
        match self {
            Self::CommandFinished { .. } => ActivityTerminalKind::CommandFinished,
            Self::CommandFailed { .. } => ActivityTerminalKind::CommandFailed,
            Self::ToolFinished { .. } => ActivityTerminalKind::ToolFinished,
            Self::ToolFailed { .. } => ActivityTerminalKind::ToolFailed,
        }
    }
}

#[derive(Clone, Debug)]
struct ActivityRecord {
    kind: ActivityKind,
    tool_call_id: String,
    command_id: Option<String>,
    index: usize,
    terminal: Option<TerminalFacts>,
}

#[derive(Default)]
struct ActivityReducer {
    items: Vec<ActivityItem>,
    indexes: HashMap<ActivityKey, usize>,
    activities: HashMap<String, ActivityRecord>,
    tool_calls: HashMap<String, String>,
}

pub fn project_activity(
    events: &[VerifiedSessionEvent],
) -> Result<ActivityProjection, ActivityProjectionError> {
    let mut reducer = ActivityReducer::default();
    for event in events {
        reducer.push(event)?;
    }
    Ok(ActivityProjection {
        items: reducer.items,
        indexes: reducer.indexes,
    })
}

impl ActivityReducer {
    fn push(&mut self, event: &VerifiedSessionEvent) -> Result<(), ActivityProjectionError> {
        match event {
            VerifiedSessionEvent::V2(event) => self.push_v2(event),
            VerifiedSessionEvent::V3(event) => self.push_v3(event),
        }
    }

    fn push_v2(&mut self, event: &DurableSessionEvent) -> Result<(), ActivityProjectionError> {
        match &event.event {
            SessionEventPayload::SessionReady { .. } => {
                self.push_lifecycle(&event.event_id, event.command_id.clone())
            }
            SessionEventPayload::UserMessage { text, .. } => self.push_message(
                &event.event_id,
                event.command_id.clone(),
                MessageRole::User,
                text,
            ),
            SessionEventPayload::AssistantMessage { text, .. } => self.push_message(
                &event.event_id,
                event.command_id.clone(),
                MessageRole::Assistant,
                text,
            ),
            SessionEventPayload::SessionError { message, .. } => {
                self.push_session_failure(&event.event_id, event.command_id.clone(), message)
            }
        }
    }

    fn push_v3(
        &mut self,
        event: &DurableSessionActivityEvent,
    ) -> Result<(), ActivityProjectionError> {
        match &event.event {
            SessionActivityEventPayload::SessionReady { .. } => {
                self.push_lifecycle(&event.event_id, event.command_id.clone())
            }
            SessionActivityEventPayload::UserMessage { text, .. } => self.push_message(
                &event.event_id,
                event.command_id.clone(),
                MessageRole::User,
                text,
            ),
            SessionActivityEventPayload::AssistantMessage { text, .. } => self.push_message(
                &event.event_id,
                event.command_id.clone(),
                MessageRole::Assistant,
                text,
            ),
            SessionActivityEventPayload::SessionError { message, .. } => {
                self.push_session_failure(&event.event_id, event.command_id.clone(), message)
            }
            SessionActivityEventPayload::CommandStarted {
                activity_id,
                tool_call_id,
                command,
                started_at,
                working_directory,
                ..
            } => self.start_command(
                activity_id,
                tool_call_id,
                event.command_id.clone(),
                command,
                started_at,
                working_directory.clone(),
            ),
            SessionActivityEventPayload::CommandFinished {
                activity_id,
                tool_call_id,
                duration_ms,
                exit,
                outcome,
                output,
                ..
            } => self.finish_activity(
                activity_id,
                tool_call_id,
                event.command_id.clone(),
                ActivityKind::Command,
                TerminalFacts::CommandFinished {
                    duration_ms: *duration_ms,
                    exit: exit.clone(),
                    outcome: *outcome,
                    output: output.clone(),
                },
            ),
            SessionActivityEventPayload::CommandFailed {
                activity_id,
                tool_call_id,
                duration_ms,
                message,
                ..
            } => self.finish_activity(
                activity_id,
                tool_call_id,
                event.command_id.clone(),
                ActivityKind::Command,
                TerminalFacts::CommandFailed {
                    duration_ms: *duration_ms,
                    message: message.clone(),
                },
            ),
            SessionActivityEventPayload::ToolStarted {
                activity_id,
                tool_call_id,
                tool_name,
                summary,
                started_at,
                ..
            } => self.start_tool(
                activity_id,
                tool_call_id,
                event.command_id.clone(),
                tool_name,
                summary,
                started_at,
            ),
            SessionActivityEventPayload::ToolFinished {
                activity_id,
                tool_call_id,
                duration_ms,
                outcome,
                summary,
                ..
            } => self.finish_activity(
                activity_id,
                tool_call_id,
                event.command_id.clone(),
                ActivityKind::Tool,
                TerminalFacts::ToolFinished {
                    duration_ms: *duration_ms,
                    outcome: *outcome,
                    summary: summary.clone(),
                },
            ),
            SessionActivityEventPayload::ToolFailed {
                activity_id,
                tool_call_id,
                duration_ms,
                message,
                outcome,
                ..
            } => self.finish_activity(
                activity_id,
                tool_call_id,
                event.command_id.clone(),
                ActivityKind::Tool,
                TerminalFacts::ToolFailed {
                    duration_ms: *duration_ms,
                    message: message.clone(),
                    outcome: *outcome,
                },
            ),
        }
    }

    fn push_message(
        &mut self,
        event_id: &str,
        command_id: Option<String>,
        role: MessageRole,
        text: &str,
    ) -> Result<(), ActivityProjectionError> {
        let key = ActivityKey::Event(event_id.to_owned());
        self.push_item(ActivityItem::Message(MessageActivity {
            key,
            event_id: event_id.to_owned(),
            command_id,
            role,
            text: text.to_owned(),
        }))
        .map(|_| ())
    }

    fn push_lifecycle(
        &mut self,
        event_id: &str,
        command_id: Option<String>,
    ) -> Result<(), ActivityProjectionError> {
        let key = ActivityKey::Event(event_id.to_owned());
        self.push_item(ActivityItem::Lifecycle(LifecycleActivity {
            key,
            event_id: event_id.to_owned(),
            command_id,
        }))
        .map(|_| ())
    }

    fn push_session_failure(
        &mut self,
        event_id: &str,
        command_id: Option<String>,
        message: &str,
    ) -> Result<(), ActivityProjectionError> {
        let key = ActivityKey::Event(event_id.to_owned());
        self.push_item(ActivityItem::Failure(FailureActivity {
            key,
            command_id,
            message: message.to_owned(),
            source: FailureSource::Session {
                event_id: event_id.to_owned(),
            },
        }))
        .map(|_| ())
    }

    fn start_command(
        &mut self,
        activity_id: &str,
        tool_call_id: &str,
        command_id: Option<String>,
        command: &str,
        started_at: &str,
        working_directory: Option<String>,
    ) -> Result<(), ActivityProjectionError> {
        self.reserve_activity(
            activity_id,
            tool_call_id,
            command_id.clone(),
            ActivityKind::Command,
        )?;
        let key = ActivityKey::Activity(activity_id.to_owned());
        let index = self.push_item(ActivityItem::Command(CommandActivity {
            key,
            activity_id: activity_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
            command_id: command_id.clone(),
            command: command.to_owned(),
            started_at: started_at.to_owned(),
            working_directory,
            state: CommandActivityState::Incomplete,
        }))?;
        self.activities.insert(
            activity_id.to_owned(),
            ActivityRecord {
                kind: ActivityKind::Command,
                tool_call_id: tool_call_id.to_owned(),
                command_id,
                index,
                terminal: None,
            },
        );
        Ok(())
    }

    fn start_tool(
        &mut self,
        activity_id: &str,
        tool_call_id: &str,
        command_id: Option<String>,
        tool_name: &str,
        summary: &ActivitySummary,
        started_at: &str,
    ) -> Result<(), ActivityProjectionError> {
        validate_summary(activity_id, BoundedPayloadField::ToolStartSummary, summary)?;
        self.reserve_activity(
            activity_id,
            tool_call_id,
            command_id.clone(),
            ActivityKind::Tool,
        )?;
        let key = ActivityKey::Activity(activity_id.to_owned());
        let index = self.push_item(ActivityItem::Tool(ToolActivity {
            key,
            activity_id: activity_id.to_owned(),
            tool_call_id: tool_call_id.to_owned(),
            command_id: command_id.clone(),
            tool_name: tool_name.to_owned(),
            summary: summary.clone(),
            started_at: started_at.to_owned(),
            state: ToolActivityState::Incomplete,
        }))?;
        self.activities.insert(
            activity_id.to_owned(),
            ActivityRecord {
                kind: ActivityKind::Tool,
                tool_call_id: tool_call_id.to_owned(),
                command_id,
                index,
                terminal: None,
            },
        );
        Ok(())
    }

    fn reserve_activity(
        &mut self,
        activity_id: &str,
        tool_call_id: &str,
        command_id: Option<String>,
        kind: ActivityKind,
    ) -> Result<(), ActivityProjectionError> {
        if let Some(existing) = self.activities.get(activity_id) {
            if existing.kind != kind {
                return Err(ActivityProjectionError::ActivityKindReuse {
                    activity_id: activity_id.to_owned(),
                    first: existing.kind,
                    conflicting: kind,
                });
            }
            self.validate_activity_identity(activity_id, tool_call_id, command_id.as_ref())?;
            return Err(ActivityProjectionError::ActivityKindReuse {
                activity_id: activity_id.to_owned(),
                first: existing.kind,
                conflicting: kind,
            });
        }
        if let Some(first_activity_id) = self.tool_calls.get(tool_call_id)
            && first_activity_id != activity_id
        {
            return Err(ActivityProjectionError::ToolCallConflict(
                ToolCallConflict::Reused {
                    tool_call_id: tool_call_id.to_owned(),
                    first_activity_id: first_activity_id.clone(),
                    conflicting_activity_id: activity_id.to_owned(),
                },
            ));
        }
        self.tool_calls
            .insert(tool_call_id.to_owned(), activity_id.to_owned());
        Ok(())
    }

    fn validate_activity_identity(
        &self,
        activity_id: &str,
        tool_call_id: &str,
        command_id: Option<&String>,
    ) -> Result<ActivityRecord, ActivityProjectionError> {
        let record = self.activities.get(activity_id).cloned().ok_or_else(|| {
            ActivityProjectionError::OrphanTerminal {
                activity_id: activity_id.to_owned(),
                terminal: ActivityTerminalKind::CommandFinished,
            }
        })?;
        if record.tool_call_id != tool_call_id {
            return Err(ActivityProjectionError::ToolCallConflict(
                ToolCallConflict::ActivityMismatch {
                    activity_id: activity_id.to_owned(),
                    expected_tool_call_id: record.tool_call_id,
                    actual_tool_call_id: tool_call_id.to_owned(),
                },
            ));
        }
        if record.command_id.as_ref() != command_id {
            return Err(ActivityProjectionError::CommandIdentityConflict {
                activity_id: activity_id.to_owned(),
                expected_command_id: record.command_id,
                actual_command_id: command_id.cloned(),
            });
        }
        Ok(record)
    }

    fn finish_activity(
        &mut self,
        activity_id: &str,
        tool_call_id: &str,
        command_id: Option<String>,
        kind: ActivityKind,
        terminal: TerminalFacts,
    ) -> Result<(), ActivityProjectionError> {
        let terminal_kind = terminal.kind();
        let Some(existing) = self.activities.get(activity_id) else {
            return Err(ActivityProjectionError::OrphanTerminal {
                activity_id: activity_id.to_owned(),
                terminal: terminal_kind,
            });
        };
        if existing.kind != kind {
            return Err(ActivityProjectionError::ActivityKindReuse {
                activity_id: activity_id.to_owned(),
                first: existing.kind,
                conflicting: kind,
            });
        }
        let record =
            self.validate_activity_identity(activity_id, tool_call_id, command_id.as_ref())?;
        if let Some(first) = &record.terminal {
            if first == &terminal {
                return Ok(());
            }
            return Err(ActivityProjectionError::ConflictingTerminalOutcome {
                activity_id: activity_id.to_owned(),
                first: first.kind(),
                conflicting: terminal_kind,
            });
        }
        self.fold_terminal(activity_id, record.index, terminal.clone())?;
        if let Some(record) = self.activities.get_mut(activity_id) {
            record.terminal = Some(terminal);
        }
        Ok(())
    }

    fn fold_terminal(
        &mut self,
        activity_id: &str,
        index: usize,
        terminal: TerminalFacts,
    ) -> Result<(), ActivityProjectionError> {
        let current = self.items.get(index).cloned().ok_or_else(|| {
            ActivityProjectionError::DuplicateStableKey {
                key: ActivityKey::Activity(activity_id.to_owned()),
            }
        })?;
        let replacement = match (current, terminal) {
            (
                ActivityItem::Command(mut command),
                TerminalFacts::CommandFinished {
                    duration_ms,
                    exit,
                    outcome,
                    output,
                },
            ) => {
                if let Some(output) = &output {
                    validate_output(activity_id, output)?;
                }
                command.state = CommandActivityState::Finished {
                    duration_ms,
                    exit,
                    outcome,
                    output,
                };
                ActivityItem::Command(command)
            }
            (
                ActivityItem::Command(command),
                TerminalFacts::CommandFailed {
                    duration_ms,
                    message,
                },
            ) => ActivityItem::Failure(FailureActivity {
                key: command.key,
                command_id: command.command_id,
                message,
                source: FailureSource::Command {
                    activity_id: command.activity_id,
                    tool_call_id: command.tool_call_id,
                    command: command.command,
                    started_at: command.started_at,
                    working_directory: command.working_directory,
                    duration_ms,
                },
            }),
            (
                ActivityItem::Tool(mut tool),
                TerminalFacts::ToolFinished {
                    duration_ms,
                    outcome,
                    summary,
                },
            ) => {
                validate_summary(
                    activity_id,
                    BoundedPayloadField::ToolFinishSummary,
                    &summary,
                )?;
                tool.state = ToolActivityState::Finished {
                    duration_ms,
                    outcome,
                    summary,
                };
                ActivityItem::Tool(tool)
            }
            (
                ActivityItem::Tool(tool),
                TerminalFacts::ToolFailed {
                    duration_ms,
                    message,
                    outcome,
                },
            ) => ActivityItem::Failure(FailureActivity {
                key: tool.key,
                command_id: tool.command_id,
                message,
                source: FailureSource::Tool {
                    activity_id: tool.activity_id,
                    tool_call_id: tool.tool_call_id,
                    tool_name: tool.tool_name,
                    summary: tool.summary,
                    started_at: tool.started_at,
                    duration_ms,
                    outcome,
                },
            }),
            _ => {
                return Err(ActivityProjectionError::ActivityKindReuse {
                    activity_id: activity_id.to_owned(),
                    first: self
                        .activities
                        .get(activity_id)
                        .map_or(ActivityKind::Command, |record| record.kind),
                    conflicting: self
                        .activities
                        .get(activity_id)
                        .map_or(ActivityKind::Tool, |record| record.kind),
                });
            }
        };
        self.items[index] = replacement;
        Ok(())
    }

    fn push_item(&mut self, item: ActivityItem) -> Result<usize, ActivityProjectionError> {
        let key = item.key().clone();
        if self.indexes.contains_key(&key) {
            return Err(ActivityProjectionError::DuplicateStableKey { key });
        }
        let index = self.items.len();
        self.items.push(item);
        self.indexes.insert(key, index);
        Ok(index)
    }
}

fn validate_output(
    activity_id: &str,
    output: &ActivityOutput,
) -> Result<(), ActivityProjectionError> {
    let visible = u64::try_from(output.text.len()).unwrap_or(u64::MAX);
    let valid = if output.truncated {
        output.omitted_bytes > 0
            && visible.checked_add(output.omitted_bytes) == Some(output.original_bytes)
    } else {
        output.omitted_bytes == 0 && output.original_bytes == visible
    };
    if valid {
        Ok(())
    } else {
        Err(ActivityProjectionError::ImpossibleBoundedPayload {
            activity_id: activity_id.to_owned(),
            field: BoundedPayloadField::CommandOutput,
        })
    }
}

fn validate_summary(
    activity_id: &str,
    field: BoundedPayloadField,
    summary: &ActivitySummary,
) -> Result<(), ActivityProjectionError> {
    let visible = u64::try_from(summary.text.len()).unwrap_or(u64::MAX);
    let valid = if summary.truncated {
        summary.omitted_bytes > 0
            && visible.checked_add(summary.omitted_bytes) == Some(summary.original_bytes)
    } else {
        summary.omitted_bytes == 0 && summary.original_bytes == visible
    };
    if valid {
        Ok(())
    } else {
        Err(ActivityProjectionError::ImpossibleBoundedPayload {
            activity_id: activity_id.to_owned(),
            field,
        })
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::BTreeMap;

    use nopal_feed_client::session::{
        DURABLE_SESSION_EVENT_KIND, DurableSessionEvent, SessionEventPayload,
    };
    use nopal_feed_client::session_activity::{
        ActivityOutput, ActivityOutputChannel, ActivitySummary, CommandExit, CommandOutcome,
        DURABLE_SESSION_ACTIVITY_EVENT_KIND, DurableSessionActivityEvent,
        SessionActivityEventPayload, ToolFailureOutcome, ToolOutcome,
    };
    use serde_json::json;

    use super::{
        ActivityItem, ActivityKey, ActivityKind, ActivityProjectionError, ActivityTerminalKind,
        BoundedPayloadField, CommandActivityState, Direction, FailureSource, MessageRole,
        ToolActivityState, ToolCallConflict, VerifiedSessionEvent, project_activity,
    };

    fn v2(event_id: &str, sequence: u64, event: SessionEventPayload) -> VerifiedSessionEvent {
        VerifiedSessionEvent::V2(DurableSessionEvent {
            kind: DURABLE_SESSION_EVENT_KIND.to_owned(),
            event_id: event_id.to_owned(),
            plot_id: "plot-a".to_owned(),
            session_id: "session-a".to_owned(),
            stream_id: "stream-a".to_owned(),
            sequence,
            previous_cursor: (sequence > 1).then(|| format!("cursor-{}", sequence - 1)),
            cursor: format!("cursor-{sequence}"),
            command_id: Some("command-a".to_owned()),
            event,
            extra: BTreeMap::from([("future_envelope".to_owned(), json!({"v": 2}))]),
        })
    }

    fn v3(
        event_id: &str,
        sequence: u64,
        event: SessionActivityEventPayload,
    ) -> VerifiedSessionEvent {
        v3_with_command(event_id, sequence, Some("command-a"), event)
    }

    fn v3_with_command(
        event_id: &str,
        sequence: u64,
        command_id: Option<&str>,
        event: SessionActivityEventPayload,
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
            command_id: command_id.map(str::to_owned),
            event,
            extra: BTreeMap::from([("future_envelope".to_owned(), json!({"v": 3}))]),
        })
    }

    fn summary(text: &str) -> ActivitySummary {
        ActivitySummary {
            text: text.to_owned(),
            details_unavailable: false,
            truncated: false,
            original_bytes: text.len() as u64,
            omitted_bytes: 0,
            extra: BTreeMap::from([("future_summary".to_owned(), json!(true))]),
        }
    }

    fn output(text: &str) -> ActivityOutput {
        ActivityOutput {
            channel: ActivityOutputChannel::Combined,
            text: text.to_owned(),
            truncated: false,
            original_bytes: text.len() as u64,
            omitted_bytes: 0,
            extra: BTreeMap::from([("future_output".to_owned(), json!("kept"))]),
        }
    }

    #[test]
    fn exact_v2_v3_messages_preserve_first_event_order_and_navigate_stable_keys() {
        let events = vec![
            v2(
                "event-user",
                1,
                SessionEventPayload::UserMessage {
                    text: "  exact user\n".to_owned(),
                    extra: BTreeMap::from([("future_payload".to_owned(), json!(2))]),
                },
            ),
            v3(
                "event-assistant",
                2,
                SessionActivityEventPayload::AssistantMessage {
                    text: "exact assistant".to_owned(),
                    extra: BTreeMap::from([("future_payload".to_owned(), json!(3))]),
                },
            ),
            v3(
                "event-ready",
                3,
                SessionActivityEventPayload::SessionReady {
                    extra: BTreeMap::new(),
                },
            ),
        ];
        let exact_events = events.clone();

        assert_eq!(events[0].event_id(), "event-user");
        assert_eq!(events[0].plot_id(), "plot-a");
        assert_eq!(events[0].session_id(), "session-a");
        assert_eq!(events[0].stream_id(), "stream-a");
        assert_eq!(events[0].sequence(), 1);
        assert_eq!(events[0].previous_cursor(), None);
        assert_eq!(events[0].cursor(), "cursor-1");
        assert_eq!(events[0].command_id(), Some("command-a"));
        assert_eq!(events[0].user_message_command_id(), Some("command-a"));
        assert!(matches!(
            events[1].semantic_session_event(),
            Some(nopal_feed_client::session::SessionEvent {
                event: SessionEventPayload::AssistantMessage { text, .. },
                ..
            }) if text == "exact assistant"
        ));

        let projection = project_activity(&events).expect("verified projection");

        assert_eq!(events, exact_events, "the reducer is read-only");
        assert_eq!(projection.items().len(), 3);
        assert!(matches!(
            &projection.items()[0],
            ActivityItem::Message(message)
                if message.role == MessageRole::User && message.text == "  exact user\n"
        ));
        assert!(matches!(
            &projection.items()[1],
            ActivityItem::Message(message)
                if message.role == MessageRole::Assistant && message.text == "exact assistant"
        ));
        assert!(matches!(&projection.items()[2], ActivityItem::Lifecycle(_)));

        let user = ActivityKey::Event("event-user".to_owned());
        let assistant = ActivityKey::Event("event-assistant".to_owned());
        let ready = ActivityKey::Event("event-ready".to_owned());
        assert_eq!(projection.first_key(), Some(&user));
        assert_eq!(projection.item(&assistant), Some(&projection.items()[1]));
        assert_eq!(
            projection.adjacent_key(&user, Direction::Next),
            Some(&assistant)
        );
        assert_eq!(
            projection.adjacent_key(&ready, Direction::Previous),
            Some(&assistant)
        );
        assert_eq!(projection.adjacent_key(&ready, Direction::Next), None);
        assert_eq!(
            projection.adjacent_key(&ActivityKey::Event("unknown".to_owned()), Direction::Next),
            None
        );
    }

    #[test]
    fn command_terminal_folds_into_the_start_position_with_exact_typed_facts() {
        let events = vec![
            v3(
                "event-command-start",
                1,
                SessionActivityEventPayload::CommandStarted {
                    activity_id: "activity-command".to_owned(),
                    tool_call_id: "tool-call-command".to_owned(),
                    command: "printf 'exact'".to_owned(),
                    started_at: "2026-07-13T16:00:00Z".to_owned(),
                    working_directory: Some("repo".to_owned()),
                    extra: BTreeMap::from([("future_start".to_owned(), json!(1))]),
                },
            ),
            v2(
                "event-between",
                2,
                SessionEventPayload::AssistantMessage {
                    text: "between".to_owned(),
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "event-command-finish",
                3,
                SessionActivityEventPayload::CommandFinished {
                    activity_id: "activity-command".to_owned(),
                    tool_call_id: "tool-call-command".to_owned(),
                    duration_ms: 17,
                    exit: CommandExit::Code { code: 0 },
                    outcome: CommandOutcome::Succeeded,
                    output: Some(output("exact output")),
                    extra: BTreeMap::from([("future_finish".to_owned(), json!(2))]),
                },
            ),
        ];

        assert_eq!(events[0].event_id(), "event-command-start");
        assert_eq!(events[0].previous_cursor(), None);
        assert_eq!(events[0].command_id(), Some("command-a"));
        assert_eq!(events[0].user_message_command_id(), None);
        assert_eq!(events[0].semantic_session_event(), None);

        let projection = project_activity(&events).expect("folded command");

        assert_eq!(projection.items().len(), 2);
        let ActivityItem::Command(command) = &projection.items()[0] else {
            panic!("first start remains the command position");
        };
        assert_eq!(
            command.key,
            ActivityKey::Activity("activity-command".to_owned())
        );
        assert_eq!(command.command, "printf 'exact'");
        assert_eq!(command.working_directory.as_deref(), Some("repo"));
        let CommandActivityState::Finished {
            duration_ms,
            exit,
            outcome,
            output,
        } = &command.state
        else {
            panic!("command is complete");
        };
        assert_eq!(*duration_ms, 17);
        assert_eq!(exit, &CommandExit::Code { code: 0 });
        assert_eq!(*outcome, CommandOutcome::Succeeded);
        assert_eq!(
            output
                .as_ref()
                .and_then(|output| output.extra.get("future_output")),
            Some(&json!("kept"))
        );
        assert!(matches!(&projection.items()[1], ActivityItem::Message(_)));
    }

    #[test]
    fn tool_terminal_folds_while_unfinished_starts_remain_incomplete() {
        let events = vec![
            v3(
                "event-tool-start",
                1,
                SessionActivityEventPayload::ToolStarted {
                    activity_id: "activity-tool".to_owned(),
                    tool_call_id: "tool-call-tool".to_owned(),
                    tool_name: "read".to_owned(),
                    summary: summary("read Cargo.toml"),
                    started_at: "2026-07-13T16:00:00Z".to_owned(),
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "event-command-start",
                2,
                SessionActivityEventPayload::CommandStarted {
                    activity_id: "activity-incomplete".to_owned(),
                    tool_call_id: "tool-call-incomplete".to_owned(),
                    command: "long-running".to_owned(),
                    started_at: "2026-07-13T16:00:01Z".to_owned(),
                    working_directory: None,
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "event-tool-finish",
                3,
                SessionActivityEventPayload::ToolFinished {
                    activity_id: "activity-tool".to_owned(),
                    tool_call_id: "tool-call-tool".to_owned(),
                    duration_ms: 9,
                    outcome: ToolOutcome::Succeeded,
                    summary: summary("read complete"),
                    extra: BTreeMap::new(),
                },
            ),
        ];

        let projection = project_activity(&events).expect("tool fold");

        let ActivityItem::Tool(tool) = &projection.items()[0] else {
            panic!("tool row");
        };
        let ToolActivityState::Finished {
            duration_ms,
            outcome,
            summary,
        } = &tool.state
        else {
            panic!("tool is complete");
        };
        assert_eq!(*duration_ms, 9);
        assert_eq!(*outcome, ToolOutcome::Succeeded);
        assert_eq!(summary.extra["future_summary"], true);
        assert!(matches!(
            &projection.items()[1],
            ActivityItem::Command(command)
                if command.state == CommandActivityState::Incomplete
        ));
    }

    #[test]
    fn typed_session_command_and_tool_failures_keep_stable_keys_and_start_context() {
        let events = vec![
            v3(
                "command-start",
                1,
                SessionActivityEventPayload::CommandStarted {
                    activity_id: "command-failure".to_owned(),
                    tool_call_id: "command-call".to_owned(),
                    command: "false".to_owned(),
                    started_at: "t1".to_owned(),
                    working_directory: None,
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "command-failed",
                2,
                SessionActivityEventPayload::CommandFailed {
                    activity_id: "command-failure".to_owned(),
                    tool_call_id: "command-call".to_owned(),
                    duration_ms: Some(3),
                    message: "spawn failed".to_owned(),
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "tool-start",
                3,
                SessionActivityEventPayload::ToolStarted {
                    activity_id: "tool-failure".to_owned(),
                    tool_call_id: "tool-call".to_owned(),
                    tool_name: "write".to_owned(),
                    summary: summary("write file"),
                    started_at: "t2".to_owned(),
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "tool-failed",
                4,
                SessionActivityEventPayload::ToolFailed {
                    activity_id: "tool-failure".to_owned(),
                    tool_call_id: "tool-call".to_owned(),
                    duration_ms: None,
                    message: "denied".to_owned(),
                    outcome: ToolFailureOutcome::Failed,
                    extra: BTreeMap::new(),
                },
            ),
            v2(
                "session-failure",
                5,
                SessionEventPayload::SessionError {
                    message: "session unavailable".to_owned(),
                    extra: BTreeMap::new(),
                },
            ),
        ];

        let projection = project_activity(&events).expect("typed failures");

        assert!(matches!(
            &projection.items()[0],
            ActivityItem::Failure(failure)
                if failure.key == ActivityKey::Activity("command-failure".to_owned())
                    && failure.message == "spawn failed"
                    && matches!(failure.source, FailureSource::Command { duration_ms: Some(3), .. })
        ));
        assert!(matches!(
            &projection.items()[1],
            ActivityItem::Failure(failure)
                if failure.key == ActivityKey::Activity("tool-failure".to_owned())
                    && matches!(failure.source, FailureSource::Tool { outcome: ToolFailureOutcome::Failed, .. })
        ));
        assert!(matches!(
            &projection.items()[2],
            ActivityItem::Failure(failure)
                if failure.key == ActivityKey::Event("session-failure".to_owned())
                    && matches!(failure.source, FailureSource::Session { .. })
        ));
    }

    #[test]
    fn terminal_without_a_matching_start_is_an_orphan_error() {
        let error = project_activity(&[v3(
            "orphan",
            1,
            SessionActivityEventPayload::CommandFinished {
                activity_id: "missing".to_owned(),
                tool_call_id: "missing-call".to_owned(),
                duration_ms: 1,
                exit: CommandExit::Unavailable {
                    reason: "unknown".to_owned(),
                },
                outcome: CommandOutcome::Unknown,
                output: None,
                extra: BTreeMap::new(),
            },
        )])
        .expect_err("orphan terminal");

        assert_eq!(
            error,
            ActivityProjectionError::OrphanTerminal {
                activity_id: "missing".to_owned(),
                terminal: ActivityTerminalKind::CommandFinished,
            }
        );
    }

    #[test]
    fn activity_identity_cannot_be_reused_for_another_kind() {
        let error = project_activity(&[
            v3(
                "command",
                1,
                SessionActivityEventPayload::CommandStarted {
                    activity_id: "shared".to_owned(),
                    tool_call_id: "command-call".to_owned(),
                    command: "echo".to_owned(),
                    started_at: "t1".to_owned(),
                    working_directory: None,
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "tool",
                2,
                SessionActivityEventPayload::ToolStarted {
                    activity_id: "shared".to_owned(),
                    tool_call_id: "tool-call".to_owned(),
                    tool_name: "read".to_owned(),
                    summary: summary("read"),
                    started_at: "t2".to_owned(),
                    extra: BTreeMap::new(),
                },
            ),
        ])
        .expect_err("kind reuse");

        assert_eq!(
            error,
            ActivityProjectionError::ActivityKindReuse {
                activity_id: "shared".to_owned(),
                first: ActivityKind::Command,
                conflicting: ActivityKind::Tool,
            }
        );
    }

    #[test]
    fn tool_call_identity_must_match_the_start_and_cannot_cross_activities() {
        let mismatch = project_activity(&[
            v3(
                "start",
                1,
                SessionActivityEventPayload::CommandStarted {
                    activity_id: "activity-a".to_owned(),
                    tool_call_id: "call-a".to_owned(),
                    command: "echo".to_owned(),
                    started_at: "t1".to_owned(),
                    working_directory: None,
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "finish",
                2,
                SessionActivityEventPayload::CommandFinished {
                    activity_id: "activity-a".to_owned(),
                    tool_call_id: "call-b".to_owned(),
                    duration_ms: 1,
                    exit: CommandExit::Code { code: 0 },
                    outcome: CommandOutcome::Succeeded,
                    output: None,
                    extra: BTreeMap::new(),
                },
            ),
        ])
        .expect_err("tool-call mismatch");
        assert_eq!(
            mismatch,
            ActivityProjectionError::ToolCallConflict(ToolCallConflict::ActivityMismatch {
                activity_id: "activity-a".to_owned(),
                expected_tool_call_id: "call-a".to_owned(),
                actual_tool_call_id: "call-b".to_owned(),
            })
        );

        let reused = project_activity(&[
            v3(
                "first",
                1,
                SessionActivityEventPayload::CommandStarted {
                    activity_id: "activity-a".to_owned(),
                    tool_call_id: "shared-call".to_owned(),
                    command: "echo".to_owned(),
                    started_at: "t1".to_owned(),
                    working_directory: None,
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "second",
                2,
                SessionActivityEventPayload::ToolStarted {
                    activity_id: "activity-b".to_owned(),
                    tool_call_id: "shared-call".to_owned(),
                    tool_name: "read".to_owned(),
                    summary: summary("read"),
                    started_at: "t2".to_owned(),
                    extra: BTreeMap::new(),
                },
            ),
        ])
        .expect_err("tool-call reuse");
        assert_eq!(
            reused,
            ActivityProjectionError::ToolCallConflict(ToolCallConflict::Reused {
                tool_call_id: "shared-call".to_owned(),
                first_activity_id: "activity-a".to_owned(),
                conflicting_activity_id: "activity-b".to_owned(),
            })
        );
    }

    #[test]
    fn conflicting_terminal_outcomes_and_command_identity_are_rejected() {
        let start = v3(
            "start",
            1,
            SessionActivityEventPayload::CommandStarted {
                activity_id: "activity".to_owned(),
                tool_call_id: "call".to_owned(),
                command: "echo".to_owned(),
                started_at: "t1".to_owned(),
                working_directory: None,
                extra: BTreeMap::new(),
            },
        );
        let finished = v3(
            "finish",
            2,
            SessionActivityEventPayload::CommandFinished {
                activity_id: "activity".to_owned(),
                tool_call_id: "call".to_owned(),
                duration_ms: 1,
                exit: CommandExit::Code { code: 0 },
                outcome: CommandOutcome::Succeeded,
                output: None,
                extra: BTreeMap::new(),
            },
        );
        let failed = v3(
            "failed",
            3,
            SessionActivityEventPayload::CommandFailed {
                activity_id: "activity".to_owned(),
                tool_call_id: "call".to_owned(),
                duration_ms: Some(2),
                message: "late failure".to_owned(),
                extra: BTreeMap::new(),
            },
        );
        assert_eq!(
            project_activity(&[start.clone(), finished, failed]).expect_err("terminal conflict"),
            ActivityProjectionError::ConflictingTerminalOutcome {
                activity_id: "activity".to_owned(),
                first: ActivityTerminalKind::CommandFinished,
                conflicting: ActivityTerminalKind::CommandFailed,
            }
        );

        let different_command = v3_with_command(
            "finish-other-command",
            2,
            Some("command-b"),
            SessionActivityEventPayload::CommandFinished {
                activity_id: "activity".to_owned(),
                tool_call_id: "call".to_owned(),
                duration_ms: 1,
                exit: CommandExit::Code { code: 0 },
                outcome: CommandOutcome::Succeeded,
                output: None,
                extra: BTreeMap::new(),
            },
        );
        assert_eq!(
            project_activity(&[start, different_command]).expect_err("command identity conflict"),
            ActivityProjectionError::CommandIdentityConflict {
                activity_id: "activity".to_owned(),
                expected_command_id: Some("command-a".to_owned()),
                actual_command_id: Some("command-b".to_owned()),
            }
        );
    }

    #[test]
    fn impossible_bounded_terminal_payload_is_rejected_without_formatting_or_inference() {
        let invalid = ActivityOutput {
            channel: ActivityOutputChannel::Stdout,
            text: "kept".to_owned(),
            truncated: true,
            original_bytes: 4,
            omitted_bytes: 1,
            extra: BTreeMap::new(),
        };
        let error = project_activity(&[
            v3(
                "start",
                1,
                SessionActivityEventPayload::CommandStarted {
                    activity_id: "activity".to_owned(),
                    tool_call_id: "call".to_owned(),
                    command: "echo".to_owned(),
                    started_at: "t1".to_owned(),
                    working_directory: None,
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "finish",
                2,
                SessionActivityEventPayload::CommandFinished {
                    activity_id: "activity".to_owned(),
                    tool_call_id: "call".to_owned(),
                    duration_ms: 1,
                    exit: CommandExit::Code { code: 0 },
                    outcome: CommandOutcome::Succeeded,
                    output: Some(invalid),
                    extra: BTreeMap::new(),
                },
            ),
        ])
        .expect_err("impossible bounds");

        assert_eq!(
            error,
            ActivityProjectionError::ImpossibleBoundedPayload {
                activity_id: "activity".to_owned(),
                field: BoundedPayloadField::CommandOutput,
            }
        );

        let overflow = ActivityOutput {
            channel: ActivityOutputChannel::Stdout,
            text: "kept".to_owned(),
            truncated: true,
            original_bytes: 3,
            omitted_bytes: u64::MAX,
            extra: BTreeMap::new(),
        };
        let overflow_error = project_activity(&[
            v3(
                "overflow-start",
                1,
                SessionActivityEventPayload::CommandStarted {
                    activity_id: "overflow-activity".to_owned(),
                    tool_call_id: "overflow-call".to_owned(),
                    command: "echo".to_owned(),
                    started_at: "t1".to_owned(),
                    working_directory: None,
                    extra: BTreeMap::new(),
                },
            ),
            v3(
                "overflow-finish",
                2,
                SessionActivityEventPayload::CommandFinished {
                    activity_id: "overflow-activity".to_owned(),
                    tool_call_id: "overflow-call".to_owned(),
                    duration_ms: 1,
                    exit: CommandExit::Code { code: 0 },
                    outcome: CommandOutcome::Succeeded,
                    output: Some(overflow),
                    extra: BTreeMap::new(),
                },
            ),
        ])
        .expect_err("overflowing bounds");

        assert_eq!(
            overflow_error,
            ActivityProjectionError::ImpossibleBoundedPayload {
                activity_id: "overflow-activity".to_owned(),
                field: BoundedPayloadField::CommandOutput,
            }
        );
    }
}
