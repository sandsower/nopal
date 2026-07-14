//! Loopback-only client for the versioned Rondo Core HTTP contract.

use std::fmt;
use std::net::IpAddr;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use url::{Host, Url};

const SURFACE: &str = "rondo.core/v1";
const MAX_RESPONSE_BYTES: u64 = 1024 * 1024;
const DOT_SEGMENT_PLACEHOLDER: &str = "nopal-rondo-opaque-dot-segment";
// Some supported platforms can represent a wider `Instant` than others.
// Use the signed 64-bit nanosecond range as the portable monotonic-clock
// ceiling, then still ask the current clock before handing the value to ureq.
const MAX_PORTABLE_TIMEOUT: Duration = Duration::from_nanos(i64::MAX as u64);

/// A request to submit one approved execution manifest.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct SubmitRequest {
    pub manifest_path: String,
    pub manifest_sha256: String,
    pub repo_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plot_id: Option<String>,
}

impl SubmitRequest {
    pub fn new(
        manifest_path: impl Into<String>,
        manifest_sha256: impl Into<String>,
        repo_id: impl Into<String>,
    ) -> Self {
        Self {
            manifest_path: manifest_path.into(),
            manifest_sha256: manifest_sha256.into(),
            repo_id: repo_id.into(),
            plot_id: None,
        }
    }

    pub fn for_plot(
        manifest_path: impl Into<String>,
        manifest_sha256: impl Into<String>,
        repo_id: impl Into<String>,
        plot_id: impl Into<String>,
    ) -> Self {
        Self {
            manifest_path: manifest_path.into(),
            manifest_sha256: manifest_sha256.into(),
            repo_id: repo_id.into(),
            plot_id: Some(plot_id.into()),
        }
    }
}

/// Opaque identifiers needed to observe one Rondo-owned run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunHandle {
    pub repo_id: String,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plot_id: Option<String>,
}

impl RunHandle {
    pub fn new(repo_id: impl Into<String>, run_id: impl Into<String>) -> Self {
        Self {
            repo_id: repo_id.into(),
            run_id: run_id.into(),
            plot_id: None,
        }
    }

    pub fn for_plot(
        repo_id: impl Into<String>,
        run_id: impl Into<String>,
        plot_id: impl Into<String>,
    ) -> Self {
        Self {
            repo_id: repo_id.into(),
            run_id: run_id.into(),
            plot_id: Some(plot_id.into()),
        }
    }
}

/// The accepted run handle returned by Rondo Core.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SubmitResponse {
    pub surface: String,
    pub service_id: String,
    pub repo_id: String,
    pub plot_id: Option<String>,
    pub run_id: String,
    pub status: String,
    pub event_cursor: String,
    pub deduplicated: bool,
}

impl SubmitResponse {
    pub fn run_handle(&self) -> RunHandle {
        match self.plot_id.as_deref() {
            Some(plot_id) => RunHandle::for_plot(
                self.repo_id.clone(),
                self.run_id.clone(),
                plot_id.to_owned(),
            ),
            None => RunHandle::new(self.repo_id.clone(), self.run_id.clone()),
        }
    }
}

/// One opaque, Rondo-owned evidence reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidencePointer {
    pub artifact_kind: String,
    pub uri: String,
}

/// One bounded status observation from Rondo Core.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunStatusResponse {
    pub run_id: String,
    pub plot_id: Option<String>,
    pub status: String,
    pub last_event: Option<Value>,
    pub evidence_pointers: Vec<EvidencePointer>,
    pub event_cursor: String,
}

/// Incremental events after an optional opaque cursor.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RunEventsResponse {
    pub plot_id: Option<String>,
    pub events: Vec<Value>,
    pub next_event_cursor: String,
    pub has_more: bool,
}

impl RunEventsResponse {
    pub fn evidence_pointers(&self) -> Vec<EvidencePointer> {
        self.events
            .iter()
            .filter_map(|event| evidence_pointer_from_event(event).ok().flatten())
            .collect()
    }
}

/// Verified process identity and readiness returned by Rondo Core.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub surface: String,
    pub runtime_version: String,
    pub instance_id: String,
    pub service_mode: String,
    pub ready: bool,
    pub active_run_count: u64,
}

