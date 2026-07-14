//! Bounded, renderer-neutral native activation wire protocol.
//!
//! Frames are one JSON value followed by an optional ASCII-whitespace suffix.
//! Encoders add a newline so Unix socket adapters can use line framing without
//! making transport ownership part of this module.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use std::collections::{HashSet, VecDeque};
use std::fmt;
use std::time::{Duration, Instant};

pub const ACTIVATION_REQUEST_KIND: &str = "nopal.native_activation_request/v1";
pub const ACTIVATION_ACK_KIND: &str = "nopal.native_activation_ack/v1";
pub const MAX_ACTIVATION_FRAME_BYTES: usize = 1_024;
pub const MAX_REPLAY_NONCES: usize = 4_096;

const SCOPE_FINGERPRINT_BYTES: usize = 64;
const NONCE_BYTES: usize = 32;

/// One monotonic budget shared by activation transport and renderer work.
#[derive(Clone, Copy, Debug)]
pub struct ActivationDeadline {
    ends_at: Instant,
}

impl ActivationDeadline {
    /// Starts one activation lifetime with a finite, non-zero total budget.
    pub fn after(timeout: Duration) -> Result<Self, ActivationDeadlineError> {
        if timeout.is_zero() {
            return Err(ActivationDeadlineError::Zero);
        }
        let ends_at = Instant::now()
            .checked_add(timeout)
            .ok_or(ActivationDeadlineError::Unrepresentable)?;
        Ok(Self { ends_at })
    }

    /// Returns the time left in the activation lifetime.
    pub fn remaining(self) -> Result<Duration, ActivationDeadlineElapsed> {
        self.ends_at
            .checked_duration_since(Instant::now())
            .filter(|remaining| !remaining.is_zero())
            .ok_or(ActivationDeadlineElapsed)
    }

    /// Reports whether the shared activation lifetime has elapsed.
    pub fn is_elapsed(self) -> bool {
        self.remaining().is_err()
    }
}

/// An invalid total activation budget.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ActivationDeadlineError {
    Zero,
    Unrepresentable,
}

impl fmt::Display for ActivationDeadlineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => {
                formatter.write_str("native activation total timeout must be greater than zero")
            }
            Self::Unrepresentable => {
                formatter.write_str("native activation total timeout is not representable")
            }
        }
    }
}

impl std::error::Error for ActivationDeadlineError {}

/// The shared monotonic activation lifetime has elapsed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ActivationDeadlineElapsed;

impl fmt::Display for ActivationDeadlineElapsed {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("native activation total deadline elapsed")
    }
}

impl std::error::Error for ActivationDeadlineElapsed {}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationRequest {
    scope_fingerprint: String,
    nonce: String,
}

impl ActivationRequest {
    pub fn new(
        scope_fingerprint: impl Into<String>,
        nonce: impl Into<String>,
    ) -> Result<Self, ActivationProtocolError> {
        let scope_fingerprint = scope_fingerprint.into();
        let nonce = nonce.into();
        validate_scope_fingerprint(&scope_fingerprint)?;
        validate_nonce(&nonce)?;
        Ok(Self {
            scope_fingerprint,
            nonce,
        })
    }

    pub fn scope_fingerprint(&self) -> &str {
        &self.scope_fingerprint
    }

