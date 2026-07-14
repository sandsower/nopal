//! Coordinates deterministic Nopal decisions and the provisional Rondo Core
//! lifecycle boundary for the Nopal product surface.
//!
//! Rendering lives at the edge (`main.rs`), while this module owns the typed
//! coordinator envelopes and adapter seams.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::Duration;

use nopal_core::diagnostics::{Code, Diagnostic};
use nopal_core::plot::ExecutionEvidencePointer;
use nopal_core::plot_execution::{AcceptanceInput, ObservationInput, cursor_position};
use nopal_core::plot_store::{self, PlotEnv};
use nopal_core::policy::{self, ActionClass, Decision, Mode, Placement};
use nopal_core::toon::{self, Value};
use nopal_rondo_client::{
    ClientError, HealthResponse, RondoCoreClient, RunEventsResponse, RunHandle, RunStatusResponse,
    SubmitRequest, SubmitResponse,
};
use nopal_rondo_lifecycle::{
    LifecycleReport, SUPPORTED_RONDO_RUNTIME_VERSION, StartOptions, StatePaths,
};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use sha2::{Digest, Sha256};

pub const STATUS_KIND: &str = "nopal.status/v1";
pub const PLACEMENT_KIND: &str = "nopal.placement/v1";
pub const RONDO_SERVICE_KIND: &str = "nopal.rondo_service/v1";
pub const RUN_START_DRY_RUN_KIND: &str = "nopal.run_start_dry_run/v1";
pub const RUN_SUBMIT_KIND: &str = "nopal.run_submit/v1";
pub const RUN_OBSERVATION_KIND: &str = "nopal.run_observation/v1";
const CONFIG_KIND: &str = "nopal.config/v1";
const CONFIG_PATH: &str = ".nopal/config.jsonc";
const RONDO_CORE_URL_ENV: &str = "NOPAL_RONDO_CORE_URL";
const DEFAULT_RONDO_REQUEST_TIMEOUT_MS: u64 = 10_000;
const DEFAULT_RUN_START_MODE: &str = "nopal_tui";
const DEFAULT_RUN_START_ACTION: &str = "run.start";
const INTERACTIVE_RONDO_WARNING: &str = "nopal: warning: Rondo Core is unavailable; AFK execution is disabled for this interactive session. Run `nopal rondo start`, then `nopal rondo health` for diagnostics.";