/// Stable Rondo Core failures from the pinned HTTP contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CoreErrorCode {
    InvalidRequest,
    DigestConflict,
    InvalidManifest,
    UnapprovedManifest,
    CapacityExhausted,
    RunNotFound,
    OrchestratorUnavailable,
    CoreUnavailable,
    LoopbackRequired,
}

impl CoreErrorCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid_request",
            Self::DigestConflict => "digest_conflict",
            Self::InvalidManifest => "invalid_manifest",
            Self::UnapprovedManifest => "unapproved_manifest",
            Self::CapacityExhausted => "capacity_exhausted",
            Self::RunNotFound => "run_not_found",
            Self::OrchestratorUnavailable => "orchestrator_unavailable",
            Self::CoreUnavailable => "core_unavailable",
            Self::LoopbackRequired => "loopback_required",
        }
    }

    const fn expected_status(self) -> u16 {
        match self {
            Self::InvalidRequest => 400,
            Self::DigestConflict => 409,
            Self::InvalidManifest | Self::UnapprovedManifest => 422,
            Self::CapacityExhausted => 429,
            Self::RunNotFound => 404,
            Self::OrchestratorUnavailable | Self::CoreUnavailable => 503,
            Self::LoopbackRequired => 403,
        }
    }
}

impl fmt::Display for CoreErrorCode {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Sanitized client failures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ClientError {
    Configuration(ConfigurationError),
    Request(RequestError),
    Core(CoreErrorCode),
    Transport(TransportError),
    Protocol(ProtocolError),
}

impl fmt::Display for ClientError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Configuration(error) => write!(
                formatter,
                "Rondo Core client configuration is invalid: {error}"
            ),
            Self::Request(error) => write!(formatter, "Rondo Core request is invalid: {error}"),
            Self::Core(code) => write!(formatter, "Rondo Core rejected the request: {code}"),
            Self::Transport(TransportError::Timeout) => {
                formatter.write_str("Rondo Core request timed out")
            }
            Self::Transport(TransportError::Unavailable) => {
                formatter.write_str("Rondo Core is unavailable")
            }
            Self::Protocol(error) => write!(formatter, "Rondo Core response is invalid: {error}"),
        }
    }
}

impl std::error::Error for ClientError {}

/// Configuration failures that are safe to surface to an operator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigurationError {
    InvalidBaseUrl,
    InvalidTimeout,
}

impl fmt::Display for ConfigurationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBaseUrl => {
                formatter.write_str("base URL must be a literal loopback HTTP origin")
            }
            Self::InvalidTimeout => formatter
                .write_str("timeout must be greater than zero and representable by the clock"),
        }
    }
}

/// Local request validation failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestError {
    MissingManifestPath,
    InvalidManifestDigest,
    MissingRepoId,
    InvalidRepoId,
    RepoIdTooLong,
    RepoIdContainsControl,
    MissingPlotId,
    InvalidPlotId,
    PlotIdTooLong,
    PlotIdContainsControl,
    MissingRunId,
    InvalidEventCursor,
}

impl fmt::Display for RequestError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::MissingManifestPath => "manifest path is required",
            Self::InvalidManifestDigest => "manifest digest must be lowercase SHA-256",
            Self::MissingRepoId => "repository identifier is required",
            Self::InvalidRepoId => "repository identifier must not have surrounding whitespace",
            Self::RepoIdTooLong => "repository identifier must not exceed 512 UTF-8 bytes",
            Self::RepoIdContainsControl => {
                "repository identifier must not contain control characters"
            }
            Self::MissingPlotId => "Plot identifier is required when supplied",
            Self::InvalidPlotId => "Plot identifier must not have surrounding whitespace",
            Self::PlotIdTooLong => "Plot identifier must not exceed 512 UTF-8 bytes",
            Self::PlotIdContainsControl => "Plot identifier must not contain control characters",
            Self::MissingRunId => "run identifier is required",
            Self::InvalidEventCursor => {
                "event cursor must match rondo.core/v1:<decimal> with at most 20 digits when provided"
            }
        };
        formatter.write_str(message)
    }
}

/// Connectivity failures intentionally omit addresses and request contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TransportError {
    Timeout,
    Unavailable,
}

