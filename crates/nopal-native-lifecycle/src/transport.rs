//! Bounded activation transport over renderer-neutral byte streams.

use std::fmt;
use std::io::{self, Read, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};

use crate::activation::{
    ActivationAck, ActivationAckValidator, ActivationDeadline, ActivationOutcome,
    ActivationProtocolError, ActivationRequest, ActivationRequestValidator,
    MAX_ACTIVATION_FRAME_BYTES, encode_ack, encode_request,
};
use crate::supervisor::{
    NativeApplicationAck, NativeApplicationUnavailable, PrimaryActivationService,
    SecondaryActivationForwarder,
};

const NONCE_RANDOM_BYTES: usize = 16;
const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";

/// A closed transport failure that never promotes a secondary into a primary.
#[derive(Debug)]
pub enum ActivationTransportError {
    /// The byte stream failed or exceeded its configured deadline.
    Io(io::Error),
    /// A complete frame violated the exact activation protocol.
    Protocol(ActivationProtocolError),
    /// The peer ended the stream before sending the required newline terminator.
    IncompleteFrame,
    /// The operating system could not provide a fresh activation nonce.
    EntropyUnavailable(String),
}

impl fmt::Display for ActivationTransportError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "native activation I/O failed: {error}"),
            Self::Protocol(error) => {
                write!(formatter, "native activation protocol failed: {error}")
            }
            Self::IncompleteFrame => formatter.write_str(
                "native activation peer ended before the bounded newline frame completed",
            ),
            Self::EntropyUnavailable(error) => {
                write!(formatter, "generate native activation nonce: {error}")
            }
        }
    }
}

impl std::error::Error for ActivationTransportError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Protocol(error) => Some(error),
            Self::IncompleteFrame | Self::EntropyUnavailable(_) => None,
        }
    }
}

impl From<io::Error> for ActivationTransportError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ActivationProtocolError> for ActivationTransportError {
    fn from(error: ActivationProtocolError) -> Self {
        Self::Protocol(error)
    }
}

/// Generates a request bound to one scope and a fresh 128-bit operating-system nonce.
pub fn generate_activation_request(
    scope_fingerprint: impl Into<String>,
) -> Result<ActivationRequest, ActivationTransportError> {
    let mut random = [0_u8; NONCE_RANDOM_BYTES];
    getrandom::fill(&mut random)
        .map_err(|error| ActivationTransportError::EntropyUnavailable(error.to_string()))?;

    let mut nonce = String::with_capacity(NONCE_RANDOM_BYTES * 2);
    for byte in random {
        nonce.push(char::from(LOWER_HEX[usize::from(byte >> 4)]));
        nonce.push(char::from(LOWER_HEX[usize::from(byte & 0x0f)]));
    }
    ActivationRequest::new(scope_fingerprint, nonce).map_err(Into::into)
}

/// Reads exactly one bounded newline frame and returns as soon as it completes.
///
/// A peer does not need to close its connection. The reader performs at most
/// `MAX_ACTIVATION_FRAME_BYTES + 1` one-byte reads and never grows beyond that bound.
pub fn read_activation_frame(reader: &mut impl Read) -> Result<Vec<u8>, ActivationTransportError> {
    let mut frame = Vec::with_capacity(MAX_ACTIVATION_FRAME_BYTES.min(256));
    let mut byte = [0_u8; 1];

    while frame.len() <= MAX_ACTIVATION_FRAME_BYTES {
        match reader.read(&mut byte) {
            Ok(0) => return Err(ActivationTransportError::IncompleteFrame),
            Ok(_) => {
                frame.push(byte[0]);
                if frame.len() > MAX_ACTIVATION_FRAME_BYTES {
                    return Err(ActivationProtocolError::FrameTooLarge {
                        actual: frame.len(),
                        limit: MAX_ACTIVATION_FRAME_BYTES,
                    }
                    .into());
                }
                if byte[0] == b'\n' {
                    return Ok(frame);
                }
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error.into()),
        }
    }

    Err(ActivationProtocolError::FrameTooLarge {
        actual: MAX_ACTIVATION_FRAME_BYTES + 1,
        limit: MAX_ACTIVATION_FRAME_BYTES,
    }
    .into())
}

fn write_activation_frame(
    writer: &mut impl Write,
    frame: &[u8],
) -> Result<(), ActivationTransportError> {
    writer.write_all(frame)?;
    writer.flush()?;
    Ok(())
}

