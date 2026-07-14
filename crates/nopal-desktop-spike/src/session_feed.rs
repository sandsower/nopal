use std::collections::VecDeque;

use std::collections::BTreeMap;

use nopal_feed_client::session::{
    DEFAULT_REPLAY_PAGE_LIMIT, SESSION_COMMAND_KIND, SESSION_MODEL_REQUEST_KIND,
    SESSION_SUBSCRIBE_KIND, SessionCommand, SessionCommandPayload, SessionFeedErrorCode,
    SessionModelError, SessionModelReference, SessionModelRequest, SessionModelRequestPayload,
    SessionModelState, SessionReplayComplete, SessionServerFrame, SessionSubscribe,
    SessionV3ServerFrame, SessionV4ServerFrame,
};

use crate::activity::VerifiedSessionEvent;

const V2_ENDPOINT_KIND: &str = "nopal.session/v2";
const V3_ENDPOINT_KIND: &str = "nopal.session/v3";
const V4_ENDPOINT_KIND: &str = "nopal.session/v4";
const MAX_FRAMES_PER_POLL: usize = 256;
const BACKOFF_MS: [u64; 6] = [100, 250, 500, 1_000, 2_000, 5_000];

fn feed_error_code(code: SessionFeedErrorCode) -> &'static str {
    match code {
        SessionFeedErrorCode::HistoryGap => "history_gap",
        SessionFeedErrorCode::HistoryCorrupt => "history_corrupt",
        SessionFeedErrorCode::ForeignSession => "foreign_session",
        SessionFeedErrorCode::BranchDiverged => "branch_diverged",
        SessionFeedErrorCode::HistoryTooLarge => "history_too_large",
        SessionFeedErrorCode::CursorConflict => "cursor_conflict",
        SessionFeedErrorCode::CommandConflict => "command_conflict",
        SessionFeedErrorCode::ReplayBufferOverflow => "replay_buffer_overflow",
        SessionFeedErrorCode::ProtocolViolation => "protocol_violation",
        SessionFeedErrorCode::Unavailable => "unavailable",
        SessionFeedErrorCode::Internal => "internal",
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionFeedContext {
    pub plot_id: String,
    pub session_id: String,
    pub endpoint_kind: String,
    pub endpoint_address: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionEndpointVersion {
    V2,
    V3,
    V4,
}

impl TryFrom<&str> for SessionEndpointVersion {
    type Error = FeedError;

    fn try_from(kind: &str) -> Result<Self, Self::Error> {
        match kind {
            V2_ENDPOINT_KIND => Ok(Self::V2),
            V3_ENDPOINT_KIND => Ok(Self::V3),
            V4_ENDPOINT_KIND => Ok(Self::V4),
            _ => Err(FeedError::protocol(format!(
                "unsupported Session endpoint kind {kind:?}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedErrorKind {
    EndpointAbsent,
    Io,
    Eof,
    Protocol,
    History,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeedError {
    pub kind: FeedErrorKind,
    pub message: String,
}

impl FeedError {
    pub fn endpoint_absent(message: impl Into<String>) -> Self {
        Self {
            kind: FeedErrorKind::EndpointAbsent,
            message: message.into(),
        }
    }

    pub fn io(message: impl Into<String>) -> Self {
        Self {
            kind: FeedErrorKind::Io,
            message: message.into(),
        }
    }

    pub fn eof(message: impl Into<String>) -> Self {
        Self {
            kind: FeedErrorKind::Eof,
            message: message.into(),
        }
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self {
            kind: FeedErrorKind::Protocol,
            message: message.into(),
        }
    }

    pub fn history(message: impl Into<String>) -> Self {
        Self {
            kind: FeedErrorKind::History,
            message: message.into(),
        }
    }

    pub fn retryable(&self) -> bool {
        matches!(
            self.kind,
            FeedErrorKind::EndpointAbsent | FeedErrorKind::Io | FeedErrorKind::Eof
        )
    }

    fn code(&self) -> &'static str {
        match self.kind {
            FeedErrorKind::EndpointAbsent => "endpoint_absent",
            FeedErrorKind::Io => "io",
            FeedErrorKind::Eof => "eof",
            FeedErrorKind::Protocol => "protocol",
            FeedErrorKind::History => "history",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientFeedFrame {
    Subscribe(SessionSubscribe),
    Prompt(SessionCommand),
    Model(SessionModelRequest),
}

pub trait FeedConnection {
    fn send(&mut self, frame: ClientFeedFrame) -> Result<(), FeedError>;
    fn try_receive(&mut self) -> Result<Option<SessionFeedServerFrame>, FeedError>;
    fn close(&mut self);
}

pub trait FeedTransport {
    type Connection: FeedConnection;

    fn connect(&mut self, context: &SessionFeedContext) -> Result<Self::Connection, FeedError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionFeedServerFrame {
    Event(Box<VerifiedSessionEvent>),
    ReplayComplete(SessionReplayComplete),
    FeedError(nopal_feed_client::session::SessionFeedError),
    ModelState(SessionModelState),
    ModelError(SessionModelError),
}

impl From<SessionServerFrame> for SessionFeedServerFrame {
    fn from(frame: SessionServerFrame) -> Self {
        match frame {
            SessionServerFrame::Event(event) => {
                Self::Event(Box::new(VerifiedSessionEvent::V2(event)))
            }
            SessionServerFrame::ReplayComplete(complete) => Self::ReplayComplete(complete),
            SessionServerFrame::FeedError(error) => Self::FeedError(error),
        }
    }
}

impl From<SessionV3ServerFrame> for SessionFeedServerFrame {
    fn from(frame: SessionV3ServerFrame) -> Self {
        match frame {
            SessionV3ServerFrame::Event(event) => {
                Self::Event(Box::new(VerifiedSessionEvent::V2(event)))
            }
            SessionV3ServerFrame::ActivityEvent(event) => {
                Self::Event(Box::new(VerifiedSessionEvent::V3(event)))
            }
            SessionV3ServerFrame::ReplayComplete(complete) => Self::ReplayComplete(complete),
            SessionV3ServerFrame::FeedError(error) => Self::FeedError(error),
        }
    }
}

impl From<SessionV4ServerFrame> for SessionFeedServerFrame {
    fn from(frame: SessionV4ServerFrame) -> Self {
        match frame {
            SessionV4ServerFrame::Event(event) => {
                Self::Event(Box::new(VerifiedSessionEvent::V2(event)))
            }
            SessionV4ServerFrame::ActivityEvent(event) => {
                Self::Event(Box::new(VerifiedSessionEvent::V3(event)))
            }
            SessionV4ServerFrame::ReplayComplete(complete) => Self::ReplayComplete(complete),
            SessionV4ServerFrame::FeedError(error) => Self::FeedError(error),
            SessionV4ServerFrame::ModelState(state) => Self::ModelState(state),
            SessionV4ServerFrame::ModelError(error) => Self::ModelError(error),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedState {
    Idle,
    Connecting {
        attempt: u32,
    },
    Restoring {
        attempt: u32,
        request_id: String,
        after_cursor: Option<String>,
        received: u64,
    },
    Live,
    Backoff {
        attempt: u32,
        retry_at_ms: u64,
        reason: String,
    },
    Fatal {
        code: String,
        message: String,
    },
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedUpdate {
    State {
        generation: u64,
        state: FeedState,
    },
    Event {
        generation: u64,
        event: Box<VerifiedSessionEvent>,
    },
    ReplayComplete {
        generation: u64,
        complete: SessionReplayComplete,
    },
    Error {
        generation: u64,
        code: String,
        message: String,
        retryable: bool,
    },
    ModelState {
        generation: u64,
        state: SessionModelState,
    },
    ModelError {
        generation: u64,
        error: SessionModelError,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeedResumePoint {
    pub stream_id: Option<String>,
    pub sequence: u64,
    pub cursor: Option<String>,
}

pub struct SessionFeed<T>
where
    T: FeedTransport,
{
    generation: u64,
    context: SessionFeedContext,
    endpoint_version: Result<SessionEndpointVersion, FeedError>,
    transport: T,
    connection: Option<T::Connection>,
    resume: FeedResumePoint,
    replay_candidate: Option<FeedResumePoint>,
    state: FeedState,
    updates: VecDeque<FeedUpdate>,
    consecutive_failures: u32,
    connection_sequence: u64,
    closed: bool,
}

impl<T> SessionFeed<T>
where
    T: FeedTransport,
{
    pub fn new(
        generation: u64,
        context: SessionFeedContext,
        resume: FeedResumePoint,
        transport: T,
    ) -> Self {
        let endpoint_version = SessionEndpointVersion::try_from(context.endpoint_kind.as_str());
        Self {
            generation,
            context,
            endpoint_version,
            transport,
            connection: None,
            resume,
            replay_candidate: None,
            state: FeedState::Idle,
            updates: VecDeque::new(),
            consecutive_failures: 0,
            connection_sequence: 0,
            closed: false,
        }
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn state(&self) -> &FeedState {
        &self.state
    }

    pub fn cursor(&self) -> Option<&str> {
        self.resume.cursor.as_deref()
    }

    pub fn resume_point(&self) -> &FeedResumePoint {
        &self.resume
    }

    pub fn can_submit(&self) -> bool {
        !self.closed && matches!(self.state, FeedState::Live) && self.connection.is_some()
    }

    pub fn poll(&mut self, now_ms: u64) {
        if self.closed || matches!(self.state, FeedState::Fatal { .. }) {
            return;
        }
        if let FeedState::Backoff { retry_at_ms, .. } = self.state
            && now_ms < retry_at_ms
        {
            return;
        }
        if self.connection.is_none()
            && matches!(self.state, FeedState::Idle | FeedState::Backoff { .. })
            && !self.open_connection(now_ms)
        {
            return;
        }

        for _ in 0..MAX_FRAMES_PER_POLL {
            let Some(connection) = self.connection.as_mut() else {
                break;
            };
            let frame = match connection.try_receive() {
                Ok(Some(frame)) => frame,
                Ok(None) => break,
                Err(error) => {
                    self.handle_error(error, now_ms);
                    break;
                }
            };
            if !self.handle_frame(frame, now_ms) {
                break;
            }
        }
    }

    pub fn retry_now(&mut self) -> bool {
        if self.closed || !matches!(self.state, FeedState::Backoff { .. }) {
            return false;
        }
        self.set_state(FeedState::Idle);
        true
    }

    pub fn submit_prompt(
        &mut self,
        command_id: impl Into<String>,
        text: impl Into<String>,
        now_ms: u64,
    ) -> Result<(), FeedError> {
        if !self.can_submit() {
            return Err(FeedError::io("structured Session feed is not live"));
        }
        let frame = ClientFeedFrame::Prompt(SessionCommand {
            kind: SESSION_COMMAND_KIND.to_owned(),
            command_id: command_id.into(),
            plot_id: self.context.plot_id.clone(),
            session_id: self.context.session_id.clone(),
            command: SessionCommandPayload::Prompt {
                text: text.into(),
                extra: BTreeMap::new(),
            },
            extra: BTreeMap::new(),
        });
        let Some(connection) = self.connection.as_mut() else {
            return Err(FeedError::io("structured Session feed lost its connection"));
        };
        let result = connection.send(frame);
        if let Err(error) = &result {
            self.handle_error(error.clone(), now_ms);
        }
        result
    }

    pub fn request_models(
        &mut self,
        request_id: impl Into<String>,
        now_ms: u64,
    ) -> Result<(), FeedError> {
        self.send_model_request(
            request_id.into(),
            SessionModelRequestPayload::Refresh {
                extra: BTreeMap::new(),
            },
            now_ms,
        )
    }

    pub fn switch_model(
        &mut self,
        request_id: impl Into<String>,
        provider: impl Into<String>,
        model_id: impl Into<String>,
        now_ms: u64,
    ) -> Result<(), FeedError> {
        self.send_model_request(
            request_id.into(),
            SessionModelRequestPayload::Switch {
                model: SessionModelReference {
                    provider: provider.into(),
                    id: model_id.into(),
                    extra: BTreeMap::new(),
                },
                extra: BTreeMap::new(),
            },
            now_ms,
        )
    }

    fn send_model_request(
        &mut self,
        request_id: String,
        request: SessionModelRequestPayload,
        now_ms: u64,
    ) -> Result<(), FeedError> {
        if !self.can_submit() || self.endpoint_version != Ok(SessionEndpointVersion::V4) {
            return Err(FeedError::io("Session model control is not live"));
        }
        let frame = ClientFeedFrame::Model(SessionModelRequest {
            kind: SESSION_MODEL_REQUEST_KIND.to_owned(),
            request_id,
            plot_id: self.context.plot_id.clone(),
            session_id: self.context.session_id.clone(),
            request,
            extra: BTreeMap::new(),
        });
        let Some(connection) = self.connection.as_mut() else {
            return Err(FeedError::io("structured Session feed lost its connection"));
        };
        let result = connection.send(frame);
        if let Err(error) = &result {
            self.handle_error(error.clone(), now_ms);
        }
        result
    }

    pub fn take_updates(&mut self) -> Vec<FeedUpdate> {
        self.updates.drain(..).collect()
    }

    pub fn close(&mut self) {
        if self.closed {
            return;
        }
        self.closed = true;
        self.discard_replay_candidate();
        self.close_connection();
        self.updates.clear();
        self.state = FeedState::Closed;
    }

    fn open_connection(&mut self, now_ms: u64) -> bool {
        self.discard_replay_candidate();
        if let Err(error) = self.endpoint_version.clone() {
            self.handle_error(error, now_ms);
            return false;
        }
        self.connection_sequence = self.connection_sequence.saturating_add(1);
        let attempt = self.consecutive_failures.saturating_add(1);
        self.set_state(FeedState::Connecting { attempt });
        let mut connection = match self.transport.connect(&self.context) {
            Ok(connection) => connection,
            Err(error) => {
                self.handle_error(error, now_ms);
                return false;
            }
        };
        let request_id = format!("subscribe-{}-{}", self.generation, self.connection_sequence);
        let subscribe = ClientFeedFrame::Subscribe(SessionSubscribe {
            kind: SESSION_SUBSCRIBE_KIND.to_owned(),
            request_id: request_id.clone(),
            plot_id: self.context.plot_id.clone(),
            session_id: self.context.session_id.clone(),
            after_cursor: self.resume.cursor.clone(),
            page_limit: DEFAULT_REPLAY_PAGE_LIMIT,
            extra: BTreeMap::new(),
        });
        if let Err(error) = connection.send(subscribe) {
            connection.close();
            self.handle_error(error, now_ms);
            return false;
        }
        self.connection = Some(connection);
        self.replay_candidate = Some(self.resume.clone());
        self.set_state(FeedState::Restoring {
            attempt,
            request_id,
            after_cursor: self.resume.cursor.clone(),
            received: 0,
        });
        true
    }

    fn handle_frame(&mut self, frame: SessionFeedServerFrame, now_ms: u64) -> bool {
        match frame {
            SessionFeedServerFrame::Event(event) => self.handle_event(event, now_ms),
            SessionFeedServerFrame::ReplayComplete(complete) => {
                self.handle_replay_complete(complete, now_ms)
            }
            SessionFeedServerFrame::FeedError(error) => {
                if error
                    .plot_id
                    .as_deref()
                    .is_some_and(|id| id != self.context.plot_id)
                    || error
                        .session_id
                        .as_deref()
                        .is_some_and(|id| id != self.context.session_id)
                {
                    self.handle_error(FeedError::protocol("foreign Session feed error"), now_ms);
                } else {
                    self.transition_error(
                        feed_error_code(error.code).to_owned(),
                        error.message,
                        error.retryable,
                        now_ms,
                    );
                }
                false
            }
            SessionFeedServerFrame::ModelState(state) => {
                if !matches!(self.state, FeedState::Live) {
                    self.handle_error(
                        FeedError::protocol("model state arrived before replay completion"),
                        now_ms,
                    );
                    return false;
                }
                self.updates.push_back(FeedUpdate::ModelState {
                    generation: self.generation,
                    state,
                });
                true
            }
            SessionFeedServerFrame::ModelError(error) => {
                if !matches!(self.state, FeedState::Live) {
                    self.handle_error(
                        FeedError::protocol("model error arrived before replay completion"),
                        now_ms,
                    );
                    return false;
                }
                self.updates.push_back(FeedUpdate::ModelError {
                    generation: self.generation,
                    error,
                });
                true
            }
        }
    }

    fn handle_event(&mut self, event: Box<VerifiedSessionEvent>, now_ms: u64) -> bool {
        if event.plot_id() != self.context.plot_id || event.session_id() != self.context.session_id
        {
            self.handle_error(FeedError::protocol("foreign Session event"), now_ms);
            return false;
        }
        let restoring = match &mut self.state {
            FeedState::Restoring { received, .. } => {
                *received += 1;
                true
            }
            FeedState::Live => false,
            _ => {
                self.handle_error(
                    FeedError::protocol("Session event arrived outside replay or live state"),
                    now_ms,
                );
                return false;
            }
        };
        let observation = if restoring {
            self.replay_candidate
                .as_mut()
                .ok_or_else(|| FeedError::protocol("Session replay candidate is unavailable"))
                .and_then(|candidate| observe_event(candidate, &event))
        } else {
            observe_event(&mut self.resume, &event)
        };
        if let Err(error) = observation {
            self.handle_error(error, now_ms);
            return false;
        }
        self.updates.push_back(FeedUpdate::Event {
            generation: self.generation,
            event,
        });
        true
    }

    fn handle_replay_complete(&mut self, complete: SessionReplayComplete, now_ms: u64) -> bool {
        let FeedState::Restoring {
            request_id: expected_request,
            received,
            ..
        } = &self.state
        else {
            self.handle_error(
                FeedError::protocol("replay completion arrived outside restore state"),
                now_ms,
            );
            return false;
        };
        let expected_request = expected_request.clone();
        let received = *received;
        let Some(candidate) = self.replay_candidate.as_ref() else {
            self.handle_error(
                FeedError::protocol("Session replay candidate is unavailable"),
                now_ms,
            );
            return false;
        };
        if complete.request_id != expected_request
            || complete.plot_id != self.context.plot_id
            || complete.session_id != self.context.session_id
            || complete.event_count != received
            || complete.cursor != candidate.cursor
            || complete.sequence != candidate.sequence
            || candidate
                .stream_id
                .as_ref()
                .is_some_and(|stream_id| stream_id != &complete.stream_id)
        {
            self.handle_error(
                FeedError::protocol("replay completion does not match the active subscription"),
                now_ms,
            );
            return false;
        }
        let Some(mut candidate) = self.replay_candidate.take() else {
            self.handle_error(
                FeedError::protocol("Session replay candidate is unavailable"),
                now_ms,
            );
            return false;
        };
        candidate.stream_id = Some(complete.stream_id.clone());
        self.resume = candidate;
        self.updates.push_back(FeedUpdate::ReplayComplete {
            generation: self.generation,
            complete,
        });
        self.consecutive_failures = 0;
        self.set_state(FeedState::Live);
        true
    }

    fn handle_error(&mut self, error: FeedError, now_ms: u64) {
        let code = error.code().to_owned();
        let retryable = error.retryable();
        self.transition_error(code, error.message, retryable, now_ms);
    }

    fn transition_error(&mut self, code: String, message: String, retryable: bool, now_ms: u64) {
        self.discard_replay_candidate();
        self.close_connection();
        self.updates.push_back(FeedUpdate::Error {
            generation: self.generation,
            code: code.clone(),
            message: message.clone(),
            retryable,
        });
        if retryable {
            self.consecutive_failures = self.consecutive_failures.saturating_add(1);
            let delay_index = usize::try_from(self.consecutive_failures.saturating_sub(1))
                .unwrap_or(usize::MAX)
                .min(BACKOFF_MS.len() - 1);
            self.set_state(FeedState::Backoff {
                attempt: self.consecutive_failures,
                retry_at_ms: now_ms.saturating_add(BACKOFF_MS[delay_index]),
                reason: message,
            });
        } else {
            self.set_state(FeedState::Fatal { code, message });
        }
    }

    fn close_connection(&mut self) {
        if let Some(mut connection) = self.connection.take() {
            connection.close();
        }
    }

    fn discard_replay_candidate(&mut self) {
        self.replay_candidate = None;
    }

    fn set_state(&mut self, state: FeedState) {
        self.state = state.clone();
        self.updates.push_back(FeedUpdate::State {
            generation: self.generation,
            state,
        });
    }
}

fn observe_event(
    resume: &mut FeedResumePoint,
    event: &VerifiedSessionEvent,
) -> Result<(), FeedError> {
    if resume
        .stream_id
        .as_ref()
        .is_some_and(|stream_id| stream_id != event.stream_id())
    {
        return Err(FeedError::protocol(
            "Session event changes the durable stream identity",
        ));
    }
    let is_next = resume
        .sequence
        .checked_add(1)
        .is_some_and(|sequence| sequence == event.sequence())
        && event.previous_cursor() == resume.cursor.as_deref();
    let is_exact_head_overlap =
        event.sequence() == resume.sequence && Some(event.cursor()) == resume.cursor.as_deref();
    if !is_next && !is_exact_head_overlap {
        return Err(FeedError::protocol(
            "Session event does not continue the verified durable cursor chain",
        ));
    }
    if resume.stream_id.is_none() {
        resume.stream_id = Some(event.stream_id().to_owned());
    }
    if is_next {
        resume.sequence = event.sequence();
        resume.cursor = Some(event.cursor().to_owned());
    }
    Ok(())
}

impl<T> Drop for SessionFeed<T>
where
    T: FeedTransport,
{
    fn drop(&mut self) {
        self.close();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use nopal_feed_client::session::{
        DurableSessionEvent, SESSION_FEED_ERROR_KIND, SESSION_REPLAY_COMPLETE_KIND,
        SESSION_SUBSCRIBE_KIND, SessionFeedError, SessionFeedErrorCode, SessionReplayComplete,
        SessionServerFrame, SessionSubscribe, SessionV3ServerFrame, parse_durable_session_event,
    };
    use nopal_feed_client::session_activity::{
        DurableSessionActivityEvent, parse_session_activity_event,
    };

    use super::{
        ClientFeedFrame, FeedConnection, FeedError, FeedResumePoint, FeedState, FeedTransport,
        FeedUpdate, SessionEndpointVersion, SessionFeed, SessionFeedContext,
        SessionFeedServerFrame,
    };
    use crate::activity::VerifiedSessionEvent;

    #[derive(Default)]
    struct FakeState {
        connect_results: VecDeque<Result<(), FeedError>>,
        inbound: VecDeque<Result<Option<SessionServerFrame>, FeedError>>,
        inbound_v3: VecDeque<Result<Option<SessionV3ServerFrame>, FeedError>>,
        sent: Vec<ClientFeedFrame>,
        closes: usize,
        connects: usize,
        v2_receives: usize,
        v3_receives: usize,
    }

    #[derive(Clone, Default)]
    struct FakeTransport(Rc<RefCell<FakeState>>);

    struct FakeConnection(Rc<RefCell<FakeState>>, SessionEndpointVersion);

    impl FeedTransport for FakeTransport {
        type Connection = FakeConnection;

        fn connect(&mut self, context: &SessionFeedContext) -> Result<Self::Connection, FeedError> {
            let mut state = self.0.borrow_mut();
            state.connects += 1;
            state.connect_results.pop_front().unwrap_or(Ok(()))?;
            drop(state);
            Ok(FakeConnection(
                self.0.clone(),
                SessionEndpointVersion::try_from(context.endpoint_kind.as_str())?,
            ))
        }
    }

    impl FeedConnection for FakeConnection {
        fn send(&mut self, frame: ClientFeedFrame) -> Result<(), FeedError> {
            self.0.borrow_mut().sent.push(frame);
            Ok(())
        }

        fn try_receive(&mut self) -> Result<Option<SessionFeedServerFrame>, FeedError> {
            match self.1 {
                SessionEndpointVersion::V2 => {
                    self.0.borrow_mut().v2_receives += 1;
                    self.0
                        .borrow_mut()
                        .inbound
                        .pop_front()
                        .unwrap_or(Ok(None))
                        .map(|frame| frame.map(SessionFeedServerFrame::from))
                }
                SessionEndpointVersion::V3 => {
                    self.0.borrow_mut().v3_receives += 1;
                    self.0
                        .borrow_mut()
                        .inbound_v3
                        .pop_front()
                        .unwrap_or(Ok(None))
                        .map(|frame| frame.map(SessionFeedServerFrame::from))
                }
                SessionEndpointVersion::V4 => Ok(None),
            }
        }

        fn close(&mut self) {
            self.0.borrow_mut().closes += 1;
        }
    }

    fn context() -> SessionFeedContext {
        SessionFeedContext {
            plot_id: "plot-a".to_owned(),
            session_id: "session-a".to_owned(),
            endpoint_kind: "nopal.session/v2".to_owned(),
            endpoint_address: "/tmp/session-a.sock".to_owned(),
        }
    }

    fn v3_context() -> SessionFeedContext {
        SessionFeedContext {
            endpoint_kind: "nopal.session/v3".to_owned(),
            ..context()
        }
    }

    fn event(sequence: u64, previous: Option<&str>, cursor: &str) -> DurableSessionEvent {
        parse_durable_session_event(
            &serde_json::json!({
                "kind": "nopal.session.event/v2",
                "event_id": format!("event-{sequence}"),
                "plot_id": "plot-a",
                "session_id": "session-a",
                "stream_id": "stream-a",
                "sequence": sequence,
                "previous_cursor": previous,
                "cursor": cursor,
                "event": {"type": "assistant_message", "text": format!("answer {sequence}")}
            })
            .to_string(),
        )
        .unwrap()
    }

    fn activity_event(
        sequence: u64,
        previous: Option<&str>,
        cursor: &str,
    ) -> DurableSessionActivityEvent {
        parse_session_activity_event(
            &serde_json::json!({
                "kind": "nopal.session.event/v3",
                "event_id": format!("activity-event-{sequence}"),
                "plot_id": "plot-a",
                "session_id": "session-a",
                "stream_id": "stream-a",
                "sequence": sequence,
                "previous_cursor": previous,
                "cursor": cursor,
                "event": {
                    "type": "command_started",
                    "activity_id": format!("activity-{sequence}"),
                    "tool_call_id": format!("call-{sequence}"),
                    "command": "printf exact",
                    "started_at": "2026-07-13T17:00:00Z"
                },
                "future_envelope": {"preserved": true}
            })
            .to_string(),
        )
        .unwrap()
    }

    #[test]
    fn explicit_v3_negotiation_resumes_from_v2_and_routes_exact_mixed_frames() {
        let transport = FakeTransport::default();
        let v2 = event(2, Some("cursor-1"), "cursor-2");
        let v3 = activity_event(3, Some("cursor-2"), "cursor-3");
        transport.0.borrow_mut().inbound_v3.extend([
            Ok(Some(SessionV3ServerFrame::Event(v2.clone()))),
            Ok(Some(SessionV3ServerFrame::ActivityEvent(v3.clone()))),
            Ok(Some(SessionV3ServerFrame::ReplayComplete(
                SessionReplayComplete {
                    kind: SESSION_REPLAY_COMPLETE_KIND.to_owned(),
                    request_id: "subscribe-21-1".to_owned(),
                    plot_id: "plot-a".to_owned(),
                    session_id: "session-a".to_owned(),
                    stream_id: "stream-a".to_owned(),
                    cursor: Some("cursor-3".to_owned()),
                    sequence: 3,
                    event_count: 2,
                    extra: Default::default(),
                },
            ))),
        ]);
        let mut feed = SessionFeed::new(
            21,
            v3_context(),
            FeedResumePoint {
                stream_id: Some("stream-a".to_owned()),
                sequence: 1,
                cursor: Some("cursor-1".to_owned()),
            },
            transport.clone(),
        );

        feed.poll(0);

        assert_eq!(feed.state(), &FeedState::Live);
        assert_eq!(feed.cursor(), Some("cursor-3"));
        assert_eq!(transport.0.borrow().v2_receives, 0);
        assert!(transport.0.borrow().v3_receives > 0);
        assert!(matches!(
            feed.take_updates().as_slice(),
            [
                FeedUpdate::State { state: FeedState::Connecting { .. }, .. },
                FeedUpdate::State { state: FeedState::Restoring { after_cursor: Some(cursor), .. }, .. },
                FeedUpdate::Event { event, .. },
                FeedUpdate::Event { event: activity, .. },
                FeedUpdate::ReplayComplete { complete: SessionReplayComplete { event_count: 2, .. }, .. },
                FeedUpdate::State { state: FeedState::Live, .. },
            ] if cursor == "cursor-1"
                && matches!(event.as_ref(), VerifiedSessionEvent::V2(exact) if exact == &v2)
                && matches!(activity.as_ref(), VerifiedSessionEvent::V3(exact) if exact == &v3)
        ));
    }

    #[test]
    fn explicit_v2_negotiation_never_uses_the_v3_receive_path_and_mismatch_is_fatal() {
        let transport = FakeTransport::default();
        transport.0.borrow_mut().inbound.push_back(Ok(Some(complete(
            "subscribe-22-1",
            None,
            0,
            0,
        ))));
        let mut v2_feed =
            SessionFeed::new(22, context(), FeedResumePoint::default(), transport.clone());
        v2_feed.poll(0);
        assert_eq!(v2_feed.state(), &FeedState::Live);
        assert!(transport.0.borrow().v2_receives > 0);
        assert_eq!(transport.0.borrow().v3_receives, 0);

        let mismatch_transport = FakeTransport::default();
        let mut mismatch_context = context();
        mismatch_context.endpoint_kind = "nopal.session/v5".to_owned();
        let mut mismatch = SessionFeed::new(
            23,
            mismatch_context,
            FeedResumePoint::default(),
            mismatch_transport.clone(),
        );
        mismatch.poll(0);
        assert!(matches!(
            mismatch.state(),
            FeedState::Fatal { code, .. } if code == "protocol"
        ));
        assert_eq!(mismatch_transport.0.borrow().connects, 0);
    }

    #[test]
    fn v3_gap_is_fatal_without_advancing_the_last_verified_v2_resume_point() {
        let transport = FakeTransport::default();
        transport.0.borrow_mut().inbound_v3.push_back(Ok(Some(
            SessionV3ServerFrame::ActivityEvent(activity_event(3, Some("cursor-2"), "cursor-3")),
        )));
        let verified = FeedResumePoint {
            stream_id: Some("stream-a".to_owned()),
            sequence: 1,
            cursor: Some("cursor-1".to_owned()),
        };
        let mut feed = SessionFeed::new(24, v3_context(), verified.clone(), transport);

        feed.poll(0);

        assert!(matches!(
            feed.state(),
            FeedState::Fatal { code, message }
                if code == "protocol" && message.contains("cursor chain")
        ));
        assert_eq!(feed.resume_point(), &verified);
        assert!(
            !feed
                .take_updates()
                .iter()
                .any(|update| matches!(update, FeedUpdate::Event { .. }))
        );
    }

    fn complete(
        request_id: &str,
        cursor: Option<&str>,
        sequence: u64,
        count: u64,
    ) -> SessionServerFrame {
        SessionServerFrame::ReplayComplete(SessionReplayComplete {
            kind: SESSION_REPLAY_COMPLETE_KIND.to_owned(),
            request_id: request_id.to_owned(),
            plot_id: "plot-a".to_owned(),
            session_id: "session-a".to_owned(),
            stream_id: "stream-a".to_owned(),
            cursor: cursor.map(str::to_owned),
            sequence,
            event_count: count,
            extra: Default::default(),
        })
    }

    #[test]
    fn tracer_subscribes_from_cursor_stages_replay_then_enables_live_commands() {
        let transport = FakeTransport::default();
        transport.0.borrow_mut().inbound.extend([
            Ok(Some(SessionServerFrame::Event(event(
                2,
                Some("cursor-1"),
                "cursor-2",
            )))),
            Ok(Some(complete("subscribe-7-1", Some("cursor-2"), 2, 1))),
        ]);
        let mut feed = SessionFeed::new(
            7,
            context(),
            FeedResumePoint {
                stream_id: Some("stream-a".to_owned()),
                sequence: 1,
                cursor: Some("cursor-1".to_owned()),
            },
            transport.clone(),
        );

        feed.poll(0);

        assert_eq!(
            transport.0.borrow().sent[0],
            ClientFeedFrame::Subscribe(SessionSubscribe {
                kind: SESSION_SUBSCRIBE_KIND.to_owned(),
                request_id: "subscribe-7-1".to_owned(),
                plot_id: "plot-a".to_owned(),
                session_id: "session-a".to_owned(),
                after_cursor: Some("cursor-1".to_owned()),
                page_limit: 256,
                extra: Default::default(),
            })
        );
        assert_eq!(feed.state(), &FeedState::Live);
        assert!(feed.can_submit());
        assert!(matches!(
            feed.take_updates().as_slice(),
            [
                FeedUpdate::State {
                    state: FeedState::Connecting { .. },
                    ..
                },
                FeedUpdate::State {
                    state: FeedState::Restoring { .. },
                    ..
                },
                FeedUpdate::Event { generation: 7, .. },
                FeedUpdate::ReplayComplete {
                    generation: 7,
                    complete: SessionReplayComplete { event_count: 1, .. }
                },
                FeedUpdate::State {
                    state: FeedState::Live,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn exact_head_overlap_is_delivered_for_timeline_dedup_and_keeps_the_resume_head() {
        let transport = FakeTransport::default();
        transport.0.borrow_mut().inbound.extend([
            Ok(Some(SessionServerFrame::Event(event(1, None, "cursor-1")))),
            Ok(Some(complete("subscribe-4-1", Some("cursor-1"), 1, 1))),
        ]);
        let mut feed = SessionFeed::new(
            4,
            context(),
            FeedResumePoint {
                stream_id: Some("stream-a".to_owned()),
                sequence: 1,
                cursor: Some("cursor-1".to_owned()),
            },
            transport,
        );

        feed.poll(0);

        assert_eq!(feed.state(), &FeedState::Live);
        assert_eq!(feed.resume_point().sequence, 1);
        assert_eq!(feed.cursor(), Some("cursor-1"));
        assert_eq!(
            feed.take_updates()
                .iter()
                .filter(|update| matches!(update, FeedUpdate::Event { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn endpoint_absence_and_eof_reconnect_with_bounded_deterministic_backoff() {
        let transport = FakeTransport::default();
        transport.0.borrow_mut().connect_results.extend([
            Err(FeedError::endpoint_absent("missing")),
            Err(FeedError::io("refused")),
            Ok(()),
        ]);
        let mut feed =
            SessionFeed::new(3, context(), FeedResumePoint::default(), transport.clone());

        feed.poll(0);
        assert!(matches!(
            feed.state(),
            FeedState::Backoff {
                attempt: 1,
                retry_at_ms: 100,
                ..
            }
        ));
        feed.poll(99);
        assert_eq!(transport.0.borrow().connects, 1);
        feed.poll(100);
        assert!(matches!(
            feed.state(),
            FeedState::Backoff {
                attempt: 2,
                retry_at_ms: 350,
                ..
            }
        ));
        feed.poll(349);
        assert_eq!(transport.0.borrow().connects, 2);
        feed.poll(350);
        assert!(matches!(feed.state(), FeedState::Restoring { .. }));

        transport
            .0
            .borrow_mut()
            .inbound
            .push_back(Err(FeedError::eof("closed")));
        feed.poll(351);
        assert!(matches!(
            feed.state(),
            FeedState::Backoff {
                attempt: 3,
                retry_at_ms: 851,
                ..
            }
        ));
    }

    #[test]
    fn partial_replay_eof_reconnects_from_the_original_verified_cursor() {
        let transport = FakeTransport::default();
        transport.0.borrow_mut().inbound.extend([
            Ok(Some(SessionServerFrame::Event(event(
                2,
                Some("cursor-1"),
                "cursor-2",
            )))),
            Err(FeedError::eof("closed before replay completion")),
        ]);
        let mut feed = SessionFeed::new(
            11,
            context(),
            FeedResumePoint {
                stream_id: Some("stream-a".to_owned()),
                sequence: 1,
                cursor: Some("cursor-1".to_owned()),
            },
            transport.clone(),
        );

        feed.poll(0);

        assert!(matches!(feed.state(), FeedState::Backoff { .. }));
        assert_eq!(feed.cursor(), Some("cursor-1"));
        assert_eq!(feed.resume_point().sequence, 1);
        transport.0.borrow_mut().inbound.extend([
            Ok(Some(SessionServerFrame::Event(event(
                2,
                Some("cursor-1"),
                "cursor-2",
            )))),
            Ok(Some(complete("subscribe-11-2", Some("cursor-2"), 2, 1))),
        ]);

        feed.poll(100);

        assert_eq!(feed.state(), &FeedState::Live);
        assert_eq!(feed.cursor(), Some("cursor-2"));
        assert_eq!(feed.resume_point().sequence, 2);
        let subscriptions = transport
            .0
            .borrow()
            .sent
            .iter()
            .filter_map(|frame| match frame {
                ClientFeedFrame::Subscribe(subscribe) => Some(subscribe.after_cursor.clone()),
                ClientFeedFrame::Prompt(_) => None,
                ClientFeedFrame::Model(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            subscriptions,
            [Some("cursor-1".to_owned()), Some("cursor-1".to_owned())]
        );
    }

    #[test]
    fn retryable_replay_overflow_discards_partial_candidate_before_reconnect() {
        let transport = FakeTransport::default();
        transport.0.borrow_mut().inbound.extend([
            Ok(Some(SessionServerFrame::Event(event(
                2,
                Some("cursor-1"),
                "cursor-2",
            )))),
            Ok(Some(SessionServerFrame::FeedError(SessionFeedError {
                kind: SESSION_FEED_ERROR_KIND.to_owned(),
                request_id: Some("subscribe-12-1".to_owned()),
                plot_id: Some("plot-a".to_owned()),
                session_id: Some("session-a".to_owned()),
                code: SessionFeedErrorCode::ReplayBufferOverflow,
                retryable: true,
                message: "replay buffer overflowed".to_owned(),
                extra: Default::default(),
            }))),
        ]);
        let mut feed = SessionFeed::new(
            12,
            context(),
            FeedResumePoint {
                stream_id: Some("stream-a".to_owned()),
                sequence: 1,
                cursor: Some("cursor-1".to_owned()),
            },
            transport.clone(),
        );

        feed.poll(0);

        assert!(matches!(feed.state(), FeedState::Backoff { .. }));
        assert_eq!(feed.cursor(), Some("cursor-1"));
        assert_eq!(feed.resume_point().sequence, 1);
        transport.0.borrow_mut().inbound.extend([
            Ok(Some(SessionServerFrame::Event(event(
                2,
                Some("cursor-1"),
                "cursor-2",
            )))),
            Ok(Some(complete("subscribe-12-2", Some("cursor-2"), 2, 1))),
        ]);

        feed.poll(100);

        assert_eq!(feed.state(), &FeedState::Live);
        assert_eq!(feed.cursor(), Some("cursor-2"));
        assert_eq!(feed.resume_point().sequence, 2);
        let subscriptions = transport
            .0
            .borrow()
            .sent
            .iter()
            .filter_map(|frame| match frame {
                ClientFeedFrame::Subscribe(subscribe) => Some(subscribe.after_cursor.clone()),
                ClientFeedFrame::Prompt(_) => None,
                ClientFeedFrame::Model(_) => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            subscriptions,
            [Some("cursor-1".to_owned()), Some("cursor-1".to_owned())]
        );
    }

    #[test]
    fn manual_retry_bypasses_delay_but_preserves_failure_attempt_count() {
        let transport = FakeTransport::default();
        transport
            .0
            .borrow_mut()
            .connect_results
            .push_back(Err(FeedError::endpoint_absent("missing")));
        let mut feed =
            SessionFeed::new(1, context(), FeedResumePoint::default(), transport.clone());
        feed.poll(10);
        assert!(matches!(
            feed.state(),
            FeedState::Backoff { attempt: 1, .. }
        ));
        assert!(feed.retry_now());
        feed.poll(11);
        assert_eq!(transport.0.borrow().connects, 2);
        assert!(matches!(
            feed.state(),
            FeedState::Restoring { attempt: 2, .. }
        ));
    }

    #[test]
    fn replay_completion_resets_backoff_for_the_next_eof() {
        let transport = FakeTransport::default();
        transport
            .0
            .borrow_mut()
            .connect_results
            .push_back(Err(FeedError::io("first")));
        let mut feed =
            SessionFeed::new(5, context(), FeedResumePoint::default(), transport.clone());
        feed.poll(0);
        feed.poll(100);
        transport.0.borrow_mut().inbound.extend([
            Ok(Some(complete("subscribe-5-2", None, 0, 0))),
            Err(FeedError::eof("host restarted")),
        ]);
        feed.poll(101);
        assert!(matches!(
            feed.state(),
            FeedState::Backoff {
                attempt: 1,
                retry_at_ms: 201,
                ..
            }
        ));
    }

    #[test]
    fn fatal_protocol_error_never_retries() {
        let transport = FakeTransport::default();
        let mut feed =
            SessionFeed::new(8, context(), FeedResumePoint::default(), transport.clone());
        feed.poll(0);
        transport
            .0
            .borrow_mut()
            .inbound
            .push_back(Err(FeedError::protocol("foreign Session")));
        feed.poll(1);
        feed.poll(100_000);
        assert!(matches!(feed.state(), FeedState::Fatal { .. }));
        assert_eq!(transport.0.borrow().connects, 1);
    }

    #[test]
    fn remote_fatal_error_retains_its_stable_contract_code_without_duplication() {
        let transport = FakeTransport::default();
        let mut feed =
            SessionFeed::new(8, context(), FeedResumePoint::default(), transport.clone());
        feed.poll(0);
        feed.take_updates();
        transport.0.borrow_mut().inbound.extend([
            Ok(Some(SessionServerFrame::Event(event(1, None, "cursor-1")))),
            Ok(Some(SessionServerFrame::FeedError(SessionFeedError {
                kind: SESSION_FEED_ERROR_KIND.to_owned(),
                request_id: Some("subscribe-8-1".to_owned()),
                plot_id: Some("plot-a".to_owned()),
                session_id: Some("session-a".to_owned()),
                code: SessionFeedErrorCode::HistoryGap,
                retryable: false,
                message: "cursor is no longer available".to_owned(),
                extra: Default::default(),
            }))),
        ]);

        feed.poll(1);

        assert!(matches!(
            feed.state(),
            FeedState::Fatal { code, .. } if code == "history_gap"
        ));
        assert_eq!(feed.resume_point(), &FeedResumePoint::default());
        assert_eq!(
            feed.take_updates()
                .iter()
                .filter(|update| matches!(update, FeedUpdate::Error { .. }))
                .count(),
            1
        );
    }

    #[test]
    fn close_cancels_connection_clears_updates_and_rejects_late_frames() {
        let transport = FakeTransport::default();
        let mut feed =
            SessionFeed::new(9, context(), FeedResumePoint::default(), transport.clone());
        feed.poll(0);
        transport
            .0
            .borrow_mut()
            .inbound
            .push_back(Ok(Some(SessionServerFrame::Event(event(
                1, None, "cursor-1",
            )))));
        feed.poll(1);
        assert_eq!(feed.resume_point(), &FeedResumePoint::default());
        feed.take_updates();
        feed.close();
        assert_eq!(feed.resume_point(), &FeedResumePoint::default());
        transport
            .0
            .borrow_mut()
            .inbound
            .push_back(Ok(Some(SessionServerFrame::Event(event(
                1, None, "cursor-1",
            )))));
        feed.poll(500);
        assert_eq!(feed.state(), &FeedState::Closed);
        assert!(feed.take_updates().is_empty());
        assert_eq!(transport.0.borrow().closes, 1);
        assert!(!feed.retry_now());
    }
}