    pub fn nonce(&self) -> &str {
        &self.nonce
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivationOutcome {
    Focused,
    Reopened,
    Unavailable,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ActivationAck {
    scope_fingerprint: String,
    nonce: String,
    outcome: ActivationOutcome,
}

impl ActivationAck {
    pub fn for_request(request: &ActivationRequest, outcome: ActivationOutcome) -> Self {
        Self {
            scope_fingerprint: request.scope_fingerprint.clone(),
            nonce: request.nonce.clone(),
            outcome,
        }
    }

    pub fn scope_fingerprint(&self) -> &str {
        &self.scope_fingerprint
    }

    pub fn nonce(&self) -> &str {
        &self.nonce
    }

    pub const fn outcome(&self) -> ActivationOutcome {
        self.outcome
    }
}

#[derive(Debug, Eq, PartialEq)]
pub enum ActivationProtocolError {
    FrameTooLarge {
        actual: usize,
        limit: usize,
    },
    MalformedJson(String),
    EncodingFailed(String),
    UnexpectedKind {
        expected: &'static str,
        actual: String,
    },
    InvalidScopeFingerprint,
    InvalidNonce,
    ScopeMismatch,
    NonceMismatch,
    ReplayedNonce,
    InvalidReplayCapacity {
        requested: usize,
        maximum: usize,
    },
}

impl fmt::Display for ActivationProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::FrameTooLarge { actual, limit } => {
                write!(
                    formatter,
                    "activation frame is {actual} bytes; limit is {limit}"
                )
            }
            Self::MalformedJson(error) => write!(formatter, "malformed activation frame: {error}"),
            Self::EncodingFailed(error) => write!(formatter, "encode activation frame: {error}"),
            Self::UnexpectedKind { expected, actual } => {
                write!(
                    formatter,
                    "activation kind must be {expected}, got {actual}"
                )
            }
            Self::InvalidScopeFingerprint => formatter.write_str(
                "activation scope fingerprint must be exactly 64 lowercase hexadecimal bytes",
            ),
            Self::InvalidNonce => formatter
                .write_str("activation nonce must be exactly 32 lowercase hexadecimal bytes"),
            Self::ScopeMismatch => formatter.write_str("activation scope fingerprint mismatch"),
            Self::NonceMismatch => formatter.write_str("activation nonce mismatch"),
            Self::ReplayedNonce => {
                formatter.write_str("activation nonce has already been consumed")
            }
            Self::InvalidReplayCapacity { requested, maximum } => write!(
                formatter,
                "activation replay window {requested} must be between 1 and {maximum} nonces"
            ),
        }
    }
}

impl std::error::Error for ActivationProtocolError {}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RequestWire {
    kind: String,
    scope_fingerprint: String,
    nonce: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AckWire {
    kind: String,
    scope_fingerprint: String,
    nonce: String,
    outcome: ActivationOutcome,
}

pub fn encode_request(request: &ActivationRequest) -> Result<Vec<u8>, ActivationProtocolError> {
    encode_frame(&RequestWire {
        kind: ACTIVATION_REQUEST_KIND.to_owned(),
        scope_fingerprint: request.scope_fingerprint.clone(),
        nonce: request.nonce.clone(),
    })
}

pub fn encode_ack(ack: &ActivationAck) -> Result<Vec<u8>, ActivationProtocolError> {
    encode_frame(&AckWire {
        kind: ACTIVATION_ACK_KIND.to_owned(),
        scope_fingerprint: ack.scope_fingerprint.clone(),
        nonce: ack.nonce.clone(),
        outcome: ack.outcome,
    })
}

/// Server-side exact-scope decoder with a bounded recent-nonce replay window.
///
/// The window rejects recent replays without permanently exhausting a long-lived
/// primary. Requests older than the configured window can become valid again.
/// Cryptographically random 128-bit nonces keep that bounded policy safe while
/// ensuring memory never grows with primary lifetime.
pub struct ActivationRequestValidator {
    expected_scope_fingerprint: String,
    consumed_nonces: HashSet<String>,
    nonce_order: VecDeque<String>,
    nonce_capacity: usize,
}

impl ActivationRequestValidator {
    pub fn new(
        expected_scope_fingerprint: impl Into<String>,
    ) -> Result<Self, ActivationProtocolError> {
        Self::with_nonce_capacity(expected_scope_fingerprint, MAX_REPLAY_NONCES)
    }

    pub fn with_nonce_capacity(
        expected_scope_fingerprint: impl Into<String>,
        nonce_capacity: usize,
    ) -> Result<Self, ActivationProtocolError> {
        let expected_scope_fingerprint = expected_scope_fingerprint.into();
        validate_scope_fingerprint(&expected_scope_fingerprint)?;
        if nonce_capacity == 0 || nonce_capacity > MAX_REPLAY_NONCES {
            return Err(ActivationProtocolError::InvalidReplayCapacity {
                requested: nonce_capacity,
                maximum: MAX_REPLAY_NONCES,
            });
        }
        Ok(Self {
            expected_scope_fingerprint,
            consumed_nonces: HashSet::with_capacity(nonce_capacity),
            nonce_order: VecDeque::with_capacity(nonce_capacity),
            nonce_capacity,
        })
    }