/// Raw failures emitted by an HTTP transport before client-level classification.
///
/// Connectivity failures become [`ClientError::Transport`]. Failures involving a
/// received response become [`ClientError::Protocol`]. Keeping this boundary type
/// separate prevents the public client error from representing contradictory
/// states such as a malformed response classified as a connectivity failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WireError {
    Timeout,
    Unavailable,
    MalformedHttp,
    InvalidUtf8,
    ResponseTooLarge,
}

/// Protocol failures intentionally omit response bodies and internal details.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    MalformedHttp,
    InvalidUtf8,
    ResponseTooLarge,
    MalformedJson,
    InvalidResponse,
    SurfaceMismatch,
    RepoIdMismatch,
    RunIdMismatch,
    PlotIdMismatch,
    InconsistentSubmitStatus,
    UnexpectedStatus(u16),
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedHttp => formatter.write_str("HTTP response is malformed"),
            Self::InvalidUtf8 => formatter.write_str("response body is not valid UTF-8"),
            Self::ResponseTooLarge => {
                formatter.write_str("response body exceeds the supported limit")
            }
            Self::MalformedJson => formatter.write_str("response body is not valid JSON"),
            Self::InvalidResponse => {
                formatter.write_str("required response fields are missing or invalid")
            }
            Self::SurfaceMismatch => {
                formatter.write_str("response surface does not match rondo.core/v1")
            }
            Self::RepoIdMismatch => {
                formatter.write_str("response repository identifier does not match the request")
            }
            Self::RunIdMismatch => {
                formatter.write_str("response run identifier does not match the request")
            }
            Self::PlotIdMismatch => {
                formatter.write_str("response Plot identifier does not match the request")
            }
            Self::InconsistentSubmitStatus => {
                formatter.write_str("submission status conflicts with its deduplication flag")
            }
            Self::UnexpectedStatus(status) => write!(formatter, "unexpected HTTP status {status}"),
        }
    }
}

/// Minimal HTTP method vocabulary needed by the Core contract.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
}

/// Transport-neutral request used for deterministic client tests and adapters.
///
/// `url` is an already validated and encoded absolute loopback URL.
#[derive(Clone, Debug, PartialEq)]
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub json_body: Option<Value>,
}

/// Transport-neutral response. The raw body is parsed only by the protocol layer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpResponse {
    pub status: u16,
    pub body: String,
}

impl HttpResponse {
    pub fn new(status: u16, body: Value) -> Self {
        Self {
            status,
            body: body.to_string(),
        }
    }

    pub fn raw(status: u16, body: impl Into<String>) -> Self {
        Self {
            status,
            body: body.into(),
        }
    }
}

/// Injectable transport for the small synchronous Core client.
pub trait Transport: Send + Sync {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, WireError>;
}

/// Production blocking transport backed by one configured ureq agent.
#[derive(Clone)]
pub struct UreqTransport {
    agent: ureq::Agent,
}

impl UreqTransport {
    fn new(timeout: Duration) -> Result<Self, ClientError> {
        validate_timeout(timeout)?;

        let config = ureq::Agent::config_builder()
            .timeout_global(Some(timeout))
            .proxy(None)
            .max_redirects(0)
            .http_status_as_error(false)
            .build();

        Ok(Self {
            agent: ureq::Agent::new_with_config(config),
        })
    }
}

fn validate_timeout(timeout: Duration) -> Result<(), ClientError> {
    if timeout.is_zero()
        || timeout > MAX_PORTABLE_TIMEOUT
        || Instant::now().checked_add(timeout).is_none()
    {
        return Err(ClientError::Configuration(
            ConfigurationError::InvalidTimeout,
        ));
    }
    Ok(())
}

impl Transport for UreqTransport {
    fn send(&self, request: HttpRequest) -> Result<HttpResponse, WireError> {
        let result = match request.method {
            HttpMethod::Get => self
                .agent
                .get(request.url.as_str())
                .header("Accept", "application/json")
                .call(),
            HttpMethod::Post => {
                let Some(body) = request.json_body else {
                    return Err(WireError::Unavailable);
                };
                self.agent
                    .post(request.url.as_str())
                    .header("Accept", "application/json")
                    .send_json(body)
            }
        };

        let mut response = result.map_err(map_ureq_error)?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(MAX_RESPONSE_BYTES + 1)
            .read_to_vec()
            .map_err(map_ureq_error)?;
        if body.len() > MAX_RESPONSE_BYTES as usize {
            return Err(WireError::ResponseTooLarge);
        }
        let body = String::from_utf8(body).map_err(|_| WireError::InvalidUtf8)?;
        Ok(HttpResponse::raw(status, body))
    }
}