/// Sends one caller-supplied request and validates the exact matching acknowledgement.
///
/// This entry point exists for deterministic testing and protocol adapters. Normal
/// secondary launches should call [`exchange_activation`] so every launch gets a
/// fresh cryptographic nonce.
pub fn exchange_activation_request(
    stream: &mut (impl Read + Write),
    request: &ActivationRequest,
) -> Result<ActivationOutcome, ActivationTransportError> {
    write_activation_frame(stream, &encode_request(request)?)?;
    let frame = read_activation_frame(stream)?;
    let mut validator = ActivationAckValidator::new(request);
    Ok(validator.decode_and_validate(&frame)?.outcome())
}

/// Generates and exchanges one exact activation request on a connected stream.
pub fn exchange_activation(
    stream: &mut (impl Read + Write),
    scope_fingerprint: impl Into<String>,
) -> Result<ActivationOutcome, ActivationTransportError> {
    let request = generate_activation_request(scope_fingerprint)?;
    exchange_activation_request(stream, &request)
}

/// Serves one request, waits for the host action to complete, then emits its exact ack.
///
/// Host action failures are represented by the closed `Unavailable` protocol outcome.
/// Invalid or replayed requests fail before invoking the host and receive no ack.
pub fn serve_activation<A>(
    stream: &mut (impl Read + Write),
    validator: &mut ActivationRequestValidator,
    activation_service: &A,
    deadline: ActivationDeadline,
) -> Result<ActivationOutcome, ActivationTransportError>
where
    A: PrimaryActivationService + ?Sized,
{
    let frame = read_activation_frame(stream)?;
    ensure_deadline_open(deadline)?;
    let request = validator.decode_and_validate(&frame)?;
    let outcome = match catch_unwind(AssertUnwindSafe(|| {
        activation_service.activate_primary(deadline)
    })) {
        Ok(Ok(NativeApplicationAck::Focused)) if !deadline.is_elapsed() => {
            ActivationOutcome::Focused
        }
        Ok(Ok(NativeApplicationAck::Reopened)) if !deadline.is_elapsed() => {
            ActivationOutcome::Reopened
        }
        Ok(Ok(_) | Err(_)) | Err(_) => ActivationOutcome::Unavailable,
    };
    let acknowledgement = ActivationAck::for_request(&request, outcome);
    write_activation_frame(stream, &encode_ack(&acknowledgement)?)?;
    Ok(outcome)
}

fn ensure_deadline_open(deadline: ActivationDeadline) -> Result<(), ActivationTransportError> {
    deadline
        .remaining()
        .map(|_| ())
        .map_err(|error| io::Error::new(io::ErrorKind::TimedOut, error.to_string()).into())
}

#[cfg(unix)]
mod unix {
    use std::io::{self, Read, Write};
    use std::os::unix::net::UnixStream;
    use std::sync::{Mutex, MutexGuard, TryLockError};
    use std::thread;
    use std::time::Duration;

    use super::{
        ActivationDeadline, ActivationOutcome, ActivationRequest, ActivationRequestValidator,
        ActivationTransportError, NativeApplicationAck, NativeApplicationUnavailable,
        PrimaryActivationService, SecondaryActivationForwarder, exchange_activation,
        serve_activation,
    };

    const VALIDATION_NONCE: &str = "00000000000000000000000000000000";
    const IO_RETRY_INTERVAL: Duration = Duration::from_millis(1);
    const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(1);

    fn invalid_timeout(message: &'static str) -> ActivationTransportError {
        io::Error::new(io::ErrorKind::InvalidInput, message).into()
    }

    fn deadline_after(timeout: Duration) -> Result<ActivationDeadline, ActivationTransportError> {
        ActivationDeadline::after(timeout).map_err(|error| {
            invalid_timeout(match error {
                crate::activation::ActivationDeadlineError::Zero => {
                    "native activation total timeout must be greater than zero"
                }
                crate::activation::ActivationDeadlineError::Unrepresentable => {
                    "native activation total timeout is not representable"
                }
            })
        })
    }

    fn remaining_until(deadline: ActivationDeadline) -> io::Result<Duration> {
        deadline.remaining().map_err(|_| {
            io::Error::new(
                io::ErrorKind::TimedOut,
                "native activation total I/O deadline elapsed",
            )
        })
    }