    pub fn decode_and_validate(
        &mut self,
        frame: &[u8],
    ) -> Result<ActivationRequest, ActivationProtocolError> {
        let wire: RequestWire = decode_frame(frame)?;
        validate_kind(&wire.kind, ACTIVATION_REQUEST_KIND)?;
        let request = ActivationRequest::new(wire.scope_fingerprint, wire.nonce)?;
        if request.scope_fingerprint != self.expected_scope_fingerprint {
            return Err(ActivationProtocolError::ScopeMismatch);
        }
        if self.consumed_nonces.contains(&request.nonce) {
            return Err(ActivationProtocolError::ReplayedNonce);
        }
        if self.consumed_nonces.len() == self.nonce_capacity
            && let Some(expired) = self.nonce_order.pop_front()
        {
            self.consumed_nonces.remove(&expired);
        }
        self.consumed_nonces.insert(request.nonce.clone());
        self.nonce_order.push_back(request.nonce.clone());
        Ok(request)
    }

    pub fn consumed_nonce_count(&self) -> usize {
        self.consumed_nonces.len()
    }
}

/// Client-side consume-once validator bound to one exact request.
pub struct ActivationAckValidator {
    expected_scope_fingerprint: String,
    expected_nonce: String,
    consumed: bool,
}

impl ActivationAckValidator {
    pub fn new(request: &ActivationRequest) -> Self {
        Self {
            expected_scope_fingerprint: request.scope_fingerprint.clone(),
            expected_nonce: request.nonce.clone(),
            consumed: false,
        }
    }

    pub fn decode_and_validate(
        &mut self,
        frame: &[u8],
    ) -> Result<ActivationAck, ActivationProtocolError> {
        if self.consumed {
            return Err(ActivationProtocolError::ReplayedNonce);
        }
        let wire: AckWire = decode_frame(frame)?;
        validate_kind(&wire.kind, ACTIVATION_ACK_KIND)?;
        validate_scope_fingerprint(&wire.scope_fingerprint)?;
        validate_nonce(&wire.nonce)?;
        if wire.scope_fingerprint != self.expected_scope_fingerprint {
            return Err(ActivationProtocolError::ScopeMismatch);
        }
        if wire.nonce != self.expected_nonce {
            return Err(ActivationProtocolError::NonceMismatch);
        }
        self.consumed = true;
        Ok(ActivationAck {
            scope_fingerprint: wire.scope_fingerprint,
            nonce: wire.nonce,
            outcome: wire.outcome,
        })
    }
}

fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>, ActivationProtocolError> {
    let mut frame = serde_json::to_vec(value)
        .map_err(|error| ActivationProtocolError::EncodingFailed(error.to_string()))?;
    frame.push(b'\n');
    ensure_frame_limit(frame.len())?;
    Ok(frame)
}

fn decode_frame<T: DeserializeOwned>(frame: &[u8]) -> Result<T, ActivationProtocolError> {
    ensure_frame_limit(frame.len())?;
    let mut deserializer = serde_json::Deserializer::from_slice(frame);
    let value = T::deserialize(&mut deserializer)
        .map_err(|error| ActivationProtocolError::MalformedJson(error.to_string()))?;
    deserializer
        .end()
        .map_err(|error| ActivationProtocolError::MalformedJson(error.to_string()))?;
    Ok(value)
}

fn ensure_frame_limit(actual: usize) -> Result<(), ActivationProtocolError> {
    if actual > MAX_ACTIVATION_FRAME_BYTES {
        Err(ActivationProtocolError::FrameTooLarge {
            actual,
            limit: MAX_ACTIVATION_FRAME_BYTES,
        })
    } else {
        Ok(())
    }
}

fn validate_kind(actual: &str, expected: &'static str) -> Result<(), ActivationProtocolError> {
    if actual == expected {
        Ok(())
    } else {
        Err(ActivationProtocolError::UnexpectedKind {
            expected,
            actual: actual.to_owned(),
        })
    }
}

fn validate_scope_fingerprint(value: &str) -> Result<(), ActivationProtocolError> {
    if is_exact_lower_hex(value, SCOPE_FINGERPRINT_BYTES) {
        Ok(())
    } else {
        Err(ActivationProtocolError::InvalidScopeFingerprint)
    }
}

fn validate_nonce(value: &str) -> Result<(), ActivationProtocolError> {
    if is_exact_lower_hex(value, NONCE_BYTES) {
        Ok(())
    } else {
        Err(ActivationProtocolError::InvalidNonce)
    }
}

fn is_exact_lower_hex(value: &str, expected_bytes: usize) -> bool {
    value.len() == expected_bytes
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(byte))
}