#[derive(Debug, Clone, Serialize)]
pub struct ModuleSummary {
    pub name: String,
    pub required: bool,
    pub state: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct NopalStatus {
    pub kind: &'static str,
    pub project: Option<String>,
    pub profile: Option<String>,
    pub ready: bool,
    pub modules: Vec<ModuleSummary>,
    pub missing_modules: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
    pub help: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlacementReport {
    pub kind: &'static str,
    pub ok: bool,
    pub mode: String,
    pub action: String,
    pub classes: Vec<String>,
    pub placement: Option<String>,
    pub placement_source: Option<String>,
    pub matched_rules: usize,
    pub diagnostics: Vec<Diagnostic>,
    pub explanation: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RondoServiceReport {
    pub kind: String,
    pub service_id: String,
    pub status: String,
    pub ok: bool,
    pub health: String,
    pub placement: String,
    pub state_path: String,
    pub log_path: String,
    pub base_url: Option<String>,
    pub runtime_version: Option<String>,
    pub instance_id: Option<String>,
    pub active_run_count: Option<u64>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunStartDryRunReport {
    pub kind: &'static str,
    pub dry_run: bool,
    pub would_submit: bool,
    pub readiness_ready: bool,
    pub placement: Option<String>,
    pub placement_source: Option<String>,
    pub rondo_status: String,
    pub rondo_log_path: String,
    pub blockers: Vec<String>,
    pub next_steps: Vec<String>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SubmittedRunHandle {
    pub service_id: String,
    pub repo_id: String,
    pub plot_id: String,
    pub run_id: String,
    pub status: String,
    pub event_cursor: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunSubmitReport {
    pub kind: &'static str,
    pub ok: bool,
    pub submitted: bool,
    pub deduplicated: bool,
    pub manifest_path: Option<String>,
    pub manifest_sha256: Option<String>,
    pub decision: Option<String>,
    pub placement: Option<String>,
    pub handle: Option<SubmittedRunHandle>,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservationHandle {
    pub repo_id: String,
    pub plot_id: String,
    pub run_id: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunObservationReport {
    pub kind: &'static str,
    pub ok: bool,
    pub handle: ObservationHandle,
    pub status: Option<String>,
    pub last_event: Option<JsonValue>,
    pub evidence_pointers: Vec<JsonValue>,
    pub event_cursor: Option<String>,
    pub events: Vec<JsonValue>,
    pub next_event_cursor: Option<String>,
    pub has_more: bool,
    pub settled: bool,
    pub diagnostics: Vec<String>,
}

#[derive(Debug, Clone)]
struct RunStartPolicyConfig {
    mode: Mode,
    action: String,
    classes: Vec<ActionClass>,
}

#[derive(Debug, Clone)]
struct RondoCoreConfig {
    base_url: Option<String>,
    request_timeout_ms: u64,
    repo_id: Option<String>,
}

#[derive(Debug, Clone)]
struct NopalConfig {
    run_start_policy: RunStartPolicyConfig,
    diagnostics: Vec<String>,
    valid: bool,
    rondo_core: RondoCoreConfig,
    rondo_diagnostics: Vec<String>,
    rondo_base_url_diagnostics: Vec<String>,
    rondo_timeout_diagnostics: Vec<String>,
    rondo_repo_id_diagnostics: Vec<String>,
}

trait RondoClientAdapter {
    fn health(&self) -> Result<HealthResponse, ClientError>;
    fn submit(&self, request: SubmitRequest) -> Result<SubmitResponse, ClientError>;
    fn status(&self, handle: RunHandle) -> Result<RunStatusResponse, ClientError>;
    fn events(
        &self,
        handle: RunHandle,
        cursor: Option<&str>,
    ) -> Result<RunEventsResponse, ClientError>;
}

trait RondoClientFactory {
    type Client: RondoClientAdapter;

    fn connect(&self, base_url: &str, timeout: Duration) -> Result<Self::Client, ClientError>;
}

struct ProductionRondoClientFactory;

impl RondoClientAdapter for RondoCoreClient {
    fn health(&self) -> Result<HealthResponse, ClientError> {
        RondoCoreClient::health(self)
    }

    fn submit(&self, request: SubmitRequest) -> Result<SubmitResponse, ClientError> {
        RondoCoreClient::submit(self, request)
    }

    fn status(&self, handle: RunHandle) -> Result<RunStatusResponse, ClientError> {
        RondoCoreClient::status(self, handle)
    }

    fn events(
        &self,
        handle: RunHandle,
        cursor: Option<&str>,
    ) -> Result<RunEventsResponse, ClientError> {
        RondoCoreClient::events(self, handle, cursor)
    }
}

impl RondoClientFactory for ProductionRondoClientFactory {
    type Client = RondoCoreClient;

    fn connect(&self, base_url: &str, timeout: Duration) -> Result<Self::Client, ClientError> {
        RondoCoreClient::new(base_url, timeout)
    }
}

pub fn status(root: &Path) -> io::Result<NopalStatus> {
    let status = nopal_core::status::status(root)?;
    let modules: Vec<ModuleSummary> = status
        .modules
        .iter()
        .map(|module| ModuleSummary {
            name: module.module.as_str().to_owned(),
            required: module.required,
            state: module.state.as_str().to_owned(),
        })
        .collect();
    let missing_modules = modules
        .iter()
        .filter(|module| module.state == "missing")
        .map(|module| module.name.clone())
        .collect();

    Ok(NopalStatus {
        kind: STATUS_KIND,
        project: status.project,
        profile: status.profile.map(|profile| profile.as_str().to_owned()),
        ready: status.ready,
        modules,
        missing_modules,
        diagnostics: status.diagnostics,
        help: status.help,
    })
}

pub fn placement(
    root: &Path,
    mode: Mode,
    action: &str,
    classes: &[ActionClass],
) -> io::Result<PlacementReport> {
    let req = policy::EvalRequest {
        mode,
        action,
        classes,
        env: &[],
    };
    let report = policy::run(root, policy::View::Placement, &req)?;
    Ok(PlacementReport {
        kind: PLACEMENT_KIND,
        ok: report.ok,
        mode: mode.as_str().to_owned(),
        action: action.to_owned(),
        classes: classes
            .iter()
            .map(|class| class.as_str().to_owned())
            .collect(),
        placement: report
            .placement
            .map(|placement| placement.as_str().to_owned()),
        placement_source: report
            .placement_source
            .map(|source| source.as_str().to_owned()),
        matched_rules: report.matched_rules.map_or(0, |rules| rules.len()),
        diagnostics: report.diagnostics,
        explanation: report.explanation.unwrap_or_default(),
    })
}

pub fn rondo_health(_root: &Path) -> io::Result<RondoServiceReport> {
    let paths = StatePaths::from_environment()?;
    Ok(lifecycle_report(
        nopal_rondo_lifecycle::health(&paths),
        "shared_user_runtime",
    ))
}

pub fn rondo_start(
    root: &Path,
    requested_placement: Option<Placement>,
) -> io::Result<RondoServiceReport> {
    rondo_start_or_restart(root, requested_placement, "start")
}

pub fn ensure_interactive_rondo(root: &Path) -> Option<&'static str> {
    let readiness = status(root).ok()?;
    if !readiness.ready {
        return None;
    }
    start_automatic_rondo_endpoint()
        .err()
        .map(|_| INTERACTIVE_RONDO_WARNING)
}

pub fn rondo_restart(
    root: &Path,
    requested_placement: Option<Placement>,
) -> io::Result<RondoServiceReport> {
    rondo_start_or_restart(root, requested_placement, "restart")
}

pub fn rondo_stop(_root: &Path) -> io::Result<RondoServiceReport> {
    let paths = StatePaths::from_environment()?;
    Ok(lifecycle_report(
        nopal_rondo_lifecycle::stop(&paths)?,
        Placement::SharedUserRuntime.as_str(),
    ))
}

fn rondo_start_or_restart(
    root: &Path,
    requested_placement: Option<Placement>,
    operation: &str,
) -> io::Result<RondoServiceReport> {
    let paths = StatePaths::from_environment()?;
    let config = nopal_config(root)?;
    let _placement = match effective_run_placement(root, requested_placement, &config)? {
        Some(placement) => placement,
        None => {
            return Ok(blocked_report(
                &paths,
                "Nopal run-start placement is blocked",
                &config.diagnostics,
            ));
        }
    };
    let executable = std::env::current_exe()?;
    let runtime = rondo_runtime(&executable)?;
    let options = StartOptions::new(paths, executable, runtime);
    let report = if operation == "restart" {
        nopal_rondo_lifecycle::restart(&options)?
    } else {
        nopal_rondo_lifecycle::start(&options)?
    };
    Ok(lifecycle_report(
        report,
        Placement::SharedUserRuntime.as_str(),
    ))
}

pub fn run_start_dry_run(root: &Path) -> io::Result<RunStartDryRunReport> {
    let readiness = status(root)?;
    let config = nopal_config(root)?;
    let placement = if config.valid {
        let policy = &config.run_start_policy;
        placement(root, policy.mode, &policy.action, &policy.classes)?
    } else {
        blocked_placement_report(&config.diagnostics)
    };
    let service = rondo_health(root)?;

    let mut blockers = Vec::new();
    if !readiness.ready {
        blockers.push("Nopal readiness is not green".to_owned());
    }
    if matches!(placement.placement.as_deref(), Some("blocked")) {
        blockers.push("Nopal placement decision is blocked".to_owned());
    }
    if !placement.ok {
        blockers.push("Nopal policy placement could not be evaluated".to_owned());
    }
    blockers.extend(config.diagnostics.iter().cloned());

    let mut next_steps =
        vec!["Review the displayed Nopal readiness and placement decision".to_owned()];
    if matches!(placement.placement.as_deref(), Some("blocked")) {
        next_steps
            .push("Resolve the Nopal placement blocker before starting Rondo Core".to_owned());
    } else if service.status == "running" {
        next_steps.push(
            "Configure rondo_core.base_url, then use `nopal run submit --manifest <path>`"
                .to_owned(),
        );
    } else {
        next_steps.push(
            "Configure a loopback rondo_core.base_url before submitting real work".to_owned(),
        );
    }

    Ok(RunStartDryRunReport {
        kind: RUN_START_DRY_RUN_KIND,
        dry_run: true,
        would_submit: false,
        readiness_ready: readiness.ready,
        placement: placement.placement,
        placement_source: placement.placement_source,
        rondo_status: service.status,
        rondo_log_path: service.log_path,
        blockers,
        next_steps,
        diagnostics: config.diagnostics,
    })
}

pub fn run_submit(
    root: &Path,
    manifest: &Path,
    plot_id: &str,
    state_dir: Option<&Path>,
) -> RunSubmitReport {
    if let Err(message) = validate_established_plot(plot_id, state_dir) {
        return fail_submit(empty_submit_report(), message);
    }
    let plot_env = PlotEnv::discover(state_dir);
    let override_url = rondo_core_url_override();
    run_submit_with_factory_mode(
        root,
        manifest,
        plot_id,
        override_url.as_deref(),
        &ProductionRondoClientFactory,
        true,
        Some(&plot_env),
    )
}

#[cfg(test)]
fn run_submit_with_factory<F: RondoClientFactory>(
    root: &Path,
    manifest: &Path,
    base_url_override: Option<&str>,
    factory: &F,
) -> RunSubmitReport {
    run_submit_for_plot_with_factory(root, manifest, "plot-test", base_url_override, factory)
}

#[cfg(test)]
fn run_submit_for_plot_with_factory<F: RondoClientFactory>(
    root: &Path,
    manifest: &Path,
    plot_id: &str,
    base_url_override: Option<&str>,
    factory: &F,
) -> RunSubmitReport {
    run_submit_with_factory_mode(
        root,
        manifest,
        plot_id,
        base_url_override,
        factory,
        false,
        None,
    )
}

fn run_submit_with_factory_mode<F: RondoClientFactory>(
    root: &Path,
    manifest: &Path,
    plot_id: &str,
    base_url_override: Option<&str>,
    factory: &F,
    auto_start: bool,
    plot_env: Option<&PlotEnv>,
) -> RunSubmitReport {
    let mut report = empty_submit_report();
    let prepared = match prepare_manifest(root, manifest) {
        Ok(prepared) => prepared,
        Err(message) => return fail_submit(report, message),
    };
    report.manifest_path = Some(prepared.display_path.clone());
    report.manifest_sha256 = Some(prepared.sha256.clone());

    let readiness = match status(&prepared.root) {
        Ok(readiness) => readiness,
        Err(_) => {
            return fail_submit(report, "Nopal readiness could not be evaluated");
        }
    };
    if !readiness.ready {
        return fail_submit(report, "Nopal readiness is not green");
    }

    let config = match nopal_config(&prepared.root) {
        Ok(config) => config,
        Err(_) => {
            return fail_submit(report, "Nopal coordinator configuration could not be read");
        }
    };
    if !config.valid {
        report.diagnostics.extend(
            config
                .diagnostics
                .iter()
                .map(|message| sanitize_diagnostic(message)),
        );
        return fail_submit(report, "Nopal run-start policy configuration is invalid");
    }

    let policy_config = &config.run_start_policy;
    let request = policy::EvalRequest {
        mode: policy_config.mode,
        action: &policy_config.action,
        classes: &policy_config.classes,
        env: &[],
    };
    let policy_report = match policy::run(&prepared.root, policy::View::Decide, &request) {
        Ok(policy_report) => policy_report,
        Err(_) => {
            return fail_submit(report, "Nopal run-start policy could not be evaluated");
        }
    };
    report.decision = policy_report
        .decision
        .map(|decision| decision.as_str().to_owned());
    report.placement = policy_report
        .placement
        .map(|placement| placement.as_str().to_owned());
    if !policy_report.ok {
        return fail_submit(report, "Nopal run-start policy report is invalid");
    }
    let Some(decision) = policy_report.decision else {
        return fail_submit(report, "Nopal run-start policy report has no decision");
    };
    let Some(placement) = policy_report.placement else {
        return fail_submit(report, "Nopal run-start policy report has no placement");
    };
    if decision != Decision::Allow {
        return fail_submit(
            report,
            format!(
                "Nopal run-start policy decision is {}; submission requires allow",
                decision.as_str()
            ),
        );
    }
    if placement == Placement::Blocked {
        return fail_submit(report, "Nopal run-start placement is blocked");
    }

    let rondo_diagnostics = submit_rondo_diagnostics(&config, base_url_override.is_some());
    if !rondo_diagnostics.is_empty() {
        report.diagnostics.extend(
            rondo_diagnostics
                .iter()
                .map(|message| sanitize_diagnostic(message)),
        );
        return fail_submit(report, "Rondo Core configuration is invalid");
    }
    let configured_base_url = base_url_override.or(config.rondo_core.base_url.as_deref());
    let automatic_base_url = if configured_base_url.is_none() && auto_start {
        match start_automatic_rondo_endpoint() {
            Ok(base_url) => Some(base_url),
            Err(message) => return fail_submit(report, message),
        }
    } else {
        None
    };
    let Some(base_url) = configured_base_url.or(automatic_base_url.as_deref()) else {
        return fail_submit(report, missing_rondo_endpoint_diagnostic());
    };
    let repo_id = config
        .rondo_core
        .repo_id
        .clone()
        .unwrap_or_else(|| default_repo_id(&prepared.root));
    if let Err(message) = validate_repo_id(&repo_id) {
        return fail_submit(report, message);
    }
    let timeout = Duration::from_millis(config.rondo_core.request_timeout_ms);
    let client = match factory.connect(base_url, timeout) {
        Ok(client) => client,
        Err(error) => {
            return fail_submit(
                report,
                client_error_diagnostic("Rondo Core client could not be configured", &error),
            );
        }
    };
    let health = match client.health() {
        Ok(health) => health,
        Err(error) => {
            return fail_submit(
                report,
                client_error_diagnostic("Rondo Core health verification failed", &error),
            );
        }
    };
    if !health.ready {
        return fail_submit(report, "Rondo Core is not ready to accept work");
    }
    if health.runtime_version != SUPPORTED_RONDO_RUNTIME_VERSION {
        return fail_submit(
            report,
            format!(
                "Rondo Core runtime version is incompatible; expected {}",
                SUPPORTED_RONDO_RUNTIME_VERSION
            ),
        );
    }
    let response = match client.submit(SubmitRequest::for_plot(
        prepared.canonical_path,
        prepared.sha256.clone(),
        repo_id,
        plot_id,
    )) {
        Ok(response) => response,
        Err(error) => {
            return fail_submit(
                report,
                client_error_diagnostic("Rondo Core submission failed", &error),
            );
        }
    };
    if response.plot_id.as_deref() != Some(plot_id) {
        return fail_submit(
            report,
            "Rondo Core submission returned a missing or mismatched Plot identifier",
        );
    }

    report.submitted = true;
    report.deduplicated = response.deduplicated;
    let handle = SubmittedRunHandle {
        service_id: response.service_id,
        repo_id: response.repo_id,
        plot_id: plot_id.to_owned(),
        run_id: response.run_id,
        status: response.status,
        event_cursor: response.event_cursor,
    };
    report.handle = Some(handle.clone());
    if let Some(plot_env) = plot_env
        && let Err(_error) = plot_store::record_execution_acceptance(
            plot_env,
            plot_id,
            AcceptanceInput {
                service_id: handle.service_id,
                repo_id: handle.repo_id,
                run_id: handle.run_id,
                manifest_sha256: prepared.sha256,
                status: handle.status,
                event_cursor: handle.event_cursor,
            },
        )
    {
        return fail_submit(
            report,
            "Rondo Core accepted the run, but Nopal could not attach it to its durable Plot",
        );
    }
    report.ok = true;
    report
}

fn start_automatic_rondo_endpoint() -> Result<String, String> {
    let paths = StatePaths::from_environment()
        .map_err(|_| "Rondo Core lifecycle state is unavailable".to_owned())?;
    let executable = std::env::current_exe()
        .map_err(|_| "Nopal executable identity is unavailable".to_owned())?;
    let runtime =
        rondo_runtime(&executable).map_err(|error| sanitize_diagnostic(&error.to_string()))?;
    let lifecycle = nopal_rondo_lifecycle::start(&StartOptions::new(paths, executable, runtime))
        .map_err(|_| "Rondo Core lifecycle could not be started".to_owned())?;
    if !lifecycle.ok {
        return Err(lifecycle
            .diagnostics
            .first()
            .map(|message| sanitize_diagnostic(message))
            .unwrap_or_else(|| "Rondo Core lifecycle did not become ready".to_owned()));
    }
    lifecycle
        .base_url
        .ok_or_else(|| "Rondo Core lifecycle did not publish an endpoint".to_owned())
}

pub fn run_observe(
    root: &Path,
    repo_id: &str,
    plot_id: &str,
    run_id: &str,
    cursor: Option<&str>,
    state_dir: Option<&Path>,
) -> RunObservationReport {
    let plot_env = PlotEnv::discover(state_dir);
    let override_url = rondo_core_url_override();
    run_observe_with_factory_mode(
        ObservationRequest {
            root,
            repo_id,
            plot_id,
            run_id,
            cursor,
            base_url_override: override_url.as_deref(),
            use_lifecycle: true,
            plot_env: Some(&plot_env),
        },
        &ProductionRondoClientFactory,
    )
}

#[cfg(test)]
fn run_observe_with_factory<F: RondoClientFactory>(
    root: &Path,
    repo_id: &str,
    run_id: &str,
    cursor: Option<&str>,
    base_url_override: Option<&str>,
    factory: &F,
) -> RunObservationReport {
    run_observe_for_plot_with_factory(
        root,
        repo_id,
        "plot-test",
        run_id,
        cursor,
        base_url_override,
        factory,
    )
}

#[cfg(test)]
fn run_observe_for_plot_with_factory<F: RondoClientFactory>(
    root: &Path,
    repo_id: &str,
    plot_id: &str,
    run_id: &str,
    cursor: Option<&str>,
    base_url_override: Option<&str>,
    factory: &F,
) -> RunObservationReport {
    run_observe_with_factory_mode(
        ObservationRequest {
            root,
            repo_id,
            plot_id,
            run_id,
            cursor,
            base_url_override,
            use_lifecycle: false,
            plot_env: None,
        },
        factory,
    )
}

struct ObservationRequest<'a> {
    root: &'a Path,
    repo_id: &'a str,
    plot_id: &'a str,
    run_id: &'a str,
    cursor: Option<&'a str>,
    base_url_override: Option<&'a str>,
    use_lifecycle: bool,
    plot_env: Option<&'a PlotEnv>,
}

fn run_observe_with_factory_mode<F: RondoClientFactory>(
    request: ObservationRequest<'_>,
    factory: &F,
) -> RunObservationReport {
    let ObservationRequest {
        root,
        repo_id,
        plot_id,
        run_id,
        cursor,
        base_url_override,
        use_lifecycle,
        plot_env,
    } = request;
    let mut report = empty_observation_report("-", "-", "-");
    if let Err(message) = validate_repo_id(repo_id) {
        return fail_observation(report, message);
    }
    report.handle.repo_id = repo_id.to_owned();
    if let Err(message) = validate_plot_id(plot_id) {
        return fail_observation(report, message);
    }
    report.handle.plot_id = plot_id.to_owned();
    if let Err(message) = validate_run_id(run_id) {
        return fail_observation(report, message);
    }
    report.handle.run_id = run_id.to_owned();
    if cursor.is_some_and(|value| value.trim().is_empty()) {
        return fail_observation(report, "Rondo Core event cursor must not be empty");
    }
    let stored_cursor = if let Some(plot_env) = plot_env {
        let plot = match plot_store::load_plot(plot_env, plot_id) {
            Ok(plot) => plot,
            Err(_) => {
                return fail_observation(
                    report,
                    format!("Nopal Plot {plot_id} could not be loaded"),
                );
            }
        };
        let Some(execution) = plot
            .executions
            .iter()
            .find(|execution| execution.repo_id == repo_id && execution.run_id == run_id)
        else {
            return fail_observation(
                report,
                "The requested execution does not belong to the selected Nopal Plot",
            );
        };
        Some(execution.event_cursor.clone())
    } else {
        None
    };
    if let (Some(requested), Some(stored)) = (cursor, stored_cursor.as_deref()) {
        let Some(requested_position) = cursor_position(requested) else {
            return fail_observation(report, "Rondo Core event cursor is malformed");
        };
        let Some(stored_position) = cursor_position(stored) else {
            return fail_observation(report, "The durable Nopal execution cursor is malformed");
        };
        if requested_position > stored_position {
            return fail_observation(
                report,
                "Rondo Core event cursor cannot skip ahead of the durable Nopal execution cursor",
            );
        }
    }
    let requested_cursor = cursor.or(stored_cursor.as_deref());

    let config = match nopal_config(root) {
        Ok(config) => config,
        Err(_) => {
            return fail_observation(report, "Nopal coordinator configuration could not be read");
        }
    };
    let rondo_diagnostics = observe_rondo_diagnostics(&config, base_url_override.is_some());
    if !rondo_diagnostics.is_empty() {
        report.diagnostics.extend(
            rondo_diagnostics
                .iter()
                .map(|message| sanitize_diagnostic(message)),
        );
        return fail_observation(report, "Rondo Core configuration is invalid");
    }
    let configured_base_url = base_url_override.or(config.rondo_core.base_url.as_deref());
    let lifecycle_base_url = if configured_base_url.is_none() && use_lifecycle {
        match running_rondo_endpoint() {
            Ok(base_url) => Some(base_url),
            Err(message) => return fail_observation(report, message),
        }
    } else {
        None
    };
    let Some(base_url) = configured_base_url.or(lifecycle_base_url.as_deref()) else {
        return fail_observation(report, missing_rondo_endpoint_diagnostic());
    };
    let timeout = Duration::from_millis(config.rondo_core.request_timeout_ms);
    let client = match factory.connect(base_url, timeout) {
        Ok(client) => client,
        Err(error) => {
            return fail_observation(
                report,
                client_error_diagnostic("Rondo Core client could not be configured", &error),
            );
        }
    };
    let handle = RunHandle::for_plot(repo_id, run_id, plot_id);
    let status = match client.status(handle.clone()) {
        Ok(status) => status,
        Err(error) => {
            return fail_observation(
                report,
                client_error_diagnostic("Rondo Core status observation failed", &error),
            );
        }
    };
    if status.plot_id.as_deref() != Some(plot_id) {
        return fail_observation(
            report,
            "Rondo Core status returned a missing or mismatched Plot identifier",
        );
    }
    let observed_status = status.status.clone();
    let mut durable_evidence = Vec::new();
    for pointer in &status.evidence_pointers {
        let pointer = ExecutionEvidencePointer {
            artifact_kind: pointer.artifact_kind.clone(),
            uri: pointer.uri.clone(),
        };
        if !durable_evidence.contains(&pointer) {
            durable_evidence.push(pointer);
        }
    }
    report.status = Some(status.status);
    report.last_event = status.last_event.as_ref().map(sanitize_json);
    report.evidence_pointers = durable_evidence
        .iter()
        .map(|pointer| {
            serde_json::json!({
                "artifact_kind": pointer.artifact_kind,
                "uri": pointer.uri,
            })
        })
        .collect();
    report.event_cursor = Some(status.event_cursor);
    if let (Some(plot_env), Some(durable_cursor)) = (plot_env, stored_cursor.as_deref())
        && let Err(_error) = plot_store::record_execution_observation(
            plot_env,
            plot_id,
            ObservationInput {
                repo_id: repo_id.to_owned(),
                run_id: run_id.to_owned(),
                status: observed_status.clone(),
                event_cursor: durable_cursor.to_owned(),
                evidence: durable_evidence.clone(),
            },
        )
    {
        return fail_observation(
            report,
            "Nopal observed the run status, but could not update its durable Plot",
        );
    }

    let events = match client.events(handle, requested_cursor) {
        Ok(events) => events,
        Err(error) => {
            return fail_observation(
                report,
                client_error_diagnostic("Rondo Core event observation failed", &error),
            );
        }
    };
    if events.plot_id.as_deref() != Some(plot_id) {
        return fail_observation(
            report,
            "Rondo Core events returned a missing or mismatched Plot identifier",
        );
    }
    for pointer in events.evidence_pointers() {
        let pointer = ExecutionEvidencePointer {
            artifact_kind: pointer.artifact_kind,
            uri: pointer.uri,
        };
        if !durable_evidence.contains(&pointer) {
            report.evidence_pointers.push(serde_json::json!({
                "artifact_kind": pointer.artifact_kind.clone(),
                "uri": pointer.uri.clone(),
            }));
            durable_evidence.push(pointer);
        }
    }
    report.events = events.events.iter().map(sanitize_json).collect();
    let next_event_cursor = events.next_event_cursor.clone();
    report.next_event_cursor = Some(events.next_event_cursor);
    report.has_more = events.has_more;
    report.settled = report.status.as_deref().is_some_and(is_terminal_status) && !report.has_more;
    if let Some(plot_env) = plot_env
        && let Err(_error) = plot_store::record_execution_observation(
            plot_env,
            plot_id,
            ObservationInput {
                repo_id: repo_id.to_owned(),
                run_id: run_id.to_owned(),
                status: observed_status,
                event_cursor: next_event_cursor,
                evidence: durable_evidence,
            },
        )
    {
        return fail_observation(
            report,
            "Nopal observed the run, but could not update its durable Plot",
        );
    }
    report.ok = true;
    report
}

fn running_rondo_endpoint() -> Result<String, String> {
    let paths = StatePaths::from_environment()
        .map_err(|_| "Rondo Core lifecycle state is unavailable".to_owned())?;
    let lifecycle = nopal_rondo_lifecycle::health(&paths);
    if !lifecycle.ok {
        return Err(lifecycle
            .diagnostics
            .first()
            .map(|message| sanitize_diagnostic(message))
            .unwrap_or_else(|| "Rondo Core lifecycle is not ready".to_owned()));
    }
    lifecycle
        .base_url
        .ok_or_else(|| "Rondo Core lifecycle did not publish an endpoint".to_owned())
}

struct PreparedManifest {
    root: PathBuf,
    canonical_path: String,
    display_path: String,
    sha256: String,
}

fn prepare_manifest(root: &Path, manifest: &Path) -> Result<PreparedManifest, &'static str> {
    let root = fs::canonicalize(root).map_err(|_| "Nopal repository root could not be resolved")?;
    let candidate = if manifest.is_absolute() {
        manifest.to_path_buf()
    } else {
        root.join(manifest)
    };
    let metadata = fs::symlink_metadata(&candidate)
        .map_err(|_| "Manifest must be an accessible regular file inside the repository")?;
    if metadata.file_type().is_symlink() {
        return Err("Manifest must not be a symbolic link");
    }
    if !metadata.is_file() {
        return Err("Manifest must be a regular file");
    }
    let canonical =
        fs::canonicalize(&candidate).map_err(|_| "Manifest could not be resolved safely")?;
    let relative = canonical
        .strip_prefix(&root)
        .map_err(|_| "Manifest must stay inside the discovered repository")?;
    if relative.as_os_str().is_empty() {
        return Err("Manifest must be a regular file below the repository root");
    }
    let display_path = relative
        .to_str()
        .ok_or("Manifest display path must be valid UTF-8")?
        .to_owned();
    let canonical_path = canonical
        .to_str()
        .ok_or("Manifest path must be valid UTF-8")?
        .to_owned();
    let bytes = fs::read(&canonical).map_err(|_| "Manifest could not be read")?;
    let sha256 = format!("{:x}", Sha256::digest(bytes));
    Ok(PreparedManifest {
        root,
        canonical_path,
        display_path,
        sha256,
    })
}

fn empty_submit_report() -> RunSubmitReport {
    RunSubmitReport {
        kind: RUN_SUBMIT_KIND,
        ok: false,
        submitted: false,
        deduplicated: false,
        manifest_path: None,
        manifest_sha256: None,
        decision: None,
        placement: None,
        handle: None,
        diagnostics: Vec::new(),
    }
}

fn fail_submit(mut report: RunSubmitReport, message: impl Into<String>) -> RunSubmitReport {
    report
        .diagnostics
        .push(sanitize_diagnostic(&message.into()));
    report
}

fn empty_observation_report(repo_id: &str, plot_id: &str, run_id: &str) -> RunObservationReport {
    RunObservationReport {
        kind: RUN_OBSERVATION_KIND,
        ok: false,
        handle: ObservationHandle {
            repo_id: repo_id.to_owned(),
            plot_id: plot_id.to_owned(),
            run_id: run_id.to_owned(),
        },
        status: None,
        last_event: None,
        evidence_pointers: Vec::new(),
        event_cursor: None,
        events: Vec::new(),
        next_event_cursor: None,
        has_more: false,
        settled: false,
        diagnostics: Vec::new(),
    }
}

fn fail_observation(
    mut report: RunObservationReport,
    message: impl Into<String>,
) -> RunObservationReport {
    report
        .diagnostics
        .push(sanitize_diagnostic(&message.into()));
    report
}

fn sanitize_diagnostic(message: &str) -> String {
    nopal_core::run_ledger::redact_text(message, nopal_core::run_ledger::HINT_LIMIT)
}

fn sanitize_json(value: &JsonValue) -> JsonValue {
    let ledger_value = nopal_ledger_json::Value::from(value.clone());
    let redacted = nopal_core::run_ledger::redact_json(&ledger_value);
    serde_json::to_value(redacted).unwrap_or(JsonValue::Null)
}

fn validate_run_id(run_id: &str) -> Result<(), &'static str> {
    if run_id.trim().is_empty() {
        return Err("Rondo Core run identifier is required");
    }
    if run_id.trim() != run_id {
        return Err("Rondo Core run identifier must not have surrounding whitespace");
    }
    if run_id.chars().any(char::is_control) {
        return Err("Rondo Core run identifier must not contain control characters");
    }
    Ok(())
}

fn validate_plot_id(plot_id: &str) -> Result<(), &'static str> {
    if plot_id.trim().is_empty() {
        return Err("Nopal Plot identifier is required");
    }
    if plot_id.trim() != plot_id {
        return Err("Nopal Plot identifier must not have surrounding whitespace");
    }
    if plot_id.len() > 512 {
        return Err("Nopal Plot identifier must not exceed 512 UTF-8 bytes");
    }
    if plot_id.chars().any(char::is_control) {
        return Err("Nopal Plot identifier must not contain control characters");
    }
    Ok(())
}

fn validate_established_plot(plot_id: &str, state_dir: Option<&Path>) -> Result<(), String> {
    validate_plot_id(plot_id).map_err(str::to_owned)?;
    let env = nopal_core::plot_store::PlotEnv::discover(state_dir);
    let plot = nopal_core::plot_store::load_plot(&env, plot_id)
        .map_err(|_| format!("Nopal Plot {plot_id} could not be loaded"))?;
    if plot.provisional || plot.establishment.is_none() {
        return Err(format!(
            "Nopal Plot {plot_id} must be established before submitting work"
        ));
    }
    Ok(())
}

fn is_terminal_status(status: &str) -> bool {
    matches!(status, "completed" | "failed" | "terminated" | "paused")
}

fn rondo_core_url_override() -> Option<String> {
    std::env::var_os(RONDO_CORE_URL_ENV).map(|value| {
        value
            .into_string()
            .unwrap_or_else(|_| "\0invalid-non-utf8-rondo-core-url".to_owned())
    })
}

fn missing_rondo_endpoint_diagnostic() -> String {
    format!(
        "Rondo Core endpoint is not configured; set rondo_core.base_url or {RONDO_CORE_URL_ENV}"
    )
}

fn default_repo_id(root: &Path) -> String {
    let digest = Sha256::digest(root.as_os_str().as_encoded_bytes());
    format!("nopal.repo/v1:{digest:x}")
}

fn validate_repo_id(repo_id: &str) -> Result<(), &'static str> {
    if repo_id.trim().is_empty() {
        return Err("Rondo Core repository identifier is required");
    }
    if repo_id.trim() != repo_id {
        return Err("Rondo Core repository identifier must not have surrounding whitespace");
    }
    if repo_id.len() > 512 {
        return Err("Rondo Core repository identifier must not exceed 512 UTF-8 bytes");
    }
    if repo_id.chars().any(char::is_control) {
        return Err("Rondo Core repository identifier must not contain control characters");
    }
    Ok(())
}

fn client_error_diagnostic(context: &str, error: &ClientError) -> String {
    format!("{context}: {error}")
}

fn submit_rondo_diagnostics(config: &NopalConfig, has_endpoint_override: bool) -> Vec<String> {
    let mut diagnostics = common_rondo_diagnostics(config, has_endpoint_override);
    diagnostics.extend(config.rondo_repo_id_diagnostics.iter().cloned());
    diagnostics
}

fn observe_rondo_diagnostics(config: &NopalConfig, has_endpoint_override: bool) -> Vec<String> {
    common_rondo_diagnostics(config, has_endpoint_override)
}

fn common_rondo_diagnostics(config: &NopalConfig, has_endpoint_override: bool) -> Vec<String> {
    let mut diagnostics = config.rondo_diagnostics.clone();
    diagnostics.extend(config.rondo_timeout_diagnostics.iter().cloned());
    if !has_endpoint_override {
        diagnostics.extend(config.rondo_base_url_diagnostics.iter().cloned());
    }
    diagnostics
}

pub fn status_toon(report: &NopalStatus) -> String {
    toon::encode(&[
        ("kind".into(), Value::str(report.kind)),
        ("project".into(), opt_str(&report.project)),
        ("profile".into(), opt_str(&report.profile)),
        ("ready".into(), Value::Bool(report.ready)),
        (
            "modules".into(),
            Value::Table {
                fields: vec!["name".into(), "required".into(), "state".into()],
                rows: report
                    .modules
                    .iter()
                    .map(|module| {
                        vec![
                            Value::str(&module.name),
                            Value::Bool(module.required),
                            Value::str(&module.state),
                        ]
                    })
                    .collect(),
            },
        ),
        (
            "missing_modules".into(),
            Value::Arr(report.missing_modules.iter().map(Value::str).collect()),
        ),
        (
            "diagnostics".into(),
            diagnostics_count(report.diagnostics.len()),
        ),
        (
            "help".into(),
            Value::Arr(report.help.iter().map(Value::str).collect()),
        ),
    ])
}

pub fn placement_toon(report: &PlacementReport) -> String {
    toon::encode(&[
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
        ("mode".into(), Value::str(&report.mode)),
        ("action".into(), Value::str(&report.action)),
        (
            "classes".into(),
            Value::Arr(report.classes.iter().map(Value::str).collect()),
        ),
        ("placement".into(), opt_str(&report.placement)),
        ("placement_source".into(), opt_str(&report.placement_source)),
        (
            "matched_rules".into(),
            Value::Int(report.matched_rules as i64),
        ),
        (
            "explanation".into(),
            Value::Arr(report.explanation.iter().map(Value::str).collect()),
        ),
    ])
}

pub fn rondo_service_toon(report: &RondoServiceReport) -> String {
    toon::encode(&[
        ("kind".into(), Value::str(&report.kind)),
        ("service_id".into(), Value::str(&report.service_id)),
        ("status".into(), Value::str(&report.status)),
        ("ok".into(), Value::Bool(report.ok)),
        ("health".into(), Value::str(&report.health)),
        ("placement".into(), Value::str(&report.placement)),
        ("state_path".into(), Value::str(&report.state_path)),
        ("log_path".into(), Value::str(&report.log_path)),
        ("base_url".into(), opt_str(&report.base_url)),
        ("runtime_version".into(), opt_str(&report.runtime_version)),
        ("instance_id".into(), opt_str(&report.instance_id)),
        (
            "active_run_count".into(),
            Value::Int(
                report
                    .active_run_count
                    .map_or(-1, |count| i64::try_from(count).unwrap_or(i64::MAX)),
            ),
        ),
        (
            "diagnostics".into(),
            Value::Arr(report.diagnostics.iter().map(Value::str).collect()),
        ),
    ])
}

pub fn run_start_dry_run_toon(report: &RunStartDryRunReport) -> String {
    toon::encode(&[
        ("kind".into(), Value::str(report.kind)),
        ("dry_run".into(), Value::Bool(report.dry_run)),
        ("would_submit".into(), Value::Bool(report.would_submit)),
        (
            "readiness_ready".into(),
            Value::Bool(report.readiness_ready),
        ),
        ("placement".into(), opt_str(&report.placement)),
        ("placement_source".into(), opt_str(&report.placement_source)),
        ("rondo_status".into(), Value::str(&report.rondo_status)),
        ("rondo_log_path".into(), Value::str(&report.rondo_log_path)),
        (
            "blockers".into(),
            Value::Arr(report.blockers.iter().map(Value::str).collect()),
        ),
        (
            "next_steps".into(),
            Value::Arr(report.next_steps.iter().map(Value::str).collect()),
        ),
        (
            "diagnostics".into(),
            Value::Arr(report.diagnostics.iter().map(Value::str).collect()),
        ),
    ])
}

pub fn run_submit_toon(report: &RunSubmitReport) -> String {
    encode_report_toon(&run_submit_toon_value(report))
}

pub fn run_observation_toon(report: &RunObservationReport) -> String {
    encode_report_toon(&run_observation_toon_value(report))
}

fn run_submit_toon_value(report: &RunSubmitReport) -> Value {
    report_toon_value(report)
}

fn run_observation_toon_value(report: &RunObservationReport) -> Value {
    report_toon_value(report)
}

fn report_toon_value(report: &impl Serialize) -> Value {
    serde_json::to_value(report)
        .map(|value| toon::from_json(&value))
        .unwrap_or(Value::Null)
}

fn encode_report_toon(value: &Value) -> String {
    match value {
        Value::Obj(entries) => toon::encode(entries),
        other => toon::encode(&[("value".to_owned(), other.clone())]),
    }
}

fn nopal_config(root: &Path) -> io::Result<NopalConfig> {
    let path = root.join(CONFIG_PATH);
    let default = default_nopal_config();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(default),
        Err(err) => return Err(err),
    };
    let value = match nopal_core::config::parse_jsonc(&text, CONFIG_PATH, Code::ModuleParseError) {
        Ok(value) => value,
        Err(diagnostic) => {
            return Ok(invalid_config(format!(
                "nopal config parse error: {}",
                diagnostic.message
            )));
        }
    };
    Ok(parse_nopal_config(&value, CONFIG_PATH))
}

fn parse_nopal_config(value: &serde_json::Value, path: &str) -> NopalConfig {
    let mut config = default_nopal_config();
    let Some(object) = value.as_object() else {
        return invalid_config(format!("nopal config {path} must be a JSON object"));
    };
    match object.get("version").and_then(serde_json::Value::as_str) {
        Some(CONFIG_KIND) => {}
        Some(_) => {
            let message =
                format!("nopal config {path} has an unsupported version; expected {CONFIG_KIND:?}");
            config.diagnostics.push(message.clone());
            config.rondo_diagnostics.push(message);
            config.valid = false;
        }
        None => {
            let message = format!("nopal config {path} is missing version {CONFIG_KIND:?}");
            config.diagnostics.push(message.clone());
            config.rondo_diagnostics.push(message);
            config.valid = false;
        }
    }

    if let Some(policy_value) = object.get("run_start_policy") {
        let Some(policy_object) = policy_value.as_object() else {
            config.diagnostics.push(format!(
                "nopal config {path} field run_start_policy must be an object"
            ));
            config.valid = false;
            return parse_rondo_core_config(config, object, path);
        };
        let mut diagnostics = Vec::new();
        let mode = required_token(policy_object, "mode", path, &mut diagnostics).map(Mode::new);
        let action =
            required_token(policy_object, "action", path, &mut diagnostics).map(str::to_owned);
        let classes = parse_classes(policy_object.get("classes"), path, &mut diagnostics);
        if let (Some(mode), Some(action), true) = (mode, action, diagnostics.is_empty()) {
            config.run_start_policy = RunStartPolicyConfig {
                mode,
                action,
                classes,
            };
        } else {
            config.valid = false;
        }
        config.diagnostics.extend(diagnostics);
    }

    parse_rondo_core_config(config, object, path)
}

fn parse_rondo_core_config(
    mut config: NopalConfig,
    object: &serde_json::Map<String, serde_json::Value>,
    path: &str,
) -> NopalConfig {
    let Some(value) = object.get("rondo_core") else {
        return config;
    };
    let Some(rondo) = value.as_object() else {
        config.rondo_diagnostics.push(format!(
            "nopal config {path} field rondo_core must be an object"
        ));
        return config;
    };

    if let Some(base_url) = rondo.get("base_url") {
        match base_url.as_str() {
            Some(value) if !value.is_empty() => {
                config.rondo_core.base_url = Some(value.to_owned());
            }
            _ => {
                config.rondo_base_url_diagnostics.push(format!(
                    "nopal config {path} field rondo_core.base_url must be a nonempty string"
                ));
            }
        }
    }

    if let Some(timeout) = rondo.get("request_timeout_ms") {
        match timeout.as_u64() {
            Some(value) if value > 0 => config.rondo_core.request_timeout_ms = value,
            _ => {
                config.rondo_timeout_diagnostics.push(format!(
                    "nopal config {path} field rondo_core.request_timeout_ms must be a positive integer"
                ));
            }
        }
    }

    if let Some(repo_id) = rondo.get("repo_id") {
        match repo_id.as_str() {
            Some(value) => match validate_repo_id(value) {
                Ok(()) => config.rondo_core.repo_id = Some(value.to_owned()),
                Err(message) => {
                    config.rondo_repo_id_diagnostics.push(message.to_owned());
                }
            },
            None => {
                config.rondo_repo_id_diagnostics.push(format!(
                    "nopal config {path} field rondo_core.repo_id must be a string"
                ));
            }
        }
    }
    config
}

fn default_nopal_config() -> NopalConfig {
    NopalConfig {
        run_start_policy: default_run_start_policy(),
        diagnostics: Vec::new(),
        valid: true,
        rondo_core: RondoCoreConfig {
            base_url: None,
            request_timeout_ms: DEFAULT_RONDO_REQUEST_TIMEOUT_MS,
            repo_id: None,
        },
        rondo_diagnostics: Vec::new(),
        rondo_base_url_diagnostics: Vec::new(),
        rondo_timeout_diagnostics: Vec::new(),
        rondo_repo_id_diagnostics: Vec::new(),
    }
}

fn default_run_start_policy() -> RunStartPolicyConfig {
    RunStartPolicyConfig {
        mode: Mode::new(DEFAULT_RUN_START_MODE),
        action: DEFAULT_RUN_START_ACTION.to_owned(),
        classes: vec![ActionClass::WorkspaceWrite],
    }
}

fn required_token<'a>(
    object: &'a serde_json::Map<String, serde_json::Value>,
    key: &str,
    path: &str,
    diagnostics: &mut Vec<String>,
) -> Option<&'a str> {
    match object.get(key).and_then(serde_json::Value::as_str) {
        Some(value) if !value.is_empty() => Some(value),
        Some(_) => {
            diagnostics.push(format!("nopal config {path} field {key} must not be empty"));
            None
        }
        None => {
            diagnostics.push(format!("nopal config {path} field {key} must be a string"));
            None
        }
    }
}