/// Synchronous client for `rondo.core/v1` submission and observation.
pub struct RondoCoreClient<T = UreqTransport> {
    base_url: Url,
    transport: T,
}

impl RondoCoreClient<UreqTransport> {
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, ClientError> {
        let base_url = validate_base_url(base_url)?;
        let transport = UreqTransport::new(timeout)?;
        Ok(Self {
            base_url,
            transport,
        })
    }
}

impl<T: Transport> RondoCoreClient<T> {
    pub fn with_transport(base_url: &str, transport: T) -> Result<Self, ClientError> {
        Ok(Self {
            base_url: validate_base_url(base_url)?,
            transport,
        })
    }

    pub fn submit(&self, request: SubmitRequest) -> Result<SubmitResponse, ClientError> {
        validate_submit_request(&request)?;
        let expected_repo_id = request.repo_id.clone();
        let expected_plot_id = request.plot_id.clone();
        let response = self.transport.send(HttpRequest {
            method: HttpMethod::Post,
            url: endpoint(&self.base_url, &["execution-requests"]).to_string(),
            json_body: Some(
                serde_json::to_value(request)
                    .map_err(|_| ClientError::Protocol(ProtocolError::InvalidResponse))?,
            ),
        })?;

        match response.status {
            200 | 202 => {
                let parsed: SubmitResponse = parse_json(&response.body)?;
                validate_submit_response(
                    response.status,
                    &expected_repo_id,
                    expected_plot_id.as_deref(),
                    &parsed,
                )?;
                Ok(parsed)
            }
            status => Err(parse_core_error(status, &response.body)),
        }
    }

    pub fn health(&self) -> Result<HealthResponse, ClientError> {
        let response = self.transport.send(HttpRequest {
            method: HttpMethod::Get,
            url: endpoint(&self.base_url, &["health"]).to_string(),
            json_body: None,
        })?;

        if response.status != 200 {
            return Err(parse_core_error(response.status, &response.body));
        }

        let parsed: HealthResponse = parse_json(&response.body)?;
        validate_health_response(&parsed)?;
        Ok(parsed)
    }

    pub fn status(&self, handle: RunHandle) -> Result<RunStatusResponse, ClientError> {
        validate_handle(&handle)?;
        let url = run_url(&self.base_url, &handle, false, None);
        let response = self.transport.send(HttpRequest {
            method: HttpMethod::Get,
            url,
            json_body: None,
        })?;

        if response.status != 200 {
            return Err(parse_core_error(response.status, &response.body));
        }

        let parsed: RawRunStatusResponse = parse_json(&response.body)?;
        validate_run_response(
            &handle,
            &parsed.surface,
            &parsed.repo_id,
            &parsed.run_id,
            parsed.plot_id.as_deref(),
        )?;
        require_nonempty(&parsed.status)?;
        parse_response_cursor(&parsed.event_cursor)?;
        if !parsed.last_event.is_null() {
            validate_run_fact(&handle, &parsed.last_event)?;
            evidence_pointer_from_event(&parsed.last_event)?;
        }
        if parsed
            .evidence_pointers
            .iter()
            .any(|pointer| !valid_evidence_pointer(pointer))
        {
            return Err(ClientError::Protocol(ProtocolError::InvalidResponse));
        }

        Ok(RunStatusResponse {
            run_id: parsed.run_id,
            plot_id: parsed.plot_id,
            status: parsed.status,
            last_event: (!parsed.last_event.is_null()).then_some(parsed.last_event),
            evidence_pointers: parsed.evidence_pointers,
            event_cursor: parsed.event_cursor,
        })
    }