#[cfg(test)]
mod tests {
    use super::{
        ActivationAck, ActivationAckValidator, ActivationDeadline, ActivationDeadlineError,
        ActivationOutcome, ActivationProtocolError, ActivationRequest, ActivationRequestValidator,
        MAX_ACTIVATION_FRAME_BYTES, MAX_REPLAY_NONCES, encode_ack, encode_request,
    };
    use std::time::Duration;

    const SCOPE: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const OTHER_SCOPE: &str = "1123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
    const NONCE: &str = "0123456789abcdef0123456789abcdef";
    const OTHER_NONCE: &str = "1123456789abcdef0123456789abcdef";

    fn request() -> ActivationRequest {
        ActivationRequest::new(SCOPE, NONCE).expect("valid request fixture")
    }

    #[test]
    fn activation_deadline_rejects_invalid_budgets_and_counts_down_monotonically() {
        assert_eq!(
            ActivationDeadline::after(Duration::ZERO).expect_err("zero must be rejected"),
            ActivationDeadlineError::Zero
        );
        assert_eq!(
            ActivationDeadline::after(Duration::MAX)
                .expect_err("unrepresentable duration must be rejected"),
            ActivationDeadlineError::Unrepresentable
        );

        let deadline = ActivationDeadline::after(Duration::from_secs(1))
            .expect("finite deadline is representable");
        let first = deadline.remaining().expect("deadline remains open");
        let second = deadline.remaining().expect("deadline remains open");
        assert!(second <= first);
        assert!(first <= Duration::from_secs(1));
    }

    #[test]
    fn request_codec_emits_and_accepts_the_exact_v1_contract() {
        let request = request();
        let encoded = encode_request(&request).expect("encode request");
        assert_eq!(
            String::from_utf8(encoded.clone()).expect("request is UTF-8"),
            concat!(
                "{\"kind\":\"nopal.native_activation_request/v1\",",
                "\"scope_fingerprint\":\"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef\",",
                "\"nonce\":\"0123456789abcdef0123456789abcdef\"}\n"
            )
        );

        let mut validator = ActivationRequestValidator::new(SCOPE).expect("valid scope");
        assert_eq!(
            validator
                .decode_and_validate(&encoded)
                .expect("decode request"),
            request
        );
    }

    #[test]
    fn acknowledgements_echo_scope_and_nonce_for_every_closed_outcome() {
        for outcome in [
            ActivationOutcome::Focused,
            ActivationOutcome::Reopened,
            ActivationOutcome::Unavailable,
        ] {
            let request = request();
            let ack = ActivationAck::for_request(&request, outcome);
            let encoded = encode_ack(&ack).expect("encode ack");
            let mut validator = ActivationAckValidator::new(&request);
            let decoded = validator.decode_and_validate(&encoded).expect("decode ack");
            assert_eq!(decoded.outcome(), outcome);
            assert_eq!(decoded.scope_fingerprint(), SCOPE);
            assert_eq!(decoded.nonce(), NONCE);
        }
    }