fn parse_classes(
    value: Option<&serde_json::Value>,
    path: &str,
    diagnostics: &mut Vec<String>,
) -> Vec<ActionClass> {
    let Some(values) = value.and_then(serde_json::Value::as_array) else {
        diagnostics.push(format!(
            "nopal config {path} field classes must be an array of strings"
        ));
        return Vec::new();
    };
    let mut classes = Vec::new();
    for item in values {
        match item.as_str() {
            Some(value) if !value.is_empty() => classes.push(ActionClass::new(value)),
            Some(_) => diagnostics.push(format!(
                "nopal config {path} field classes must not contain empty strings"
            )),
            None => diagnostics.push(format!(
                "nopal config {path} field classes must contain only strings"
            )),
        }
    }
    if classes.is_empty() {
        diagnostics.push(format!(
            "nopal config {path} field classes must contain at least one class"
        ));
    }
    classes
}

fn invalid_config(message: String) -> NopalConfig {
    invalid_config_with(vec![message])
}

fn invalid_config_with(diagnostics: Vec<String>) -> NopalConfig {
    NopalConfig {
        run_start_policy: default_run_start_policy(),
        rondo_diagnostics: diagnostics.clone(),
        diagnostics,
        valid: false,
        rondo_core: RondoCoreConfig {
            base_url: None,
            request_timeout_ms: DEFAULT_RONDO_REQUEST_TIMEOUT_MS,
            repo_id: None,
        },
        rondo_base_url_diagnostics: Vec::new(),
        rondo_timeout_diagnostics: Vec::new(),
        rondo_repo_id_diagnostics: Vec::new(),
    }
}