    struct DeadlineUnixStream<'a> {
        stream: &'a mut UnixStream,
        deadline: ActivationDeadline,
    }

    impl<'a> DeadlineUnixStream<'a> {
        fn new(stream: &'a mut UnixStream, deadline: ActivationDeadline) -> io::Result<Self> {
            stream.set_nonblocking(true)?;
            Ok(Self { stream, deadline })
        }

        fn wait_for_retry(&self) -> io::Result<()> {
            let remaining = remaining_until(self.deadline)?;
            thread::sleep(IO_RETRY_INTERVAL.min(remaining));
            Ok(())
        }
    }

    impl Read for DeadlineUnixStream<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            loop {
                remaining_until(self.deadline)?;
                match self.stream.read(buffer) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        self.wait_for_retry()?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    result => return result,
                }
            }
        }
    }

    impl Write for DeadlineUnixStream<'_> {
        fn write(&mut self, buffer: &[u8]) -> io::Result<usize> {
            loop {
                remaining_until(self.deadline)?;
                match self.stream.write(buffer) {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        self.wait_for_retry()?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    result => return result,
                }
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            loop {
                remaining_until(self.deadline)?;
                match self.stream.flush() {
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                        self.wait_for_retry()?;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    result => return result,
                }
            }
        }
    }

    /// A one-connection secondary forwarder with one total Unix exchange deadline.
    pub struct UnixActivationForwarder {
        stream: Mutex<UnixStream>,
        scope_fingerprint: String,
        timeout: Duration,
    }

    impl UnixActivationForwarder {
        pub fn new(
            stream: UnixStream,
            scope_fingerprint: impl Into<String>,
            timeout: Duration,
        ) -> Result<Self, ActivationTransportError> {
            let scope_fingerprint = scope_fingerprint.into();
            ActivationRequest::new(&scope_fingerprint, VALIDATION_NONCE)?;
            deadline_after(timeout)?;
            Ok(Self {
                stream: Mutex::new(stream),
                scope_fingerprint,
                timeout,
            })
        }

        fn lock_stream_until(
            &self,
            deadline: ActivationDeadline,
        ) -> Result<MutexGuard<'_, UnixStream>, ActivationTransportError> {
            loop {
                match self.stream.try_lock() {
                    Ok(stream) => return Ok(stream),
                    Err(TryLockError::Poisoned(poisoned)) => {
                        return Ok(poisoned.into_inner());
                    }
                    Err(TryLockError::WouldBlock) => {
                        let remaining = remaining_until(deadline)?;
                        thread::sleep(LOCK_RETRY_INTERVAL.min(remaining));
                    }
                }
            }
        }
    }

    impl SecondaryActivationForwarder for UnixActivationForwarder {
        fn forward(&self) -> Result<NativeApplicationAck, NativeApplicationUnavailable> {
            let outcome = deadline_after(self.timeout)
                .and_then(|deadline| {
                    let mut stream = self.lock_stream_until(deadline)?;
                    let mut stream = DeadlineUnixStream::new(&mut stream, deadline)?;
                    exchange_activation(&mut stream, &self.scope_fingerprint)
                })
                .map_err(|error| NativeApplicationUnavailable::new(error.to_string()))?;
            match outcome {
                ActivationOutcome::Focused => Ok(NativeApplicationAck::Focused),
                ActivationOutcome::Reopened => Ok(NativeApplicationAck::Reopened),
                ActivationOutcome::Unavailable => Err(NativeApplicationUnavailable::new(
                    "running native application reported activation unavailable",
                )),
            }
        }
    }

    /// Serves one Unix activation connection under one total read, host, and write deadline.
    pub fn serve_unix_activation<A>(
        mut stream: UnixStream,
        validator: &mut ActivationRequestValidator,
        activation_service: &A,
        timeout: Duration,
    ) -> Result<ActivationOutcome, ActivationTransportError>
    where
        A: PrimaryActivationService + ?Sized,
    {
        let deadline = deadline_after(timeout)?;
        let mut stream = DeadlineUnixStream::new(&mut stream, deadline)?;
        serve_activation(&mut stream, validator, activation_service, deadline)
    }
}

#[cfg(unix)]
pub use unix::{UnixActivationForwarder, serve_unix_activation};