    pub fn events(
        &self,
        handle: RunHandle,
        event_cursor: Option<&str>,
    ) -> Result<RunEventsResponse, ClientError> {
        validate_handle(&handle)?;
        let requested_offset = match event_cursor {
            Some(cursor) => parse_request_cursor(cursor)?,
            None => "0".to_owned(),
        };

        let url = run_url(&self.base_url, &handle, true, event_cursor);
        let response = self.transport.send(HttpRequest {
            method: HttpMethod::Get,
            url,
            json_body: None,
        })?;

        if response.status != 200 {
            return Err(parse_core_error(response.status, &response.body));
        }

        let parsed: RawRunEventsResponse = parse_json(&response.body)?;
        validate_run_response(
            &handle,
            &parsed.surface,
            &parsed.repo_id,
            &parsed.run_id,
            parsed.plot_id.as_deref(),
        )?;
        let next_offset = parse_response_cursor(&parsed.next_event_cursor)?;
        let expected_next_offset = add_decimal(&requested_offset, parsed.events.len());
        if next_offset != expected_next_offset {
            return Err(ClientError::Protocol(ProtocolError::InvalidResponse));
        }
        for event in &parsed.events {
            validate_run_fact(&handle, event)?;
            evidence_pointer_from_event(event)?;
        }

        Ok(RunEventsResponse {
            plot_id: parsed.plot_id,
            events: parsed.events,
            next_event_cursor: parsed.next_event_cursor,
            has_more: parsed.has_more,
        })
    }
}

#[derive(Deserialize)]
struct RawRunStatusResponse {
    surface: String,
    run_id: String,
    repo_id: String,
    plot_id: Option<String>,
    status: String,
    last_event: Value,
    evidence_pointers: Vec<EvidencePointer>,
    event_cursor: String,
}

#[derive(Deserialize)]
struct RawRunEventsResponse {
    surface: String,
    run_id: String,
    repo_id: String,
    plot_id: Option<String>,
    events: Vec<Value>,
    next_event_cursor: String,
    has_more: bool,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    code: String,
}

fn validate_base_url(input: &str) -> Result<Url, ClientError> {
    let url = Url::parse(input)
        .map_err(|_| ClientError::Configuration(ConfigurationError::InvalidBaseUrl))?;
    let host_is_loopback = match url.host() {
        Some(Host::Ipv4(address)) => IpAddr::V4(address).is_loopback(),
        Some(Host::Ipv6(address)) => IpAddr::V6(address).is_loopback(),
        Some(Host::Domain(_)) | None => false,
    };
    let is_origin = url.path() == "/" && url.query().is_none() && url.fragment().is_none();
    let has_no_credentials = url.username().is_empty() && url.password().is_none();

    if url.scheme() != "http" || !host_is_loopback || !is_origin || !has_no_credentials {
        return Err(ClientError::Configuration(
            ConfigurationError::InvalidBaseUrl,
        ));
    }

    Ok(url)
}

fn validate_submit_request(request: &SubmitRequest) -> Result<(), ClientError> {
    if is_blank(&request.manifest_path) {
        return Err(ClientError::Request(RequestError::MissingManifestPath));
    }
    if !is_lowercase_sha256(&request.manifest_sha256) {
        return Err(ClientError::Request(RequestError::InvalidManifestDigest));
    }
    if is_blank(&request.repo_id) {
        return Err(ClientError::Request(RequestError::MissingRepoId));
    }
    if has_surrounding_whitespace(&request.repo_id) {
        return Err(ClientError::Request(RequestError::InvalidRepoId));
    }
    validate_repo_id_shape(&request.repo_id)?;
    if let Some(plot_id) = request.plot_id.as_deref() {
        validate_plot_id(plot_id)?;
    }
    Ok(())
}

fn validate_handle(handle: &RunHandle) -> Result<(), ClientError> {
    if is_blank(&handle.repo_id) {
        return Err(ClientError::Request(RequestError::MissingRepoId));
    }
    if has_surrounding_whitespace(&handle.repo_id) {
        return Err(ClientError::Request(RequestError::InvalidRepoId));
    }
    validate_repo_id_shape(&handle.repo_id)?;
    if is_blank(&handle.run_id) {
        return Err(ClientError::Request(RequestError::MissingRunId));
    }
    if let Some(plot_id) = handle.plot_id.as_deref() {
        validate_plot_id(plot_id)?;
    }
    Ok(())
}

fn validate_repo_id_shape(repo_id: &str) -> Result<(), ClientError> {
    if repo_id.len() > 512 {
        return Err(ClientError::Request(RequestError::RepoIdTooLong));
    }
    if repo_id.chars().any(char::is_control) {
        return Err(ClientError::Request(RequestError::RepoIdContainsControl));
    }
    Ok(())
}

