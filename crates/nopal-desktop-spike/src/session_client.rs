#[cfg(unix)]
use std::collections::VecDeque;
#[cfg(unix)]
use std::io::{self, Read, Write};

use nopal_feed_client::session::{
    ExpectedSessionContext, MAX_SESSION_LINE_BYTES, SessionServerFrame, SessionV3ServerFrame,
    SessionV4ServerFrame, parse_model_request, parse_session_command, parse_session_server_frame,
    parse_session_subscribe, parse_session_v3_server_frame, parse_session_v4_server_frame,
    validate_command_context, validate_durable_event_context, validate_model_error_context,
    validate_model_request_context, validate_model_state_context, validate_replay_complete_context,
    validate_session_activity_event_context,
};

use crate::session_feed::{
    ClientFeedFrame, FeedConnection, FeedError, FeedTransport, SessionEndpointVersion,
    SessionFeedContext, SessionFeedServerFrame,
};

#[derive(Debug, Clone, Copy, Default)]
pub struct ProductionFeedTransport;

#[cfg(unix)]
pub struct ProductionFeedConnection {
    stream: std::os::unix::net::UnixStream,
    connecting: bool,
    expected: ExpectedSessionContext,
    decoder: ProductionFrameDecoder,
    pending: VecDeque<Result<SessionFeedServerFrame, FeedError>>,
    outbound: Vec<u8>,
    outbound_offset: usize,
    closed: bool,
}

#[cfg(not(unix))]
pub struct ProductionFeedConnection;

#[cfg(unix)]
impl ProductionFeedConnection {
    #[cfg(test)]
    fn from_unix_stream(
        stream: std::os::unix::net::UnixStream,
        expected: ExpectedSessionContext,
    ) -> Result<Self, FeedError> {
        Self::from_unix_stream_state(stream, expected, false)
    }

    #[cfg(test)]
    fn from_unix_stream_for_version(
        stream: std::os::unix::net::UnixStream,
        expected: ExpectedSessionContext,
        endpoint_version: SessionEndpointVersion,
    ) -> Result<Self, FeedError> {
        Self::from_unix_stream_state_for_version(stream, expected, endpoint_version, false)
    }

    #[cfg(test)]
    fn from_unix_stream_state(
        stream: std::os::unix::net::UnixStream,
        expected: ExpectedSessionContext,
        connecting: bool,
    ) -> Result<Self, FeedError> {
        Self::from_unix_stream_state_for_version(
            stream,
            expected,
            SessionEndpointVersion::V2,
            connecting,
        )
    }

    fn from_unix_stream_state_for_version(
        stream: std::os::unix::net::UnixStream,
        expected: ExpectedSessionContext,
        endpoint_version: SessionEndpointVersion,
        connecting: bool,
    ) -> Result<Self, FeedError> {
        stream.set_nonblocking(true).map_err(|error| {
            FeedError::io(format!("cannot make Session feed nonblocking: {error}"))
        })?;
        let decoder = match endpoint_version {
            SessionEndpointVersion::V2 => {
                ProductionFrameDecoder::V2(SessionFrameDecoder::<SessionServerFrame>::new(
                    expected.clone(),
                ))
            }
            SessionEndpointVersion::V3 => {
                ProductionFrameDecoder::V3(SessionFrameDecoder::<SessionV3ServerFrame>::new(
                    expected.clone(),
                ))
            }
            SessionEndpointVersion::V4 => {
                ProductionFrameDecoder::V4(SessionFrameDecoder::<SessionV4ServerFrame>::new(
                    expected.clone(),
                ))
            }
        };
        Ok(Self {
            stream,
            connecting,
            decoder,
            expected,
            pending: VecDeque::new(),
            outbound: Vec::new(),
            outbound_offset: 0,
            closed: false,
        })
    }

    fn encode_frame(&self, frame: ClientFeedFrame) -> Result<Vec<u8>, FeedError> {
        let mut encoded = match frame {
            ClientFeedFrame::Subscribe(subscribe) => {
                let encoded = serde_json::to_string(&subscribe).map_err(|error| {
                    FeedError::protocol(format!("cannot encode Session subscription: {error}"))
                })?;
                let validated = parse_session_subscribe(&encoded)
                    .map_err(|error| FeedError::protocol(error.to_string()))?;
                validate_feed_context(&validated.plot_id, &validated.session_id, &self.expected)?;
                encoded
            }
            ClientFeedFrame::Prompt(command) => {
                let encoded = serde_json::to_string(&command).map_err(|error| {
                    FeedError::protocol(format!("cannot encode Session command: {error}"))
                })?;
                let validated = parse_session_command(&encoded)
                    .map_err(|error| FeedError::protocol(error.to_string()))?;
                validate_command_context(&validated, &self.expected)
                    .map_err(|error| FeedError::protocol(error.to_string()))?;
                encoded
            }
            ClientFeedFrame::Model(request) => {
                let encoded = serde_json::to_string(&request).map_err(|error| {
                    FeedError::protocol(format!("cannot encode Session model request: {error}"))
                })?;
                let validated = parse_model_request(&encoded)
                    .map_err(|error| FeedError::protocol(error.to_string()))?;
                validate_model_request_context(&validated, &self.expected)
                    .map_err(|error| FeedError::protocol(error.to_string()))?;
                encoded
            }
        };
        if encoded.len() > MAX_SESSION_LINE_BYTES {
            return Err(FeedError::protocol(format!(
                "Session client frame is {} bytes; limit is {MAX_SESSION_LINE_BYTES}",
                encoded.len()
            )));
        }
        encoded.push('\n');
        Ok(encoded.into_bytes())
    }