fn blocked_placement_report(diagnostics: &[String]) -> PlacementReport {
    PlacementReport {
        kind: PLACEMENT_KIND,
        ok: false,
        mode: DEFAULT_RUN_START_MODE.to_owned(),
        action: DEFAULT_RUN_START_ACTION.to_owned(),
        classes: vec![ActionClass::WorkspaceWrite.as_str().to_owned()],
        placement: Some(Placement::Blocked.as_str().to_owned()),
        placement_source: Some("nopal_config_invalid".to_owned()),
        matched_rules: 0,
        diagnostics: Vec::new(),
        explanation: diagnostics.to_vec(),
    }
}

fn effective_run_placement(
    root: &Path,
    requested_placement: Option<Placement>,
    config: &NopalConfig,
) -> io::Result<Option<Placement>> {
    if !config.valid {
        return Ok(None);
    }
    let policy = &config.run_start_policy;
    let policy_report = placement(root, policy.mode, &policy.action, &policy.classes)?;
    if !policy_report.ok {
        return Ok(None);
    }
    let Some(policy_placement) = policy_report
        .placement
        .as_deref()
        .and_then(Placement::parse)
    else {
        return Ok(None);
    };
    let effective = requested_placement.map_or(policy_placement, |requested| {
        std::cmp::max(requested, policy_placement)
    });
    if effective == Placement::Blocked {
        Ok(None)
    } else {
        Ok(Some(effective))
    }
}