fn validate_plot_id(plot_id: &str) -> Result<(), ClientError> {
    if is_blank(plot_id) {
        return Err(ClientError::Request(RequestError::MissingPlotId));
    }
    if has_surrounding_whitespace(plot_id) {
        return Err(ClientError::Request(RequestError::InvalidPlotId));
    }
    if plot_id.len() > 512 {
        return Err(ClientError::Request(RequestError::PlotIdTooLong));
    }
    if plot_id.chars().any(char::is_control) {
        return Err(ClientError::Request(RequestError::PlotIdContainsControl));
    }
    Ok(())
}

fn validate_submit_response(
    status: u16,
    expected_repo_id: &str,
    expected_plot_id: Option<&str>,
    response: &SubmitResponse,
) -> Result<(), ClientError> {
    if response.surface != SURFACE {
        return Err(ClientError::Protocol(ProtocolError::SurfaceMismatch));
    }
    if response.repo_id != expected_repo_id {
        return Err(ClientError::Protocol(ProtocolError::RepoIdMismatch));
    }
    validate_plot_echo(expected_plot_id, response.plot_id.as_deref())?;
    require_nonempty(&response.service_id)?;
    require_nonempty(&response.run_id)?;
    require_nonempty(&response.status)?;
    parse_response_cursor(&response.event_cursor)?;
    if (status == 200) != response.deduplicated {
        return Err(ClientError::Protocol(
            ProtocolError::InconsistentSubmitStatus,
        ));
    }
    Ok(())
}

fn validate_health_response(response: &HealthResponse) -> Result<(), ClientError> {
    if response.surface != SURFACE {
        return Err(ClientError::Protocol(ProtocolError::SurfaceMismatch));
    }
    if is_blank(&response.runtime_version)
        || !valid_instance_id(&response.instance_id)
        || response.service_mode != "trackerless_core"
    {
        return Err(ClientError::Protocol(ProtocolError::InvalidResponse));
    }
    Ok(())
}

fn valid_instance_id(value: &str) -> bool {
    if value.len() != 36 {
        return false;
    }

    value.bytes().enumerate().all(|(index, byte)| match index {
        8 | 13 | 18 | 23 => byte == b'-',
        _ => byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase(),
    })
}

fn validate_run_response(
    handle: &RunHandle,
    surface: &str,
    repo_id: &str,
    run_id: &str,
    plot_id: Option<&str>,
) -> Result<(), ClientError> {
    if surface != SURFACE {
        return Err(ClientError::Protocol(ProtocolError::SurfaceMismatch));
    }
    if repo_id != handle.repo_id {
        return Err(ClientError::Protocol(ProtocolError::RepoIdMismatch));
    }
    if run_id != handle.run_id {
        return Err(ClientError::Protocol(ProtocolError::RunIdMismatch));
    }
    validate_plot_echo(handle.plot_id.as_deref(), plot_id)?;
    Ok(())
}

fn validate_plot_echo(
    expected_plot_id: Option<&str>,
    actual_plot_id: Option<&str>,
) -> Result<(), ClientError> {
    if expected_plot_id != actual_plot_id {
        return Err(ClientError::Protocol(ProtocolError::PlotIdMismatch));
    }
    if let Some(plot_id) = actual_plot_id {
        validate_plot_id(plot_id)
            .map_err(|_| ClientError::Protocol(ProtocolError::InvalidResponse))?;
    }
    Ok(())
}

fn validate_run_fact(handle: &RunHandle, event: &Value) -> Result<(), ClientError> {
    let Some(event) = event.as_object() else {
        return Ok(());
    };
    let Some(event_type) = event.get("type").and_then(Value::as_str) else {
        return Ok(());
    };
    if !event_type.starts_with("rondo.run.") {
        return Ok(());
    }

    let namespace = event
        .get("namespace")
        .and_then(Value::as_object)
        .ok_or(ClientError::Protocol(ProtocolError::InvalidResponse))?;
    validate_fact_identity(handle, namespace)?;

    if event.get("payload_omitted") != Some(&Value::Bool(true)) {
        validate_fact_identity(handle, event)?;
    }
    Ok(())
}