    #[test]
    fn malformed_unknown_wrong_kind_and_trailing_request_data_fail_closed() {
        let fixtures = [
            b"not json".as_slice(),
            br#"{"kind":"nopal.native_activation_request/v1","scope_fingerprint":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","nonce":"0123456789abcdef0123456789abcdef","extra":true}"#,
            br#"{"kind":"nopal.native_activation_request/v2","scope_fingerprint":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","nonce":"0123456789abcdef0123456789abcdef"}"#,
            br#"{"kind":"nopal.native_activation_request/v1","scope_fingerprint":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","nonce":"0123456789abcdef0123456789abcdef"}{}"#,
        ];

        for fixture in fixtures {
            let mut validator = ActivationRequestValidator::new(SCOPE).expect("valid scope");
            assert!(validator.decode_and_validate(fixture).is_err());
        }
    }

    #[test]
    fn mismatched_scope_replayed_nonce_and_oversized_request_fail_closed() {
        let other_scope = ActivationRequest::new(OTHER_SCOPE, NONCE).expect("valid request");
        let other_scope = encode_request(&other_scope).expect("encode request");
        let mut validator = ActivationRequestValidator::new(SCOPE).expect("valid scope");
        assert!(matches!(
            validator.decode_and_validate(&other_scope),
            Err(ActivationProtocolError::ScopeMismatch)
        ));

        let encoded = encode_request(&request()).expect("encode request");
        validator
            .decode_and_validate(&encoded)
            .expect("first nonce is accepted");
        assert!(matches!(
            validator.decode_and_validate(&encoded),
            Err(ActivationProtocolError::ReplayedNonce)
        ));

        let oversized = vec![b' '; MAX_ACTIVATION_FRAME_BYTES + 1];
        assert!(matches!(
            validator.decode_and_validate(&oversized),
            Err(ActivationProtocolError::FrameTooLarge { .. })
        ));
    }

    #[test]
    fn acknowledgement_validator_rejects_wrong_scope_nonce_kind_and_replay() {
        let request = request();
        let good_ack = ActivationAck::for_request(&request, ActivationOutcome::Focused);
        let encoded = encode_ack(&good_ack).expect("encode ack");
        let mut validator = ActivationAckValidator::new(&request);
        validator
            .decode_and_validate(&encoded)
            .expect("first matching ack is accepted");
        assert!(matches!(
            validator.decode_and_validate(&encoded),
            Err(ActivationProtocolError::ReplayedNonce)
        ));

        let other_scope_request =
            ActivationRequest::new(OTHER_SCOPE, NONCE).expect("valid request");
        let wrong_scope = encode_ack(&ActivationAck::for_request(
            &other_scope_request,
            ActivationOutcome::Focused,
        ))
        .expect("encode ack");
        let mut validator = ActivationAckValidator::new(&request);
        assert!(matches!(
            validator.decode_and_validate(&wrong_scope),
            Err(ActivationProtocolError::ScopeMismatch)
        ));

        let other_nonce_request =
            ActivationRequest::new(SCOPE, OTHER_NONCE).expect("valid request");
        let wrong_nonce = encode_ack(&ActivationAck::for_request(
            &other_nonce_request,
            ActivationOutcome::Focused,
        ))
        .expect("encode ack");
        let mut validator = ActivationAckValidator::new(&request);
        assert!(matches!(
            validator.decode_and_validate(&wrong_nonce),
            Err(ActivationProtocolError::NonceMismatch)
        ));

        let wrong_kind = String::from_utf8(encoded).expect("ack is UTF-8").replace(
            "nopal.native_activation_ack/v1",
            "nopal.native_activation_ack/v2",
        );
        let mut validator = ActivationAckValidator::new(&request);
        assert!(matches!(
            validator.decode_and_validate(wrong_kind.as_bytes()),
            Err(ActivationProtocolError::UnexpectedKind { .. })
        ));
    }