    fn queue_outbound(&mut self, bytes: &[u8]) {
        if self.outbound_offset > 0 {
            self.outbound.drain(..self.outbound_offset);
            self.outbound_offset = 0;
        }
        self.outbound.extend_from_slice(bytes);
    }

    fn flush_outbound(&mut self) -> Result<bool, FeedError> {
        if self.connecting {
            return Ok(false);
        }
        while self.outbound_offset < self.outbound.len() {
            match self.stream.write(&self.outbound[self.outbound_offset..]) {
                Ok(0) => {
                    return Err(FeedError::eof(
                        "Session feed closed while sending a client frame",
                    ));
                }
                Ok(count) => self.outbound_offset += count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(false),
                Err(error) => {
                    return Err(FeedError::io(format!("Session feed write failed: {error}")));
                }
            }
        }
        self.outbound.clear();
        self.outbound_offset = 0;
        Ok(true)
    }

    fn finish_connect(&mut self) -> Result<bool, FeedError> {
        if !self.connecting {
            return Ok(true);
        }
        if let Some(error) = self.stream.take_error().map_err(|error| {
            FeedError::io(format!("cannot inspect Session feed connection: {error}"))
        })? {
            self.closed = true;
            return Err(classify_connect_error("Session feed endpoint", error));
        }
        match self.stream.peer_addr() {
            Ok(_) => {
                self.connecting = false;
                Ok(true)
            }
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::NotConnected | io::ErrorKind::WouldBlock
                ) || connect_is_pending(&error) =>
            {
                Ok(false)
            }
            Err(error) => {
                self.closed = true;
                Err(classify_connect_error("Session feed endpoint", error))
            }
        }
    }

    fn receive_pending(&mut self) -> Result<Option<SessionFeedServerFrame>, FeedError> {
        match self.pending.pop_front() {
            Some(Ok(frame)) => Ok(Some(frame)),
            Some(Err(error)) => {
                self.closed = true;
                Err(error)
            }
            None => Ok(None),
        }
    }

    fn try_receive_frame(&mut self) -> Result<Option<SessionFeedServerFrame>, FeedError> {
        if self.closed {
            return Err(FeedError::eof("Session feed is closed"));
        }
        if !self.finish_connect()? {
            return Ok(None);
        }
        if !self.flush_outbound()? {
            return Ok(None);
        }
        if !self.pending.is_empty() {
            return self.receive_pending();
        }

        let mut buffer = [0_u8; 8192];
        loop {
            match self.stream.read(&mut buffer) {
                Ok(0) => {
                    self.closed = true;
                    return self
                        .decoder
                        .finish()
                        .map_or_else(Err, |()| Err(FeedError::eof("Session feed closed")));
                }
                Ok(count) => {
                    self.pending.extend(self.decoder.push(&buffer[..count]));
                    if !self.pending.is_empty() {
                        return self.receive_pending();
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => return Ok(None),
                Err(error) => {
                    self.closed = true;
                    return Err(FeedError::io(format!("Session feed read failed: {error}")));
                }
            }
        }
    }
}

#[cfg(unix)]
impl FeedConnection for ProductionFeedConnection {
    fn send(&mut self, frame: ClientFeedFrame) -> Result<(), FeedError> {
        if self.closed {
            return Err(FeedError::eof("Session feed is closed"));
        }
        let encoded = self.encode_frame(frame)?;
        self.queue_outbound(&encoded);
        if self.connecting {
            return Ok(());
        }
        self.flush_outbound().map(|_| ())
    }

    fn try_receive(&mut self) -> Result<Option<SessionFeedServerFrame>, FeedError> {
        self.try_receive_frame()
    }

    fn close(&mut self) {
        use std::net::Shutdown;

        if self.closed {
            return;
        }
        self.closed = true;
        self.pending.clear();
        self.outbound.clear();
        self.outbound_offset = 0;
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

#[cfg(not(unix))]
impl FeedConnection for ProductionFeedConnection {
    fn send(&mut self, _frame: ClientFeedFrame) -> Result<(), FeedError> {
        Err(FeedError::protocol(
            "Unix Session feed transport is unavailable on this platform",
        ))
    }

    fn try_receive(&mut self) -> Result<Option<SessionFeedServerFrame>, FeedError> {
        Err(FeedError::protocol(
            "Unix Session feed transport is unavailable on this platform",
        ))
    }

    fn close(&mut self) {}
}

impl FeedTransport for ProductionFeedTransport {
    type Connection = ProductionFeedConnection;

    fn connect(&mut self, context: &SessionFeedContext) -> Result<Self::Connection, FeedError> {
        let endpoint_version = SessionEndpointVersion::try_from(context.endpoint_kind.as_str())?;
        if context.endpoint_address.trim().is_empty()
            || context.endpoint_address.chars().any(char::is_control)
        {
            return Err(FeedError::protocol("Session endpoint address is invalid"));
        }
        let expected = ExpectedSessionContext::new(&context.plot_id, &context.session_id)
            .map_err(|error| FeedError::protocol(error.to_string()))?;
        connect_production_feed(&context.endpoint_address, expected, endpoint_version)
    }
}

#[cfg(unix)]
fn connect_production_feed(
    address: &str,
    expected: ExpectedSessionContext,
    endpoint_version: SessionEndpointVersion,
) -> Result<ProductionFeedConnection, FeedError> {
    use std::os::unix::net::UnixStream;

    use socket2::{Domain, SockAddr, Socket, Type};

    validate_unix_endpoint_path(address).map_err(|error| {
        if error.kind() == io::ErrorKind::NotFound {
            FeedError::endpoint_absent(format!(
                "Session feed endpoint {address:?} is absent: {error}"
            ))
        } else {
            FeedError::protocol(format!(
                "Session feed endpoint {address:?} is not trusted: {error}"
            ))
        }
    })?;
    let socket = Socket::new(Domain::UNIX, Type::STREAM, None)
        .map_err(|error| FeedError::io(format!("cannot create Session feed socket: {error}")))?;
    socket.set_nonblocking(true).map_err(|error| {
        FeedError::io(format!(
            "cannot make Session feed socket nonblocking: {error}"
        ))
    })?;
    let socket_address = SockAddr::unix(address).map_err(|error| {
        FeedError::protocol(format!(
            "Session feed endpoint {address:?} is invalid: {error}"
        ))
    })?;
    let connecting = match socket.connect(&socket_address) {
        Ok(()) => false,
        Err(error) if connect_is_pending(&error) => true,
        Err(error) => return Err(classify_connect_error(address, error)),
    };
    let descriptor: std::os::fd::OwnedFd = socket.into();
    let stream = UnixStream::from(descriptor);
    ProductionFeedConnection::from_unix_stream_state_for_version(
        stream,
        expected,
        endpoint_version,
        connecting,
    )
}

#[cfg(unix)]
fn classify_connect_error(address: &str, error: io::Error) -> FeedError {
    match error.kind() {
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused => FeedError::endpoint_absent(
            format!("Session feed endpoint {address:?} is unavailable: {error}"),
        ),
        _ => FeedError::io(format!(
            "cannot connect to Session feed endpoint {address:?}: {error}"
        )),
    }
}

#[cfg(unix)]
fn connect_is_pending(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EINPROGRESS) | Some(libc::EALREADY)
    )
}

#[cfg(not(unix))]
fn connect_production_feed(
    _address: &str,
    _expected: ExpectedSessionContext,
    _endpoint_version: SessionEndpointVersion,
) -> Result<ProductionFeedConnection, FeedError> {
    Err(FeedError::protocol(
        "Unix Session feed transport is unavailable on this platform",
    ))
}

trait StrictServerFrame: Sized {
    fn parse(line: &str) -> Result<Self, FeedError>;
    fn validate_context(&self, expected: &ExpectedSessionContext) -> Result<(), FeedError>;
}

impl StrictServerFrame for SessionServerFrame {
    fn parse(line: &str) -> Result<Self, FeedError> {
        parse_session_server_frame(line).map_err(|error| FeedError::protocol(error.to_string()))
    }

    fn validate_context(&self, expected: &ExpectedSessionContext) -> Result<(), FeedError> {
        validate_server_frame_context(self, expected)
    }
}

impl StrictServerFrame for SessionV3ServerFrame {
    fn parse(line: &str) -> Result<Self, FeedError> {
        parse_session_v3_server_frame(line).map_err(|error| FeedError::protocol(error.to_string()))
    }

    fn validate_context(&self, expected: &ExpectedSessionContext) -> Result<(), FeedError> {
        validate_v3_server_frame_context(self, expected)
    }
}

impl StrictServerFrame for SessionV4ServerFrame {
    fn parse(line: &str) -> Result<Self, FeedError> {
        parse_session_v4_server_frame(line).map_err(|error| FeedError::protocol(error.to_string()))
    }

    fn validate_context(&self, expected: &ExpectedSessionContext) -> Result<(), FeedError> {
        validate_v4_server_frame_context(self, expected)
    }
}

struct SessionFrameDecoder<F> {
    expected: ExpectedSessionContext,
    line: Vec<u8>,
    discarding_oversized_line: bool,
    frame: std::marker::PhantomData<F>,
}

#[cfg(test)]
type SessionServerFrameDecoder = SessionFrameDecoder<SessionServerFrame>;

impl<F> SessionFrameDecoder<F>
where
    F: StrictServerFrame,
{
    fn new(expected: ExpectedSessionContext) -> Self {
        Self {
            expected,
            line: Vec::new(),
            discarding_oversized_line: false,
            frame: std::marker::PhantomData,
        }
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<Result<F, FeedError>> {
        let mut frames = Vec::new();
        for byte in chunk {
            if self.discarding_oversized_line {
                if *byte == b'\n' {
                    self.discarding_oversized_line = false;
                }
                continue;
            }
            if *byte == b'\n' {
                frames.push(self.decode_line());
                self.line.clear();
                continue;
            }
            self.line.push(*byte);
            if self.line.len() > MAX_SESSION_LINE_BYTES {
                frames.push(Err(FeedError::protocol(format!(
                    "Session server frame is {} bytes; limit is {MAX_SESSION_LINE_BYTES}",
                    self.line.len()
                ))));
                self.line.clear();
                self.discarding_oversized_line = true;
            }
        }
        frames
    }

    fn finish(&mut self) -> Result<(), FeedError> {
        if self.line.is_empty() && !self.discarding_oversized_line {
            return Ok(());
        }
        self.line.clear();
        self.discarding_oversized_line = false;
        Err(FeedError::protocol(
            "Session feed ended before the required LF delimiter",
        ))
    }

    fn decode_line(&self) -> Result<F, FeedError> {
        if self.line.is_empty() {
            return Err(FeedError::protocol(
                "empty Session line is not valid NDJSON",
            ));
        }
        if self.line.last() == Some(&b'\r') {
            return Err(FeedError::protocol(
                "Session protocol requires LF framing; CRLF is not accepted",
            ));
        }
        let line = std::str::from_utf8(&self.line)
            .map_err(|error| FeedError::protocol(format!("invalid Session UTF-8: {error}")))?;
        let frame = F::parse(line)?;
        frame.validate_context(&self.expected)?;
        Ok(frame)
    }
}

enum ProductionFrameDecoder {
    V2(SessionFrameDecoder<SessionServerFrame>),
    V3(SessionFrameDecoder<SessionV3ServerFrame>),
    V4(SessionFrameDecoder<SessionV4ServerFrame>),
}

impl ProductionFrameDecoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<Result<SessionFeedServerFrame, FeedError>> {
        match self {
            Self::V2(decoder) => decoder
                .push(chunk)
                .into_iter()
                .map(|frame| frame.map(SessionFeedServerFrame::from))
                .collect(),
            Self::V3(decoder) => decoder
                .push(chunk)
                .into_iter()
                .map(|frame| frame.map(SessionFeedServerFrame::from))
                .collect(),
            Self::V4(decoder) => decoder
                .push(chunk)
                .into_iter()
                .map(|frame| frame.map(SessionFeedServerFrame::from))
                .collect(),
        }
    }

    fn finish(&mut self) -> Result<(), FeedError> {
        match self {
            Self::V2(decoder) => decoder.finish(),
            Self::V3(decoder) => decoder.finish(),
            Self::V4(decoder) => decoder.finish(),
        }
    }
}

fn validate_server_frame_context(
    frame: &SessionServerFrame,
    expected: &ExpectedSessionContext,
) -> Result<(), FeedError> {
    match frame {
        SessionServerFrame::Event(event) => validate_durable_event_context(event, expected)
            .map_err(|error| FeedError::protocol(error.to_string())),
        SessionServerFrame::ReplayComplete(complete) => {
            validate_replay_complete_context(complete, expected)
                .map_err(|error| FeedError::protocol(error.to_string()))
        }
        SessionServerFrame::FeedError(error) => match (&error.plot_id, &error.session_id) {
            (Some(plot_id), Some(session_id)) => {
                validate_feed_context(plot_id, session_id, expected)
            }
            (None, None) => Ok(()),
            _ => Err(FeedError::protocol(
                "Session feed error has partial Plot/Session context",
            )),
        },
    }
}

fn validate_v3_server_frame_context(
    frame: &SessionV3ServerFrame,
    expected: &ExpectedSessionContext,
) -> Result<(), FeedError> {
    match frame {
        SessionV3ServerFrame::Event(event) => validate_durable_event_context(event, expected)
            .map_err(|error| FeedError::protocol(error.to_string())),
        SessionV3ServerFrame::ActivityEvent(event) => {
            validate_session_activity_event_context(event, expected)
                .map_err(|error| FeedError::protocol(error.to_string()))
        }
        SessionV3ServerFrame::ReplayComplete(complete) => {
            validate_replay_complete_context(complete, expected)
                .map_err(|error| FeedError::protocol(error.to_string()))
        }
        SessionV3ServerFrame::FeedError(error) => match (&error.plot_id, &error.session_id) {
            (Some(plot_id), Some(session_id)) => {
                validate_feed_context(plot_id, session_id, expected)
            }
            (None, None) => Ok(()),
            _ => Err(FeedError::protocol(
                "Session feed error has partial Plot/Session context",
            )),
        },
    }
}

fn validate_v4_server_frame_context(
    frame: &SessionV4ServerFrame,
    expected: &ExpectedSessionContext,
) -> Result<(), FeedError> {
    match frame {
        SessionV4ServerFrame::Event(event) => validate_durable_event_context(event, expected)
            .map_err(|error| FeedError::protocol(error.to_string())),
        SessionV4ServerFrame::ActivityEvent(event) => {
            validate_session_activity_event_context(event, expected)
                .map_err(|error| FeedError::protocol(error.to_string()))
        }
        SessionV4ServerFrame::ReplayComplete(complete) => {
            validate_replay_complete_context(complete, expected)
                .map_err(|error| FeedError::protocol(error.to_string()))
        }
        SessionV4ServerFrame::FeedError(error) => match (&error.plot_id, &error.session_id) {
            (Some(plot_id), Some(session_id)) => {
                validate_feed_context(plot_id, session_id, expected)
            }
            (None, None) => Ok(()),
            _ => Err(FeedError::protocol(
                "Session feed error has partial Plot/Session context",
            )),
        },
        SessionV4ServerFrame::ModelState(state) => validate_model_state_context(state, expected)
            .map_err(|error| FeedError::protocol(error.to_string())),
        SessionV4ServerFrame::ModelError(error) => validate_model_error_context(error, expected)
            .map_err(|contract| FeedError::protocol(contract.to_string())),
    }
}

fn validate_feed_context(
    plot_id: &str,
    session_id: &str,
    expected: &ExpectedSessionContext,
) -> Result<(), FeedError> {
    if plot_id == expected.plot_id && session_id == expected.session_id {
        return Ok(());
    }
    Err(FeedError::protocol(format!(
        "Session frame expected Plot/Session {:?}/{:?}, got {:?}/{:?}",
        expected.plot_id, expected.session_id, plot_id, session_id
    )))
}

/// Enforce the local walking-skeleton trust boundary before connecting.
///
/// User-only filesystem modes prevent other users from replacing or reading
/// the endpoint under the supported Nopal runtime layout. A future adversarial
/// same-user threat model still requires peer credentials or an authenticated
/// Session handshake to close the remaining check/connect race.
#[cfg(unix)]
fn validate_unix_endpoint_path(address: &str) -> io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    use std::path::Path;

    let path = Path::new(address);
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Session endpoint path must be absolute",
        ));
    }
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "Session endpoint must have a runtime directory",
        )
    })?;
    let parent_metadata = std::fs::symlink_metadata(parent)?;
    if parent_metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Session runtime directory must not be a symlink",
        ));
    }
    if !parent_metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Session endpoint parent is not a runtime directory",
        ));
    }
    if parent_metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Session runtime directory must be user-only",
        ));
    }

    let endpoint_metadata = std::fs::symlink_metadata(path)?;
    if endpoint_metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Session endpoint must not be a symlink",
        ));
    }
    if !endpoint_metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Session endpoint is not a Unix socket",
        ));
    }
    if endpoint_metadata.mode() & 0o077 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Session endpoint Unix socket must be user-only",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    #[cfg(unix)]
    use std::io::{self, Read, Write};

    use nopal_feed_client::session::{
        DURABLE_SESSION_EVENT_KIND, ExpectedSessionContext, MAX_SESSION_LINE_BYTES,
        SESSION_COMMAND_KIND, SESSION_FEED_ERROR_KIND, SESSION_REPLAY_COMPLETE_KIND,
        SESSION_SUBSCRIBE_KIND, SessionCommand, SessionCommandPayload, SessionSubscribe,
        parse_model_request, parse_session_command, parse_session_subscribe,
    };
    use nopal_feed_client::session_activity::DURABLE_SESSION_ACTIVITY_EVENT_KIND;

    use super::{ProductionFeedConnection, ProductionFeedTransport, SessionServerFrameDecoder};
    use crate::activity::VerifiedSessionEvent;
    use crate::session_feed::{
        ClientFeedFrame, FeedConnection, FeedError, FeedErrorKind, FeedTransport,
        SessionEndpointVersion, SessionFeedContext, SessionFeedServerFrame,
    };

    const PLOT_ID: &str = "plot-01";
    const SESSION_ID: &str = "session-01";

    fn expected() -> ExpectedSessionContext {
        ExpectedSessionContext::new(PLOT_ID, SESSION_ID).unwrap()
    }

    #[cfg(unix)]
    #[test]
    fn production_feed_connection_serializes_an_lf_framed_subscribe() {
        use std::os::unix::net::UnixStream;

        let (stream, mut peer) = UnixStream::pair().unwrap();
        let mut connection =
            ProductionFeedConnection::from_unix_stream(stream, expected()).unwrap();
        connection
            .send(ClientFeedFrame::Subscribe(SessionSubscribe {
                kind: SESSION_SUBSCRIBE_KIND.to_owned(),
                request_id: "subscribe-1".to_owned(),
                plot_id: PLOT_ID.to_owned(),
                session_id: SESSION_ID.to_owned(),
                after_cursor: None,
                page_limit: 256,
                extra: BTreeMap::new(),
            }))
            .unwrap();

        let mut bytes = vec![0; 4096];
        let count = peer.read(&mut bytes).unwrap();
        assert_eq!(bytes[count - 1], b'\n');
        let subscribe =
            parse_session_subscribe(std::str::from_utf8(&bytes[..count - 1]).unwrap()).unwrap();
        assert_eq!(subscribe.request_id, "subscribe-1");
        assert_eq!(subscribe.plot_id, PLOT_ID);
        assert_eq!(subscribe.session_id, SESSION_ID);
    }

    #[cfg(unix)]
    #[test]
    fn pending_nonblocking_connect_queues_subscribe_until_a_poll_observes_readiness() {
        use std::os::unix::net::UnixStream;

        let (stream, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let mut connection =
            ProductionFeedConnection::from_unix_stream_state(stream, expected(), true).unwrap();
        connection
            .send(ClientFeedFrame::Subscribe(SessionSubscribe {
                kind: SESSION_SUBSCRIBE_KIND.to_owned(),
                request_id: "subscribe-pending".to_owned(),
                plot_id: PLOT_ID.to_owned(),
                session_id: SESSION_ID.to_owned(),
                after_cursor: None,
                page_limit: 256,
                extra: BTreeMap::new(),
            }))
            .unwrap();

        let mut bytes = vec![0; 4096];
        assert_eq!(
            peer.read(&mut bytes).unwrap_err().kind(),
            io::ErrorKind::WouldBlock,
            "send must queue without waiting for connection establishment"
        );
        assert_eq!(connection.try_receive().unwrap(), None);
        let count = peer.read(&mut bytes).unwrap();
        let subscribe =
            parse_session_subscribe(std::str::from_utf8(&bytes[..count - 1]).unwrap()).unwrap();
        assert_eq!(subscribe.request_id, "subscribe-pending");
    }

    #[cfg(unix)]
    #[test]
    fn production_feed_connection_serializes_an_exact_prompt_command() {
        use std::os::unix::net::UnixStream;

        let (stream, mut peer) = UnixStream::pair().unwrap();
        let mut connection =
            ProductionFeedConnection::from_unix_stream(stream, expected()).unwrap();
        connection
            .send(ClientFeedFrame::Prompt(SessionCommand {
                kind: SESSION_COMMAND_KIND.to_owned(),
                command_id: "command-1".to_owned(),
                plot_id: PLOT_ID.to_owned(),
                session_id: SESSION_ID.to_owned(),
                command: SessionCommandPayload::Prompt {
                    text: "Explain this".to_owned(),
                    extra: BTreeMap::new(),
                },
                extra: BTreeMap::new(),
            }))
            .unwrap();

        let mut bytes = vec![0; 4096];
        let count = peer.read(&mut bytes).unwrap();
        let command =
            parse_session_command(std::str::from_utf8(&bytes[..count - 1]).unwrap()).unwrap();
        assert_eq!(command.command_id, "command-1");
        assert_eq!(command.plot_id, PLOT_ID);
        assert_eq!(command.session_id, SESSION_ID);
    }

    #[cfg(unix)]
    #[test]
    fn production_feed_connection_rejects_foreign_client_frames_and_close_is_terminal() {
        use std::os::unix::net::UnixStream;

        let (stream, mut peer) = UnixStream::pair().unwrap();
        peer.set_nonblocking(true).unwrap();
        let mut connection =
            ProductionFeedConnection::from_unix_stream(stream, expected()).unwrap();
        let error = connection
            .send(ClientFeedFrame::Subscribe(SessionSubscribe {
                kind: SESSION_SUBSCRIBE_KIND.to_owned(),
                request_id: "subscribe-foreign".to_owned(),
                plot_id: "plot-other".to_owned(),
                session_id: SESSION_ID.to_owned(),
                after_cursor: None,
                page_limit: 256,
                extra: BTreeMap::new(),
            }))
            .unwrap_err();
        assert_eq!(error.kind, FeedErrorKind::Protocol);
        let mut buffer = [0_u8; 1];
        assert_eq!(
            peer.read(&mut buffer).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        peer.set_nonblocking(false).unwrap();
        peer.write_all(format!("{}\n", durable_event("late", PLOT_ID, SESSION_ID)).as_bytes())
            .unwrap();
        connection.close();
        let error = connection.try_receive().unwrap_err();
        assert_eq!(error.kind, FeedErrorKind::Eof);
    }

    fn durable_event(event_id: &str, plot_id: &str, session_id: &str) -> String {
        serde_json::json!({
            "kind": DURABLE_SESSION_EVENT_KIND,
            "event_id": event_id,
            "plot_id": plot_id,
            "session_id": session_id,
            "stream_id": "stream-1",
            "sequence": 1,
            "previous_cursor": null,
            "cursor": "cursor-1",
            "event": {"type": "session_ready"}
        })
        .to_string()
    }

    fn replay_complete(plot_id: &str, session_id: &str) -> String {
        serde_json::json!({
            "kind": SESSION_REPLAY_COMPLETE_KIND,
            "request_id": "subscribe-1",
            "plot_id": plot_id,
            "session_id": session_id,
            "stream_id": "stream-1",
            "cursor": "cursor-1",
            "sequence": 1,
            "event_count": 1
        })
        .to_string()
    }

    fn activity_event(event_id: &str, plot_id: &str, session_id: &str) -> String {
        serde_json::json!({
            "kind": DURABLE_SESSION_ACTIVITY_EVENT_KIND,
            "event_id": event_id,
            "plot_id": plot_id,
            "session_id": session_id,
            "stream_id": "stream-1",
            "sequence": 2,
            "previous_cursor": "cursor-1",
            "cursor": "cursor-2",
            "event": {
                "type": "command_started",
                "activity_id": "activity-1",
                "tool_call_id": "call-1",
                "command": "printf exact",
                "started_at": "2026-07-13T17:00:00Z"
            },
            "future_envelope": {"preserved": true}
        })
        .to_string()
    }

    #[cfg(unix)]
    #[test]
    fn negotiated_production_decoder_keeps_v2_strict_and_v3_mixed() {
        use std::os::unix::net::UnixStream;

        let (stream, mut peer) = UnixStream::pair().unwrap();
        let mut v2_connection =
            ProductionFeedConnection::from_unix_stream(stream, expected()).unwrap();
        let v3_only = activity_event("activity-v2-mismatch", PLOT_ID, SESSION_ID);
        peer.write_all(format!("{v3_only}\n").as_bytes()).unwrap();
        let mismatch = v2_connection.try_receive().unwrap_err();
        assert_eq!(mismatch.kind, FeedErrorKind::Protocol);

        let (stream, mut peer) = UnixStream::pair().unwrap();
        let mut v3_connection = ProductionFeedConnection::from_unix_stream_for_version(
            stream,
            expected(),
            SessionEndpointVersion::V3,
        )
        .unwrap();
        let v2 = durable_event("event-v2", PLOT_ID, SESSION_ID);
        let v3 = activity_event("event-v3", PLOT_ID, SESSION_ID);
        peer.write_all(format!("{v2}\n{v3}\n").as_bytes()).unwrap();

        assert!(matches!(
            v3_connection.try_receive().unwrap(),
            Some(SessionFeedServerFrame::Event(event))
                if matches!(event.as_ref(), VerifiedSessionEvent::V2(event) if event.event_id == "event-v2")
        ));
        assert!(matches!(
            v3_connection.try_receive().unwrap(),
            Some(SessionFeedServerFrame::Event(event))
                if matches!(event.as_ref(), VerifiedSessionEvent::V3(event)
                    if event.event_id == "event-v3"
                        && event.extra.contains_key("future_envelope"))
        ));

        let (stream, mut peer) = UnixStream::pair().unwrap();
        let mut foreign = ProductionFeedConnection::from_unix_stream_for_version(
            stream,
            expected(),
            SessionEndpointVersion::V3,
        )
        .unwrap();
        let foreign_v3 = activity_event("event-foreign", "plot-other", SESSION_ID);
        peer.write_all(format!("{foreign_v3}\n").as_bytes())
            .unwrap();
        let error = foreign.try_receive().unwrap_err();
        assert_eq!(error.kind, FeedErrorKind::Protocol);
        assert!(error.message.contains("expected Plot/Session"));
    }

    #[cfg(unix)]
    #[test]
    fn v4_connection_decodes_model_state_and_serializes_exact_switch_request() {
        use std::os::unix::net::UnixStream;

        let (stream, mut peer) = UnixStream::pair().unwrap();
        let mut connection = ProductionFeedConnection::from_unix_stream_for_version(
            stream,
            expected(),
            SessionEndpointVersion::V4,
        )
        .unwrap();
        let state = serde_json::json!({
            "kind": "nopal.session.model.state/v1",
            "plot_id": PLOT_ID,
            "session_id": SESSION_ID,
            "request_id": null,
            "state_epoch": "epoch-1",
            "revision": 1,
            "agent_state": "idle",
            "current": {"provider": "nopal-proof", "id": "a", "name": "A"},
            "available": [
                {"provider": "nopal-proof", "id": "a", "name": "A"},
                {"provider": "nopal-proof", "id": "b", "name": "B"}
            ]
        });
        peer.write_all(format!("{state}\n").as_bytes()).unwrap();
        assert!(matches!(
            connection.try_receive().unwrap(),
            Some(SessionFeedServerFrame::ModelState(state))
                if state.current.as_ref().is_some_and(|model| model.id == "a")
        ));

        connection
            .send(ClientFeedFrame::Model(
                nopal_feed_client::session::SessionModelRequest {
                    kind: nopal_feed_client::session::SESSION_MODEL_REQUEST_KIND.to_owned(),
                    request_id: "switch-1".to_owned(),
                    plot_id: PLOT_ID.to_owned(),
                    session_id: SESSION_ID.to_owned(),
                    request: nopal_feed_client::session::SessionModelRequestPayload::Switch {
                        model: nopal_feed_client::session::SessionModelReference {
                            provider: "nopal-proof".to_owned(),
                            id: "b".to_owned(),
                            extra: BTreeMap::new(),
                        },
                        extra: BTreeMap::new(),
                    },
                    extra: BTreeMap::new(),
                },
            ))
            .unwrap();
        let mut bytes = vec![0; 4096];
        let count = peer.read(&mut bytes).unwrap();
        let request = parse_model_request(
            std::str::from_utf8(&bytes[..count - 1]).expect("UTF-8 model request"),
        )
        .expect("valid model switch request");
        assert_eq!(request.request_id, "switch-1");
    }

    #[cfg(unix)]
    #[test]
    fn production_feed_connection_reads_split_and_coalesced_frames_without_blocking() {
        use std::os::unix::net::UnixStream;

        let (stream, mut peer) = UnixStream::pair().unwrap();
        let mut connection =
            ProductionFeedConnection::from_unix_stream(stream, expected()).unwrap();
        assert_eq!(connection.try_receive().unwrap(), None);

        let first = durable_event("event-1", PLOT_ID, SESSION_ID);
        let second = replay_complete(PLOT_ID, SESSION_ID);
        let split = first.len() / 2;
        peer.write_all(&first.as_bytes()[..split]).unwrap();
        assert_eq!(connection.try_receive().unwrap(), None);

        peer.write_all(format!("{}\n{second}\n", &first[split..]).as_bytes())
            .unwrap();
        assert!(matches!(
            connection.try_receive().unwrap(),
            Some(SessionFeedServerFrame::Event(event))
                if matches!(event.as_ref(), VerifiedSessionEvent::V2(event)
                    if event.event_id == "event-1")
        ));
        assert!(matches!(
            connection.try_receive().unwrap(),
            Some(SessionFeedServerFrame::ReplayComplete(complete))
                if complete.request_id == "subscribe-1"
        ));
        assert_eq!(connection.try_receive().unwrap(), None);
    }

    #[test]
    fn production_feed_decoder_rejects_framing_utf8_oversize_and_foreign_context() {
        let mut decoder = SessionServerFrameDecoder::new(expected());
        assert!(matches!(
            decoder.push(b"\r\n").as_slice(),
            [Err(error)] if error.kind == FeedErrorKind::Protocol && error.message.contains("CRLF")
        ));
        assert!(matches!(
            decoder.push(&[0xff, b'\n']).as_slice(),
            [Err(error)] if error.kind == FeedErrorKind::Protocol && error.message.contains("UTF-8")
        ));

        let oversized = vec![b'x'; MAX_SESSION_LINE_BYTES + 1];
        assert!(matches!(
            decoder.push(&oversized).as_slice(),
            [Err(error)] if error.kind == FeedErrorKind::Protocol && error.message.contains("limit")
        ));
        assert!(decoder.push(b"\n").is_empty());

        let foreign = durable_event("event-foreign", "plot-other", SESSION_ID);
        assert!(matches!(
            decoder.push(format!("{foreign}\n").as_bytes()).as_slice(),
            [Err(error)] if error.kind == FeedErrorKind::Protocol
                && error.message.contains("expected Plot/Session")
        ));

        let foreign_complete = replay_complete(PLOT_ID, "session-other");
        assert!(matches!(
            decoder.push(format!("{foreign_complete}\n").as_bytes()).as_slice(),
            [Err(error)] if error.kind == FeedErrorKind::Protocol
        ));

        let foreign_error = serde_json::json!({
            "kind": SESSION_FEED_ERROR_KIND,
            "request_id": "subscribe-1",
            "plot_id": "plot-other",
            "session_id": SESSION_ID,
            "code": "foreign_session",
            "retryable": false,
            "message": "foreign"
        });
        assert!(matches!(
            decoder.push(format!("{foreign_error}\n").as_bytes()).as_slice(),
            [Err(error)] if error.kind == FeedErrorKind::Protocol
        ));
    }

    #[cfg(unix)]
    fn receive_terminal_error(connection: &mut ProductionFeedConnection) -> FeedError {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            match connection.try_receive() {
                Err(error) => return error,
                Ok(None) if std::time::Instant::now() < deadline => std::thread::yield_now(),
                Ok(None) => panic!("Session feed did not expose peer closure before the deadline"),
                Ok(Some(frame)) => {
                    panic!("unexpected Session frame before peer closure: {frame:?}")
                }
            }
        }
    }

    #[cfg(unix)]
    #[test]
    fn production_feed_connection_classifies_clean_and_partial_eof() {
        use std::os::unix::net::UnixStream;

        let (stream, peer) = UnixStream::pair().unwrap();
        let mut connection =
            ProductionFeedConnection::from_unix_stream(stream, expected()).unwrap();
        drop(peer);
        let clean = receive_terminal_error(&mut connection);
        assert_eq!(clean.kind, FeedErrorKind::Eof);
        assert!(clean.retryable());

        let (stream, mut peer) = UnixStream::pair().unwrap();
        let mut connection =
            ProductionFeedConnection::from_unix_stream(stream, expected()).unwrap();
        peer.write_all(b"{\"kind\":").unwrap();
        drop(peer);
        // A nonblocking stream may expose the final bytes before its peer closure becomes
        // observable, so classification belongs to the bounded polling contract, not one read.
        let partial = receive_terminal_error(&mut connection);
        assert_eq!(partial.kind, FeedErrorKind::Protocol);
        assert!(!partial.retryable());
    }

    #[cfg(unix)]
    #[test]
    fn production_feed_transport_classifies_absent_and_untrusted_endpoints() {
        let root = tempfile::tempdir().unwrap();
        set_mode(root.path(), 0o700);
        let missing = root.path().join("missing.sock");
        let context = SessionFeedContext {
            plot_id: PLOT_ID.to_owned(),
            session_id: SESSION_ID.to_owned(),
            endpoint_kind: "nopal.session/v2".to_owned(),
            endpoint_address: missing.to_string_lossy().into_owned(),
        };
        let absent = match ProductionFeedTransport.connect(&context) {
            Ok(_) => panic!("missing endpoint unexpectedly connected"),
            Err(error) => error,
        };
        assert_eq!(absent.kind, FeedErrorKind::EndpointAbsent);
        assert!(absent.retryable());

        let mut relative = context;
        relative.endpoint_address = "relative.sock".to_owned();
        let untrusted = match ProductionFeedTransport.connect(&relative) {
            Ok(_) => panic!("relative endpoint unexpectedly connected"),
            Err(error) => error,
        };
        assert_eq!(untrusted.kind, FeedErrorKind::Protocol);
        assert!(!untrusted.retryable());
    }

    #[cfg(unix)]
    fn set_mode(path: &std::path::Path, mode: u32) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).unwrap();
    }
}