fn blocked_report(paths: &StatePaths, reason: &str, diagnostics: &[String]) -> RondoServiceReport {
    let mut report_diagnostics = vec![format!(
        "{reason}; Nopal must not start or submit to Rondo Core"
    )];
    report_diagnostics.extend(diagnostics.iter().cloned());
    RondoServiceReport {
        kind: RONDO_SERVICE_KIND.to_owned(),
        service_id: "local-rondo-core".to_owned(),
        status: "blocked".to_owned(),
        ok: false,
        health: "blocked".to_owned(),
        placement: Placement::Blocked.as_str().to_owned(),
        state_path: paths.descriptor().display().to_string(),
        log_path: paths.log().display().to_string(),
        base_url: None,
        runtime_version: None,
        instance_id: None,
        active_run_count: None,
        diagnostics: report_diagnostics,
    }
}

fn lifecycle_report(report: LifecycleReport, placement: &str) -> RondoServiceReport {
    RondoServiceReport {
        kind: RONDO_SERVICE_KIND.to_owned(),
        service_id: "rondo-core".to_owned(),
        status: report.status,
        ok: report.ok,
        health: report.health,
        placement: placement.to_owned(),
        state_path: report.state_path,
        log_path: report.log_path,
        base_url: report.base_url,
        runtime_version: report.runtime_version,
        instance_id: report.instance_id,
        active_run_count: report.active_run_count,
        diagnostics: report.diagnostics,
    }
}