    #[test]
    fn acknowledgement_validator_rejects_unknown_trailing_and_oversized_frames() {
        let request = request();
        let encoded = encode_ack(&ActivationAck::for_request(
            &request,
            ActivationOutcome::Focused,
        ))
        .expect("encode ack");
        let encoded_text = String::from_utf8(encoded).expect("ack is UTF-8");
        let unknown = encoded_text.replace("\"outcome\"", "\"extra\":true,\"outcome\"");
        let trailing = format!("{encoded_text}{{}}");
        let oversized = vec![b' '; MAX_ACTIVATION_FRAME_BYTES + 1];

        for frame in [
            unknown.as_bytes(),
            trailing.as_bytes(),
            oversized.as_slice(),
        ] {
            let mut validator = ActivationAckValidator::new(&request);
            assert!(validator.decode_and_validate(frame).is_err());
        }
    }

    #[test]
    fn invalid_scope_and_nonce_shapes_are_rejected_before_encoding() {
        assert!(matches!(
            ActivationRequest::new("short", NONCE),
            Err(ActivationProtocolError::InvalidScopeFingerprint)
        ));
        assert!(matches!(
            ActivationRequest::new(SCOPE, "NOT-LOWERCASE-HEX"),
            Err(ActivationProtocolError::InvalidNonce)
        ));
    }

    #[test]
    fn replay_tracking_is_a_bounded_recent_nonce_window() {
        let mut validator =
            ActivationRequestValidator::with_nonce_capacity(SCOPE, 2).expect("valid scope");
        let first = ActivationRequest::new(SCOPE, NONCE).expect("valid request");
        let second = ActivationRequest::new(SCOPE, OTHER_NONCE).expect("valid request");
        let third = ActivationRequest::new(SCOPE, "2123456789abcdef0123456789abcdef")
            .expect("valid request");

        validator
            .decode_and_validate(&encode_request(&first).expect("encode first"))
            .expect("accept first");
        validator
            .decode_and_validate(&encode_request(&second).expect("encode second"))
            .expect("accept second");
        validator
            .decode_and_validate(&encode_request(&third).expect("encode third"))
            .expect("fresh nonce remains accepted after the window reaches capacity");
        assert_eq!(validator.consumed_nonce_count(), 2);
        assert!(matches!(
            validator.decode_and_validate(&encode_request(&third).expect("encode recent replay")),
            Err(ActivationProtocolError::ReplayedNonce)
        ));
        validator
            .decode_and_validate(&encode_request(&first).expect("encode evicted nonce"))
            .expect("a nonce older than the explicit replay window is accepted again");
        assert!(matches!(
            validator.decode_and_validate(&encode_request(&first).expect("encode recent replay")),
            Err(ActivationProtocolError::ReplayedNonce)
        ));
        assert_eq!(validator.consumed_nonce_count(), 2);
    }

    #[test]
    fn long_lived_validator_accepts_fresh_nonces_without_growing_memory() {
        let mut validator =
            ActivationRequestValidator::new(SCOPE).expect("valid default replay window");
        for value in 0..(MAX_REPLAY_NONCES + 512) {
            let nonce = format!("{value:032x}");
            let request = ActivationRequest::new(SCOPE, nonce).expect("valid generated request");
            validator
                .decode_and_validate(&encode_request(&request).expect("encode generated request"))
                .expect("fresh nonce remains accepted for a long-lived primary");
        }
        assert_eq!(validator.consumed_nonce_count(), MAX_REPLAY_NONCES);

        let recent = ActivationRequest::new(SCOPE, format!("{:032x}", MAX_REPLAY_NONCES + 511))
            .expect("valid recent request");
        assert!(matches!(
            validator.decode_and_validate(&encode_request(&recent).expect("encode recent replay")),
            Err(ActivationProtocolError::ReplayedNonce)
        ));
        assert_eq!(validator.consumed_nonce_count(), MAX_REPLAY_NONCES);
    }

    #[test]
    fn zero_or_excessive_replay_window_is_rejected() {
        assert!(matches!(
            ActivationRequestValidator::with_nonce_capacity(SCOPE, 0),
            Err(ActivationProtocolError::InvalidReplayCapacity { .. })
        ));
        assert!(matches!(
            ActivationRequestValidator::with_nonce_capacity(SCOPE, MAX_REPLAY_NONCES + 1),
            Err(ActivationProtocolError::InvalidReplayCapacity { .. })
        ));
    }
}