fn validate_fact_identity(
    handle: &RunHandle,
    identity: &serde_json::Map<String, Value>,
) -> Result<(), ClientError> {
    if identity.get("repo_id").and_then(Value::as_str) != Some(handle.repo_id.as_str()) {
        return Err(ClientError::Protocol(ProtocolError::RepoIdMismatch));
    }
    if identity.get("run_id").and_then(Value::as_str) != Some(handle.run_id.as_str()) {
        return Err(ClientError::Protocol(ProtocolError::RunIdMismatch));
    }
    let plot_id = optional_identity_string(identity, "plot_id")?;
    validate_plot_echo(handle.plot_id.as_deref(), plot_id)
}

fn optional_identity_string<'a>(
    identity: &'a serde_json::Map<String, Value>,
    key: &str,
) -> Result<Option<&'a str>, ClientError> {
    match identity.get(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(_other) => Err(ClientError::Protocol(ProtocolError::InvalidResponse)),
    }
}

fn require_nonempty(value: &str) -> Result<(), ClientError> {
    if is_blank(value) {
        Err(ClientError::Protocol(ProtocolError::InvalidResponse))
    } else {
        Ok(())
    }
}

fn valid_evidence_pointer(pointer: &EvidencePointer) -> bool {
    valid_bounded_text(&pointer.artifact_kind, 1_024)
        && valid_bounded_text(&pointer.uri, 2_048)
        && pointer
            .uri
            .strip_prefix("rondo-run://")
            .is_some_and(|opaque_reference| !opaque_reference.is_empty())
}

fn evidence_pointer_from_event(event: &Value) -> Result<Option<EvidencePointer>, ClientError> {
    let Some(event) = event.as_object() else {
        return Ok(None);
    };
    if event.get("type").and_then(Value::as_str) != Some("rondo.run.evidence_recorded")
        || event.get("payload_omitted") == Some(&Value::Bool(true))
    {
        return Ok(None);
    }
    let pointer = EvidencePointer {
        artifact_kind: event
            .get("artifact_kind")
            .and_then(Value::as_str)
            .ok_or(ClientError::Protocol(ProtocolError::InvalidResponse))?
            .to_owned(),
        uri: event
            .get("uri")
            .and_then(Value::as_str)
            .ok_or(ClientError::Protocol(ProtocolError::InvalidResponse))?
            .to_owned(),
    };
    if !valid_evidence_pointer(&pointer) {
        return Err(ClientError::Protocol(ProtocolError::InvalidResponse));
    }
    Ok(Some(pointer))
}

fn valid_bounded_text(value: &str, maximum: usize) -> bool {
    !is_blank(value)
        && !has_surrounding_whitespace(value)
        && value.len() <= maximum
        && !value.chars().any(char::is_control)
}

fn parse_request_cursor(value: &str) -> Result<String, ClientError> {
    parse_cursor(value).ok_or(ClientError::Request(RequestError::InvalidEventCursor))
}

fn parse_response_cursor(value: &str) -> Result<String, ClientError> {
    parse_cursor(value).ok_or(ClientError::Protocol(ProtocolError::InvalidResponse))
}

fn parse_cursor(value: &str) -> Option<String> {
    let encoded = value.strip_prefix("rondo.core/v1:")?;
    if encoded.is_empty()
        || encoded.len() > 20
        || !encoded.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let canonical = encoded.trim_start_matches('0');
    Some(if canonical.is_empty() { "0" } else { canonical }.to_owned())
}

fn add_decimal(value: &str, increment: usize) -> String {
    let mut digits = value.as_bytes().to_vec();
    let mut carry = increment;
    let mut index = digits.len();

    while carry > 0 && index > 0 {
        index -= 1;
        let sum = usize::from(digits[index] - b'0') + carry % 10;
        carry /= 10;
        digits[index] = b'0' + (sum % 10) as u8;
        if sum >= 10 {
            carry += 1;
        }
    }

    while carry > 0 {
        digits.insert(0, b'0' + (carry % 10) as u8);
        carry /= 10;
    }

    digits.into_iter().map(char::from).collect()
}

fn is_blank(value: &str) -> bool {
    value.trim().is_empty()
}