fn rondo_runtime(nopal_executable: &Path) -> io::Result<PathBuf> {
    if let Some(path) = std::env::var_os("NOPAL_RONDO_RUNTIME") {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "NOPAL_RONDO_RUNTIME does not name a regular Rondo executable",
        ));
    }

    let sibling = nopal_executable
        .parent()
        .map(|parent| parent.join("rondo"))
        .ok_or_else(|| io::Error::other("Nopal executable has no parent directory"))?;
    if sibling.is_file() {
        Ok(sibling)
    } else {
        Err(io::Error::new(
            io::ErrorKind::NotFound,
            "Rondo runtime is unavailable; set NOPAL_RONDO_RUNTIME during development",
        ))
    }
}

fn opt_str(value: &Option<String>) -> Value {
    Value::str(value.as_deref().unwrap_or("-"))
}

fn diagnostics_count(count: usize) -> Value {
    Value::Int(count as i64)
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use std::sync::{Arc, Mutex};

    use nopal_rondo_client::{
        EvidencePointer, RunEventsResponse, RunHandle, RunStatusResponse, SubmitRequest,
        SubmitResponse,
    };
    use serde_json::json;

    use super::*;

    #[derive(Default)]
    struct Calls {
        connect: usize,
        health: usize,
        submit: usize,
        status: usize,
        events: usize,
        base_url: Option<String>,
        timeout: Option<std::time::Duration>,
        submit_request: Option<SubmitRequest>,
        event_cursor: Option<Option<String>>,
    }

    #[derive(Clone)]
    struct FakeClient {
        calls: Arc<Mutex<Calls>>,
        health_response: Result<HealthResponse, nopal_rondo_client::ClientError>,
        submit_response: Result<SubmitResponse, nopal_rondo_client::ClientError>,
        status_response: Result<RunStatusResponse, nopal_rondo_client::ClientError>,
        events_response: Result<RunEventsResponse, nopal_rondo_client::ClientError>,
    }

    struct FakeFactory {
        client: FakeClient,
    }

    impl RondoClientAdapter for FakeClient {
        fn health(&self) -> Result<HealthResponse, nopal_rondo_client::ClientError> {
            self.calls.lock().expect("calls lock poisoned").health += 1;
            self.health_response.clone()
        }

        fn submit(
            &self,
            request: SubmitRequest,
        ) -> Result<SubmitResponse, nopal_rondo_client::ClientError> {
            let mut calls = self.calls.lock().expect("calls lock poisoned");
            calls.submit += 1;
            calls.submit_request = Some(request);
            self.submit_response.clone()
        }

        fn status(
            &self,
            _handle: RunHandle,
        ) -> Result<RunStatusResponse, nopal_rondo_client::ClientError> {
            self.calls.lock().expect("calls lock poisoned").status += 1;
            self.status_response.clone()
        }

        fn events(
            &self,
            _handle: RunHandle,
            cursor: Option<&str>,
        ) -> Result<RunEventsResponse, nopal_rondo_client::ClientError> {
            let mut calls = self.calls.lock().expect("calls lock poisoned");
            calls.events += 1;
            calls.event_cursor = Some(cursor.map(str::to_owned));
            self.events_response.clone()
        }
    }

    impl RondoClientFactory for FakeFactory {
        type Client = FakeClient;

        fn connect(
            &self,
            base_url: &str,
            timeout: std::time::Duration,
        ) -> Result<Self::Client, nopal_rondo_client::ClientError> {
            let mut calls = self.client.calls.lock().expect("calls lock poisoned");
            calls.connect += 1;
            calls.base_url = Some(base_url.to_owned());
            calls.timeout = Some(timeout);
            drop(calls);
            Ok(self.client.clone())
        }
    }

    fn factory() -> (FakeFactory, Arc<Mutex<Calls>>) {
        let calls = Arc::new(Mutex::new(Calls::default()));
        let client = FakeClient {
            calls: calls.clone(),
            health_response: Ok(HealthResponse {
                surface: "rondo.core/v1".to_owned(),
                runtime_version: SUPPORTED_RONDO_RUNTIME_VERSION.to_owned(),
                instance_id: "019b8941-4a0c-7ad5-b7ef-cb3c45e4a819".to_owned(),
                service_mode: "trackerless_core".to_owned(),
                ready: true,
                active_run_count: 0,
            }),
            submit_response: Ok(SubmitResponse {
                surface: "rondo.core/v1".to_owned(),
                service_id: "rondo-core".to_owned(),
                repo_id: "configured-repo".to_owned(),
                plot_id: Some("plot-test".to_owned()),
                run_id: "run-1".to_owned(),
                status: "running".to_owned(),
                event_cursor: "rondo.core/v1:0".to_owned(),
                deduplicated: true,
            }),
            status_response: Ok(RunStatusResponse {
                run_id: "run-1".to_owned(),
                plot_id: Some("plot-test".to_owned()),
                status: "completed".to_owned(),
                last_event: Some(json!({"type": "run.completed"})),
                evidence_pointers: vec![EvidencePointer {
                    artifact_kind: "report".to_owned(),
                    uri: "rondo://opaque/evidence".to_owned(),
                }],
                event_cursor: "rondo.core/v1:9".to_owned(),
            }),
            events_response: Ok(RunEventsResponse {
                plot_id: Some("plot-test".to_owned()),
                events: vec![json!({"type": "run.completed"})],
                next_event_cursor: "rondo.core/v1:9".to_owned(),
                has_more: false,
            }),
        };
        (FakeFactory { client }, calls)
    }

    fn write_project(root: &Path, decision: &str, placement: &str) {
        fs::create_dir_all(root.join(".nopal")).expect("create .nopal");
        fs::write(
            root.join(".nopal/nopal.jsonc"),
            r#"{
  "version": "nopal.project/v1",
  "project": { "name": "afk-fixture" },
  "profile": "portable"
}
"#,
        )
        .expect("write manifest");
        fs::write(
            root.join(".nopal/gates.jsonc"),
            "{ \"version\": \"nopal.gates/v1\", \"gates\": [] }\n",
        )
        .expect("write gates");
        fs::write(
            root.join(".nopal/policy.jsonc"),
            format!(
                r#"{{
  "version": "nopal.policy/v1",
  "modes": {{
    "nopal_tui": {{
      "default_decision": "{decision}",
      "default_placement": "{placement}",
      "rules": []
    }}
  }}
}}
"#
            ),
        )
        .expect("write policy");
    }

    fn write_rondo_config(root: &Path, repo_id: Option<&str>) {
        let repo_id = repo_id.map_or(String::new(), |value| {
            format!(",\n    \"repo_id\": {value:?}")
        });
        fs::write(
            root.join(".nopal/config.jsonc"),
            format!(
                r#"{{
  "version": "nopal.config/v1",
  "rondo_core": {{
    "base_url": "http://127.0.0.1:4400",
    "request_timeout_ms": 250{repo_id}
  }}
}}
"#
            ),
        )
        .expect("write config");
    }

    #[test]
    fn submit_blocks_before_connect_and_calls_submit_once_when_allowed() {
        let temp = tempfile::tempdir().expect("create tempdir");
        write_project(temp.path(), "ask", "dedicated_run_runtime");
        write_rondo_config(temp.path(), Some("configured-repo"));
        let manifest = temp.path().join("slice.json");
        fs::write(&manifest, b"exact manifest bytes\n").expect("write slice");
        let (factory, calls) = factory();

        let blocked = run_submit_with_factory(temp.path(), &manifest, None, &factory);
        assert!(!blocked.ok);
        assert_eq!(blocked.decision.as_deref(), Some("ask"));
        assert_eq!(calls.lock().expect("calls lock poisoned").connect, 0);

        write_project(temp.path(), "allow", "dedicated_run_runtime");
        let accepted = run_submit_with_factory(temp.path(), &manifest, None, &factory);
        assert!(accepted.ok);
        assert!(accepted.submitted);
        assert!(accepted.deduplicated);
        assert_eq!(accepted.manifest_path.as_deref(), Some("slice.json"));
        assert_eq!(accepted.decision.as_deref(), Some("allow"));
        assert_eq!(accepted.placement.as_deref(), Some("dedicated_run_runtime"));
        assert_eq!(
            accepted
                .handle
                .as_ref()
                .map(|handle| handle.plot_id.as_str()),
            Some("plot-test")
        );
        let calls = calls.lock().expect("calls lock poisoned");
        assert_eq!(calls.connect, 1);
        assert_eq!(calls.health, 1);
        assert_eq!(calls.submit, 1);
        assert_eq!(calls.base_url.as_deref(), Some("http://127.0.0.1:4400"));
        assert_eq!(calls.timeout, Some(std::time::Duration::from_millis(250)));
        let request = calls.submit_request.as_ref().expect("submit request");
        assert_eq!(request.repo_id, "configured-repo");
        assert_eq!(request.plot_id.as_deref(), Some("plot-test"));
        assert_eq!(
            request.manifest_sha256,
            format!("{:x}", Sha256::digest(b"exact manifest bytes\n"))
        );
        assert_eq!(
            Some(request.manifest_sha256.as_str()),
            accepted.manifest_sha256.as_deref()
        );
    }

    #[test]
    fn managed_submit_and_observation_fail_closed_on_foreign_plot_echoes() {
        let temp = tempfile::tempdir().expect("create tempdir");
        write_project(temp.path(), "allow", "dedicated_run_runtime");
        write_rondo_config(temp.path(), Some("configured-repo"));
        let manifest = temp.path().join("slice.json");
        fs::write(&manifest, b"exact manifest bytes\n").expect("write slice");
        let (mut submit_factory, _) = factory();

        submit_factory
            .client
            .submit_response
            .as_mut()
            .expect("submit response")
            .plot_id = Some("plot-foreign".to_owned());
        let submitted = run_submit_for_plot_with_factory(
            temp.path(),
            &manifest,
            "plot-test",
            None,
            &submit_factory,
        );
        assert!(!submitted.ok);
        assert!(submitted.handle.is_none());
        assert!(submitted.diagnostics[0].contains("mismatched Plot"));

        let (mut status_factory, _) = factory();
        status_factory
            .client
            .status_response
            .as_mut()
            .expect("status response")
            .plot_id = None;
        let observed = run_observe_for_plot_with_factory(
            temp.path(),
            "configured-repo",
            "plot-test",
            "run-1",
            None,
            None,
            &status_factory,
        );
        assert!(!observed.ok);
        assert!(observed.diagnostics[0].contains("mismatched Plot"));

        let (mut events_factory, _) = factory();
        events_factory
            .client
            .events_response
            .as_mut()
            .expect("events response")
            .plot_id = Some("plot-foreign".to_owned());
        let observed = run_observe_for_plot_with_factory(
            temp.path(),
            "configured-repo",
            "plot-test",
            "run-1",
            None,
            None,
            &events_factory,
        );
        assert!(!observed.ok);
        assert!(observed.diagnostics[0].contains("mismatched Plot"));
    }

    #[test]
    fn submit_never_connects_for_readiness_or_policy_or_placement_blocks() {
        #[derive(Clone, Copy)]
        enum Block {
            Readiness,
            InvalidPolicy,
            Decision(&'static str),
            Placement,
        }

        for block in [
            Block::Readiness,
            Block::InvalidPolicy,
            Block::Decision("ask"),
            Block::Decision("deny"),
            Block::Placement,
        ] {
            let temp = tempfile::tempdir().expect("create tempdir");
            let (decision, placement) = match block {
                Block::Decision(decision) => (decision, "dedicated_run_runtime"),
                Block::Placement => ("allow", "blocked"),
                Block::Readiness | Block::InvalidPolicy => ("allow", "dedicated_run_runtime"),
            };
            write_project(temp.path(), decision, placement);
            write_rondo_config(temp.path(), Some("configured-repo"));
            match block {
                Block::Readiness => {
                    fs::remove_file(temp.path().join(".nopal/gates.jsonc"))
                        .expect("remove required gates module");
                }
                Block::InvalidPolicy => {
                    fs::write(temp.path().join(".nopal/policy.jsonc"), "{ invalid policy")
                        .expect("write invalid policy");
                }
                Block::Decision(_) | Block::Placement => {}
            }
            let manifest = temp.path().join("slice.json");
            fs::write(&manifest, b"exact manifest bytes\n").expect("write slice");
            let (factory, calls) = factory();

            let report = run_submit_with_factory(temp.path(), &manifest, None, &factory);

            assert!(!report.ok);
            assert!(!report.submitted);
            let calls = calls.lock().expect("calls lock poisoned");
            assert_eq!(calls.connect, 0);
            assert_eq!(calls.submit, 0);
        }
    }

    #[test]
    fn valid_endpoint_override_supersedes_invalid_configured_base_url() {
        let temp = tempfile::tempdir().expect("create tempdir");
        write_project(temp.path(), "allow", "dedicated_run_runtime");
        fs::write(
            temp.path().join(".nopal/config.jsonc"),
            r#"{
  "version": "nopal.config/v1",
  "rondo_core": {
    "base_url": 7,
    "request_timeout_ms": 250,
    "repo_id": "configured-repo"
  }
}
"#,
        )
        .expect("write config");
        let manifest = temp.path().join("slice.json");
        fs::write(&manifest, "{}").expect("write slice");
        let (factory, calls) = factory();

        let report = run_submit_with_factory(
            temp.path(),
            &manifest,
            Some("http://127.0.0.1:4401"),
            &factory,
        );

        assert!(report.ok);
        let calls = calls.lock().expect("calls lock poisoned");
        assert_eq!(calls.connect, 1);
        assert_eq!(calls.base_url.as_deref(), Some("http://127.0.0.1:4401"));
    }

    #[test]
    fn observation_ignores_unused_configured_repo_id_and_overridden_base_url_errors() {
        let temp = tempfile::tempdir().expect("create tempdir");
        write_project(temp.path(), "allow", "dedicated_run_runtime");
        fs::write(
            temp.path().join(".nopal/config.jsonc"),
            r#"{
  "version": "nopal.config/v1",
  "rondo_core": {
    "base_url": 7,
    "request_timeout_ms": 250,
    "repo_id": " invalid-unused-id "
  }
}
"#,
        )
        .expect("write config");
        let (factory, calls) = factory();

        let report = run_observe_with_factory(
            temp.path(),
            "configured-repo",
            "run-1",
            None,
            Some("http://127.0.0.1:4401"),
            &factory,
        );

        assert!(report.ok);
        let calls = calls.lock().expect("calls lock poisoned");
        assert_eq!(calls.connect, 1);
        assert_eq!(calls.status, 1);
        assert_eq!(calls.events, 1);
    }

    #[test]
    fn observation_ignores_invalid_configured_repo_id_without_an_endpoint_override() {
        let temp = tempfile::tempdir().expect("create tempdir");
        write_project(temp.path(), "allow", "dedicated_run_runtime");
        fs::write(
            temp.path().join(".nopal/config.jsonc"),
            r#"{
  "version": "nopal.config/v1",
  "rondo_core": {
    "base_url": "http://127.0.0.1:4400",
    "request_timeout_ms": 250,
    "repo_id": " invalid-unused-id "
  }
}
"#,
        )
        .expect("write config");
        let (factory, calls) = factory();

        let report = run_observe_with_factory(
            temp.path(),
            "configured-repo",
            "run-1",
            None,
            None,
            &factory,
        );

        assert!(report.ok);
        assert_eq!(calls.lock().expect("calls lock poisoned").connect, 1);
    }

    #[test]
    fn submit_rejects_unsafe_manifest_and_invalid_repo_id_before_connect() {
        let repo = tempfile::tempdir().expect("create repo");
        let outside = tempfile::tempdir().expect("create outside dir");
        write_project(repo.path(), "allow", "dedicated_run_runtime");
        write_rondo_config(repo.path(), Some("configured-repo"));
        let outside_manifest = outside.path().join("slice.json");
        fs::write(&outside_manifest, "{}").expect("write outside slice");
        let (factory, calls) = factory();

        let report = run_submit_with_factory(repo.path(), &outside_manifest, None, &factory);
        assert!(!report.ok);
        assert_eq!(calls.lock().expect("calls lock poisoned").connect, 0);

        write_rondo_config(repo.path(), Some(" bad-repo "));
        let inside = repo.path().join("slice.json");
        fs::write(&inside, "{}").expect("write inside slice");
        let report = run_submit_with_factory(repo.path(), &inside, None, &factory);
        assert!(!report.ok);
        assert_eq!(calls.lock().expect("calls lock poisoned").connect, 0);
    }

    #[cfg(unix)]
    #[test]
    fn submit_rejects_manifest_symlink_before_connect() {
        use std::os::unix::fs::symlink;

        let repo = tempfile::tempdir().expect("create repo");
        write_project(repo.path(), "allow", "dedicated_run_runtime");
        write_rondo_config(repo.path(), Some("configured-repo"));
        let target = repo.path().join("slice-target.json");
        let link = repo.path().join("slice.json");
        fs::write(&target, "{}").expect("write target");
        symlink(&target, &link).expect("create manifest symlink");
        let (factory, calls) = factory();

        let report = run_submit_with_factory(repo.path(), &link, None, &factory);

        assert!(!report.ok);
        assert_eq!(report.diagnostics, ["Manifest must not be a symbolic link"]);
        assert_eq!(calls.lock().expect("calls lock poisoned").connect, 0);
    }

    #[test]
    fn observe_preserves_bounded_page_and_settles_only_when_caught_up() {
        let temp = tempfile::tempdir().expect("create tempdir");
        write_project(temp.path(), "allow", "dedicated_run_runtime");
        write_rondo_config(temp.path(), None);
        let (factory, calls) = factory();

        let report = run_observe_with_factory(
            temp.path(),
            "configured-repo",
            "run-1",
            Some("rondo.core/v1:8"),
            None,
            &factory,
        );
        assert!(report.ok);
        assert_eq!(report.handle.repo_id, "configured-repo");
        assert_eq!(report.handle.plot_id, "plot-test");
        assert_eq!(report.handle.run_id, "run-1");
        assert_eq!(report.status.as_deref(), Some("completed"));
        assert_eq!(report.event_cursor.as_deref(), Some("rondo.core/v1:9"));
        assert_eq!(report.next_event_cursor.as_deref(), Some("rondo.core/v1:9"));
        assert!(!report.has_more);
        assert!(report.settled);
        assert_eq!(
            report.evidence_pointers[0]["uri"],
            "rondo://opaque/evidence"
        );
        let calls = calls.lock().expect("calls lock poisoned");
        assert_eq!(calls.connect, 1);
        assert_eq!(calls.status, 1);
        assert_eq!(calls.events, 1);
        assert_eq!(calls.event_cursor, Some(Some("rondo.core/v1:8".to_owned())));
    }

    #[test]
    fn repo_id_limit_is_exact_utf8_bytes_and_default_is_path_opaque() {
        let exact = "é".repeat(256);
        assert_eq!(exact.len(), 512);
        assert_eq!(validate_repo_id(&exact), Ok(()));
        assert!(validate_repo_id(&format!("{exact}x")).is_err());

        let root = Path::new("/private/example/checkout");
        let first = default_repo_id(root);
        assert_eq!(first, default_repo_id(root));
        assert!(first.starts_with("nopal.repo/v1:"));
        assert_eq!(first.len(), "nopal.repo/v1:".len() + 64);
        assert!(!first.contains("private"));
        assert!(!first.contains("checkout"));
    }

    #[test]
    fn observe_redacts_payloads_and_waits_for_terminal_page_to_catch_up() {
        let temp = tempfile::tempdir().expect("create tempdir");
        write_project(temp.path(), "allow", "dedicated_run_runtime");
        write_rondo_config(temp.path(), None);
        let (mut factory, _) = factory();
        factory.client.status_response = Ok(RunStatusResponse {
            run_id: "run-1".to_owned(),
            plot_id: Some("plot-test".to_owned()),
            status: "completed".to_owned(),
            last_event: Some(json!({
                "api_token": "plain-secret",
                "message": "PASSWORD=hunter2"
            })),
            evidence_pointers: vec![
                EvidencePointer {
                    artifact_kind: "execution_request".to_owned(),
                    uri: "rondo-run://run-1/artifacts/execution-request.json".to_owned(),
                },
                EvidencePointer {
                    artifact_kind: "payload_omitted".to_owned(),
                    uri: "rondo-run://run-1/opaque/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_owned(),
                },
            ],
            event_cursor: "rondo.core/v1:10".to_owned(),
        });
        factory.client.events_response = Ok(RunEventsResponse {
            plot_id: Some("plot-test".to_owned()),
            events: vec![json!({
                "type": "run.output",
                "payload": "Authorization: Bearer secret-value"
            })],
            next_event_cursor: "rondo.core/v1:9".to_owned(),
            has_more: true,
        });

        let report = run_observe_with_factory(
            temp.path(),
            "configured-repo",
            "run-1",
            None,
            None,
            &factory,
        );

        assert!(report.ok);
        assert!(report.has_more);
        assert!(!report.settled);
        assert_eq!(
            report.last_event.as_ref().expect("last event")["api_token"],
            "[REDACTED]"
        );
        assert_eq!(
            report.last_event.as_ref().expect("last event")["message"],
            "PASSWORD=[REDACTED]"
        );
        assert!(report.evidence_pointers[0].get("auth_header").is_none());
        assert_eq!(
            report.evidence_pointers[0]["uri"],
            "rondo-run://run-1/artifacts/execution-request.json"
        );
        assert_eq!(
            report.evidence_pointers[1]["uri"],
            "rondo-run://run-1/opaque/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        );
        assert_eq!(
            report.events[0]["payload"],
            "Authorization: Bearer [REDACTED]"
        );

        let json = serde_json::to_string(&report).expect("serialize report");
        let toon = run_observation_toon(&report);
        for secret in ["plain-secret", "hunter2", "secret-value"] {
            assert!(!json.contains(secret));
            assert!(!toon.contains(secret));
        }
    }

    #[test]
    fn safe_rondo_owned_evidence_uri_forms_survive_redaction_exactly() {
        let value = json!({
            "evidence_pointers": [
                {"uri": "rondo-run://run-1/artifacts/execution-request.json"},
                {"uri": "rondo-run://run-1/opaque/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"}
            ]
        });

        assert_eq!(sanitize_json(&value), value);
    }

    #[test]
    fn submit_and_observation_toon_value_trees_match_json_for_success_and_failure() {
        let failed_submit = fail_submit(empty_submit_report(), "blocked");
        assert_eq!(
            toon::to_json(&run_submit_toon_value(&failed_submit)),
            serde_json::to_value(&failed_submit).expect("serialize failed submission")
        );

        let temp = tempfile::tempdir().expect("create tempdir");
        write_project(temp.path(), "allow", "dedicated_run_runtime");
        write_rondo_config(temp.path(), Some("configured-repo"));
        let manifest = temp.path().join("slice.json");
        fs::write(&manifest, "{}").expect("write slice");
        let (factory, _) = factory();
        let submitted = run_submit_with_factory(temp.path(), &manifest, None, &factory);
        assert_eq!(
            toon::to_json(&run_submit_toon_value(&submitted)),
            serde_json::to_value(&submitted).expect("serialize successful submission")
        );

        let observed = run_observe_with_factory(
            temp.path(),
            "configured-repo",
            "run-1",
            None,
            None,
            &factory,
        );
        assert_eq!(
            toon::to_json(&run_observation_toon_value(&observed)),
            serde_json::to_value(&observed).expect("serialize successful observation")
        );

        let failed_observation = fail_observation(
            empty_observation_report("repo", "plot-test", "run"),
            "unavailable",
        );
        assert_eq!(
            toon::to_json(&run_observation_toon_value(&failed_observation)),
            serde_json::to_value(&failed_observation).expect("serialize failed observation")
        );

        for rendered in [run_submit_toon(&submitted), run_observation_toon(&observed)] {
            assert!(!rendered.contains("<non-scalar>"));
        }
    }

    #[test]
    fn invalid_run_id_is_not_echoed_in_failed_observation() {
        let temp = tempfile::tempdir().expect("create tempdir");
        write_project(temp.path(), "allow", "dedicated_run_runtime");
        write_rondo_config(temp.path(), None);
        let (factory, calls) = factory();

        let report = run_observe_with_factory(
            temp.path(),
            "configured-repo",
            "run-1\nforged: value",
            None,
            None,
            &factory,
        );

        assert!(!report.ok);
        assert_eq!(report.handle.repo_id, "configured-repo");
        assert_eq!(report.handle.run_id, "-");
        assert_eq!(calls.lock().expect("calls lock poisoned").connect, 0);
    }

    #[test]
    fn unvalidated_run_id_is_not_echoed_when_repo_id_validation_fails_first() {
        let temp = tempfile::tempdir().expect("create tempdir");
        write_project(temp.path(), "allow", "dedicated_run_runtime");
        write_rondo_config(temp.path(), None);
        let (factory, calls) = factory();

        let report = run_observe_with_factory(
            temp.path(),
            "invalid\nrepo: value",
            "run-1\nforged: value",
            None,
            None,
            &factory,
        );

        assert!(!report.ok);
        assert_eq!(report.handle.repo_id, "-");
        assert_eq!(report.handle.run_id, "-");
        assert_eq!(calls.lock().expect("calls lock poisoned").connect, 0);
        let json = serde_json::to_string(&report).expect("serialize report");
        let toon = run_observation_toon(&report);
        for untrusted in ["invalid\nrepo: value", "run-1\nforged: value"] {
            assert!(!json.contains(untrusted));
            assert!(!toon.contains(untrusted));
        }
    }
}