fn has_surrounding_whitespace(value: &str) -> bool {
    value.trim() != value
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn endpoint(base_url: &Url, suffix: &[&str]) -> Url {
    let mut url = base_url.clone();
    if let Ok(mut segments) = url.path_segments_mut() {
        segments.pop_if_empty();
        segments.extend(["api", "v1"]);
        segments.extend(suffix.iter().copied());
    }
    url
}

fn run_url(base_url: &Url, handle: &RunHandle, events: bool, event_cursor: Option<&str>) -> String {
    // WHATWG URL serialization removes `.` and `..` path segments. Serialize a
    // safe placeholder first, then replace only that segment with its percent-
    // encoded wire form. This preserves the opaque identifier without parsing
    // or assigning any semantic meaning to it in Nopal.
    let encoded_dot_segment = match handle.run_id.as_str() {
        "." => Some("%2E"),
        ".." => Some("%2E%2E"),
        _ => None,
    };
    let run_id_segment = if encoded_dot_segment.is_some() {
        DOT_SEGMENT_PLACEHOLDER
    } else {
        &handle.run_id
    };
    let mut url = endpoint(base_url, &["runs", run_id_segment]);
    if events && let Ok(mut segments) = url.path_segments_mut() {
        segments.push("events");
    }
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("repo_id", &handle.repo_id);
        if let Some(cursor) = event_cursor {
            query.append_pair("cursor", cursor);
        }
    }
    let serialized = url.to_string();
    match encoded_dot_segment {
        Some(segment) => serialized.replacen(DOT_SEGMENT_PLACEHOLDER, segment, 1),
        None => serialized,
    }
}

fn parse_json<T: DeserializeOwned>(body: &str) -> Result<T, ClientError> {
    let value = serde_json::from_str::<Value>(body)
        .map_err(|_| ClientError::Protocol(ProtocolError::MalformedJson))?;
    serde_json::from_value(value).map_err(|_| ClientError::Protocol(ProtocolError::InvalidResponse))
}

fn parse_core_error(status: u16, body: &str) -> ClientError {
    let Ok(envelope) = serde_json::from_str::<ErrorEnvelope>(body) else {
        return ClientError::Protocol(ProtocolError::UnexpectedStatus(status));
    };
    let Some(code) = core_error_code(&envelope.error.code) else {
        return ClientError::Protocol(ProtocolError::UnexpectedStatus(status));
    };
    if code.expected_status() != status {
        return ClientError::Protocol(ProtocolError::UnexpectedStatus(status));
    }
    ClientError::Core(code)
}

fn core_error_code(code: &str) -> Option<CoreErrorCode> {
    match code {
        "invalid_request" => Some(CoreErrorCode::InvalidRequest),
        "digest_conflict" => Some(CoreErrorCode::DigestConflict),
        "invalid_manifest" => Some(CoreErrorCode::InvalidManifest),
        "unapproved_manifest" => Some(CoreErrorCode::UnapprovedManifest),
        "capacity_exhausted" => Some(CoreErrorCode::CapacityExhausted),
        "run_not_found" => Some(CoreErrorCode::RunNotFound),
        "orchestrator_unavailable" => Some(CoreErrorCode::OrchestratorUnavailable),
        "core_unavailable" => Some(CoreErrorCode::CoreUnavailable),
        "loopback_required" => Some(CoreErrorCode::LoopbackRequired),
        _ => None,
    }
}

fn map_ureq_error(error: ureq::Error) -> WireError {
    match error {
        ureq::Error::Timeout(_) => WireError::Timeout,
        ureq::Error::Protocol(_)
        | ureq::Error::Http(_)
        | ureq::Error::BadUri(_)
        | ureq::Error::LargeResponseHeader(_, _) => WireError::MalformedHttp,
        ureq::Error::BodyExceedsLimit(_) => WireError::ResponseTooLarge,
        ureq::Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::InvalidData | std::io::ErrorKind::UnexpectedEof
            ) =>
        {
            WireError::MalformedHttp
        }
        _ => WireError::Unavailable,
    }
}

impl From<WireError> for ClientError {
    fn from(error: WireError) -> Self {
        match error {
            WireError::Timeout => Self::Transport(TransportError::Timeout),
            WireError::Unavailable => Self::Transport(TransportError::Unavailable),
            WireError::MalformedHttp => Self::Protocol(ProtocolError::MalformedHttp),
            WireError::InvalidUtf8 => Self::Protocol(ProtocolError::InvalidUtf8),
            WireError::ResponseTooLarge => Self::Protocol(ProtocolError::ResponseTooLarge),
        }
    }
}
