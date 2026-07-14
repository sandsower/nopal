//! Consumer-side model for `nopal.field/v1`.

use std::collections::BTreeMap;

use serde::Deserialize;

pub const FIELD_KIND: &str = "nopal.field/v1";

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldSnapshot {
    pub kind: String,
    #[serde(default)]
    pub plots: Vec<FieldPlot>,
    pub entries: Vec<FieldEntry>,
    #[serde(default)]
    pub asks_unbound: Vec<FieldAsk>,
    #[serde(default)]
    pub diagnostics: Vec<FieldDiagnostic>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldPlot {
    pub kind: String,
    pub plot_id: String,
    #[serde(default)]
    pub title: String,
    #[serde(default)]
    pub provisional: bool,
    #[serde(default)]
    pub progress: String,
    #[serde(default)]
    pub conditions: Vec<String>,
    #[serde(default)]
    pub seed: FieldSeed,
    #[serde(default)]
    pub intent: String,
    #[serde(default)]
    pub fruit: FieldFruit,
    #[serde(default)]
    pub executions: Vec<FieldPlotExecution>,
    #[serde(default)]
    pub sessions: Vec<FieldSession>,
    #[serde(default)]
    pub selected_session_id: Option<String>,
    #[serde(default)]
    pub establishment: Option<FieldEstablishment>,
    #[serde(default)]
    pub repositories: Vec<FieldRepository>,
    #[serde(default)]
    pub workspaces: Vec<FieldWorkspace>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldFruit {
    #[serde(default = "absent_fruit_state")]
    pub state: String,
    #[serde(skip, default = "present_fruit")]
    pub present: bool,
}

impl Default for FieldFruit {
    fn default() -> Self {
        Self {
            state: absent_fruit_state(),
            present: false,
        }
    }
}

fn present_fruit() -> bool {
    true
}

fn absent_fruit_state() -> String {
    "absent".to_owned()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldPlotExecution {
    pub service_id: String,
    pub repo_id: String,
    pub run_id: String,
    #[serde(default)]
    pub manifest_sha256: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub outcome: Option<String>,
    #[serde(default)]
    pub event_cursor: String,
    #[serde(default)]
    pub evidence: Vec<EvidencePointer>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldEstablishment {
    pub event: String,
    pub primary_repository_id: String,
    pub effective_workflow: FieldFrozenWorkflow,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldFrozenWorkflow {
    pub source_repository_id: String,
    pub source_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldRepository {
    pub repository_id: String,
    pub root: String,
    pub configuration_root: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub roots: Vec<FieldRoot>,
    #[serde(default)]
    pub gate_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldRoot {
    pub id: String,
    pub statement: String,
    #[serde(default)]
    pub proof_requirements: Vec<FieldProofRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldProofRequirement {
    pub id: String,
    pub stage: String,
    pub gates: Vec<String>,
    pub required: bool,
    pub required_present: bool,
    pub on_missing: String,
    pub on_failure: String,
}

impl<'de> Deserialize<'de> for FieldProofRequirement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            id: String,
            #[serde(default)]
            stage: String,
            #[serde(default)]
            gates: Vec<String>,
            #[serde(default)]
            required: Option<bool>,
            #[serde(default)]
            on_missing: String,
            #[serde(default)]
            on_failure: String,
        }

        let wire = Wire::deserialize(deserializer)?;
        Ok(Self {
            id: wire.id,
            stage: wire.stage,
            gates: wire.gates,
            required: wire.required.unwrap_or(false),
            required_present: wire.required.is_some(),
            on_missing: wire.on_missing,
            on_failure: wire.on_failure,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldWorkspace {
    pub workspace_id: String,
    pub repository_id: String,
    pub root: String,
    #[serde(default)]
    pub revision: Option<String>,
    #[serde(default)]
    pub kind: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Deserialize)]
pub struct FieldSeed {
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub text: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldSession {
    pub session_id: String,
    #[serde(default)]
    pub mode: String,
    #[serde(default)]
    pub host: String,
    #[serde(default)]
    pub host_session: String,
    #[serde(default)]
    pub host_pane: Option<String>,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub protocol: Option<FieldSessionProtocolEndpoint>,
    #[serde(default)]
    pub workspace: Option<String>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldSessionProtocolEndpoint {
    pub kind: String,
    pub transport: String,
    pub address: String,
    pub state: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldEntry {
    pub run_id: String,
    #[serde(default)]
    pub flow: String,
    #[serde(default)]
    pub skill: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub ticket_id: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub started_at: String,
    #[serde(default)]
    pub updated_at: String,
    pub placement: FieldPlacement,
    #[serde(default)]
    pub gates: Vec<FieldGate>,
    #[serde(default)]
    pub rondo: Option<RondoRun>,
    #[serde(default)]
    pub asks: Vec<FieldAsk>,
    #[serde(default)]
    pub stale: bool,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldPlacement {
    pub repo: String,
    #[serde(default)]
    pub repo_hash: String,
    #[serde(default)]
    pub branch: String,
    #[serde(default)]
    pub run_dir: String,
    #[serde(default)]
    pub flow: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldGate {
    #[serde(default)]
    pub attempt: Option<u64>,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub scope: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub classification: String,
    #[serde(default)]
    pub path: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct RondoRun {
    #[serde(default)]
    pub run_id: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    pub evidence: Vec<EvidencePointer>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct EvidencePointer {
    #[serde(default)]
    pub artifact_kind: String,
    #[serde(default)]
    pub uri: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldAsk {
    #[serde(default)]
    pub kind: String,
    pub ask_id: String,
    #[serde(default)]
    pub action: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub session_id: String,
    #[serde(default)]
    pub repo: String,
    #[serde(default)]
    pub state: String,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub expires_at: String,
    #[serde(default)]
    pub run_id: Option<String>,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldDiagnostic {
    #[serde(default)]
    pub severity: String,
    #[serde(default)]
    pub code: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub position: Option<FieldPosition>,
    #[serde(default)]
    pub message: String,
    #[serde(flatten)]
    pub extra: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct FieldPosition {
    pub line: usize,
    pub column: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProgressState {
    Planned,
    Active,
    Review,
    Completed,
    Unavailable,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FruitState {
    Absent,
    Accepted,
    Unavailable,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionState {
    Running,
    Paused,
    Completed,
    Failed,
    Terminated,
    Unavailable,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExecutionOutcome {
    Completed,
    Failed,
    Terminated,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateAttemptState {
    Pass,
    Fail,
    Unavailable,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalState {
    Pending,
    Approved,
    Denied,
    Expired,
    Unavailable,
    Unknown(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Error,
    Warning,
    Info,
    Unavailable,
    Unknown(String),
}

impl FieldPlot {
    pub fn progress_state(&self) -> ProgressState {
        match self.progress.as_str() {
            "planned" => ProgressState::Planned,
            "active" => ProgressState::Active,
            "review" => ProgressState::Review,
            "completed" => ProgressState::Completed,
            "" => ProgressState::Unavailable,
            value => ProgressState::Unknown(value.to_owned()),
        }
    }

    pub fn fruit_state(&self) -> FruitState {
        if !self.fruit.present {
            return FruitState::Unavailable;
        }
        match self.fruit.state.as_str() {
            "absent" => FruitState::Absent,
            "accepted" => FruitState::Accepted,
            "" => FruitState::Unavailable,
            value => FruitState::Unknown(value.to_owned()),
        }
    }
}

impl FieldPlotExecution {
    pub fn status_state(&self) -> ExecutionState {
        match self.status.as_str() {
            "running" => ExecutionState::Running,
            "paused" => ExecutionState::Paused,
            "completed" => ExecutionState::Completed,
            "failed" => ExecutionState::Failed,
            "terminated" => ExecutionState::Terminated,
            "" => ExecutionState::Unavailable,
            value => ExecutionState::Unknown(value.to_owned()),
        }
    }

    pub fn outcome_state(&self) -> Option<ExecutionOutcome> {
        self.outcome.as_deref().map(|value| match value {
            "completed" => ExecutionOutcome::Completed,
            "failed" => ExecutionOutcome::Failed,
            "terminated" => ExecutionOutcome::Terminated,
            value => ExecutionOutcome::Unknown(value.to_owned()),
        })
    }
}

impl FieldGate {
    pub fn status_state(&self) -> GateAttemptState {
        match self.status.as_str() {
            "pass" => GateAttemptState::Pass,
            "fail" => GateAttemptState::Fail,
            "" => GateAttemptState::Unavailable,
            value => GateAttemptState::Unknown(value.to_owned()),
        }
    }
}

impl FieldAsk {
    pub fn approval_state(&self) -> ApprovalState {
        match self.state.as_str() {
            "pending" => ApprovalState::Pending,
            "approved" => ApprovalState::Approved,
            "denied" => ApprovalState::Denied,
            "expired" => ApprovalState::Expired,
            "" => ApprovalState::Unavailable,
            value => ApprovalState::Unknown(value.to_owned()),
        }
    }

    pub fn is_canonical(&self) -> bool {
        self.kind == "nopal.ask/v1"
    }

    pub fn is_approved(&self) -> bool {
        self.is_canonical() && self.approval_state() == ApprovalState::Approved
    }
}

impl FieldDiagnostic {
    pub fn severity_state(&self) -> DiagnosticSeverity {
        match self.severity.as_str() {
            "error" => DiagnosticSeverity::Error,
            "warning" => DiagnosticSeverity::Warning,
            "info" => DiagnosticSeverity::Info,
            "" => DiagnosticSeverity::Unavailable,
            value => DiagnosticSeverity::Unknown(value.to_owned()),
        }
    }
}

/// Parse the public JSON envelope as a consumer would. Additive fields are
/// retained in each model's `extra` map for forward-compatible clients, while
/// the versioned kind and required placement/entry shapes still fail loudly.
pub fn parse_field(value: &serde_json::Value) -> Result<FieldSnapshot, String> {
    let kind = value
        .get("kind")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default();
    if kind != FIELD_KIND {
        return Err(format!("unexpected field kind {kind:?}"));
    }
    FieldSnapshot::deserialize(value).map_err(|err| format!("invalid {FIELD_KIND}: {err}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn captured_field() -> serde_json::Value {
        serde_json::json!({
            "kind": "nopal.field/v1",
            "ok": true,
            "future_top_level_field": {"ignored": true},
            "plots": [{
                "kind": "nopal.plot/v1",
                "plot_id": "plot-1",
                "title": "New Plot",
                "provisional": true,
                "progress": "planned",
                "conditions": [],
                "seed": {"source": "field_open", "text": ""},
                "intent": "",
                "fruit": {"state": "absent"},
                "executions": [{
                    "service_id": "rondo-core",
                    "repo_id": "repo-1",
                    "run_id": "rondo-run-1",
                    "manifest_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                    "status": "completed",
                    "outcome": "completed",
                    "event_cursor": "rondo.core/v1:12",
                    "evidence": [{
                        "artifact_kind": "delivery_artifact",
                        "uri": "rondo-run://rondo-run-1/artifacts/delivery.json"
                    }],
                    "created_at": "2026-07-12T06:00:00+00:00",
                    "updated_at": "2026-07-12T06:05:00+00:00"
                }],
                "sessions": [{
                    "session_id": "session-1",
                    "mode": "interactive",
                    "host": "pi",
                    "host_session": "nopal-work",
                    "host_pane": "%4",
                    "state": "active",
                    "protocol": {
                        "kind": "nopal.session/v1",
                        "transport": "unix",
                        "address": "/tmp/nopal-session-1.sock",
                        "state": "ready"
                    },
                    "workspace": null,
                    "created_at": "2026-07-12T06:00:00+00:00",
                    "updated_at": "2026-07-12T06:00:01+00:00"
                }],
                "selected_session_id": "session-1",
                "created_at": "2026-07-12T06:00:00+00:00",
                "updated_at": "2026-07-12T06:00:01+00:00"
            }],
            "entries": [{
                "run_id": "run-1",
                "flow": "implement",
                "skill": "implement",
                "status": "running",
                "ticket_id": "TASK-44",
                "branch": "codex/task-44",
                "started_at": "2026-07-09T12:00:00+00:00",
                "updated_at": "2026-07-09T12:01:00+00:00",
                "stale": false,
                "placement": {
                    "repo": "/work/nopal",
                    "repo_hash": "abc123",
                    "branch": "codex/task-44",
                    "run_dir": "/state/run-1",
                    "flow": "implement",
                    "future_placement_field": 42
                },
                "gates": [{
                    "attempt": 1,
                    "name": "test",
                    "scope": "repo",
                    "status": "pass",
                    "classification": "required",
                    "path": "/state/run-1/gates/test.json"
                }],
                "rondo": {
                    "run_id": "run-1",
                    "status": "completed",
                    "evidence": [{"artifact_kind": "log", "uri": "rondo://run-1/log"}]
                },
                "asks": [{
                    "ask_id": "ask-1",
                    "action": "git.push",
                    "reason": "publish",
                    "session_id": "session-1",
                    "repo": "/work/nopal",
                    "state": "pending",
                    "created_at": "2026-07-09T12:00:00+00:00",
                    "expires_at": "2026-07-09T13:00:00+00:00",
                    "run_id": "run-1"
                }],
                "future_entry_field": [1, 2, 3]
            }],
            "asks_unbound": [{
                "ask_id": "ask-2",
                "action": "net.fetch",
                "reason": "lookup",
                "session_id": "session-2",
                "repo": "/work/nopal",
                "state": "pending",
                "created_at": "2026-07-09T12:00:00+00:00",
                "expires_at": "2026-07-09T13:00:00+00:00"
            }]
        })
    }

    #[test]
    fn parses_the_host_neutral_field_contract_without_losing_placement() {
        let snapshot = parse_field(&captured_field()).unwrap();

        assert_eq!(snapshot.entries.len(), 1);
        assert_eq!(snapshot.plots.len(), 1);
        assert_eq!(snapshot.plots[0].plot_id, "plot-1");
        assert_eq!(snapshot.plots[0].sessions[0].session_id, "session-1");
        assert_eq!(
            snapshot.plots[0].sessions[0]
                .protocol
                .as_ref()
                .map(|protocol| protocol.address.as_str()),
            Some("/tmp/nopal-session-1.sock")
        );
        assert_eq!(snapshot.plots[0].fruit.state, "absent");
        let execution = &snapshot.plots[0].executions[0];
        assert_eq!(execution.service_id, "rondo-core");
        assert_eq!(execution.repo_id, "repo-1");
        assert_eq!(execution.run_id, "rondo-run-1");
        assert_eq!(execution.status, "completed");
        assert_eq!(execution.outcome.as_deref(), Some("completed"));
        assert_eq!(execution.event_cursor, "rondo.core/v1:12");
        assert_eq!(execution.evidence[0].artifact_kind, "delivery_artifact");
        assert_eq!(
            execution.evidence[0].uri,
            "rondo-run://rondo-run-1/artifacts/delivery.json"
        );
        let run = &snapshot.entries[0];
        assert_eq!(run.run_id, "run-1");
        assert_eq!(run.placement.repo, "/work/nopal");
        assert_eq!(run.gates[0].status, "pass");
        assert_eq!(run.gates[0].attempt, Some(1));
        assert_eq!(run.asks[0].ask_id, "ask-1");
        assert_eq!(
            run.rondo.as_ref().unwrap().status.as_deref(),
            Some("completed")
        );
        assert_eq!(snapshot.asks_unbound[0].ask_id, "ask-2");
        assert_eq!(snapshot.extra["future_top_level_field"]["ignored"], true);
        assert_eq!(
            run.extra["future_entry_field"],
            serde_json::json!([1, 2, 3])
        );
        assert_eq!(run.placement.extra["future_placement_field"], 42);
    }

    #[test]
    fn accepts_legacy_field_sessions_without_a_protocol_endpoint() {
        let mut field = captured_field();
        field["plots"][0]["sessions"][0]
            .as_object_mut()
            .unwrap()
            .remove("protocol");

        let snapshot = parse_field(&field).unwrap();

        assert_eq!(snapshot.plots[0].sessions[0].protocol, None);
    }

    #[test]
    fn rejects_an_unexpected_kind_and_a_missing_entries_array() {
        let wrong_kind = serde_json::json!({"kind": "nopal.status/v1", "entries": []});
        assert_eq!(
            parse_field(&wrong_kind).unwrap_err().to_string(),
            "unexpected field kind \"nopal.status/v1\""
        );

        let missing_entries = serde_json::json!({"kind": "nopal.field/v1"});
        assert!(
            parse_field(&missing_entries)
                .unwrap_err()
                .to_string()
                .contains("entries")
        );
    }

    #[test]
    fn legacy_field_capture_without_plots_remains_valid() {
        let value = serde_json::json!({"kind": "nopal.field/v1", "entries": []});
        let snapshot = parse_field(&value).unwrap();
        assert!(snapshot.plots.is_empty());
    }

    #[test]
    fn plot_capture_without_seed_uses_an_empty_seed() {
        let mut value = captured_field();
        value["plots"][0].as_object_mut().unwrap().remove("seed");

        let snapshot = parse_field(&value).unwrap();

        assert_eq!(snapshot.plots[0].seed, FieldSeed::default());
    }

    #[test]
    fn legacy_plot_capture_defaults_fruit_to_absent_and_executions_to_empty() {
        let mut value = captured_field();
        let plot = value["plots"][0].as_object_mut().unwrap();
        plot.remove("fruit");
        plot.remove("executions");

        let snapshot = parse_field(&value).unwrap();

        assert_eq!(snapshot.plots[0].fruit.state, "absent");
        assert!(snapshot.plots[0].executions.is_empty());
    }

    #[test]
    fn parses_typed_assurance_states_and_diagnostics_without_inference() {
        let mut value = captured_field();
        value["diagnostics"] = serde_json::json!([{
            "severity": "warning",
            "code": "field_partial_coverage",
            "path": "field",
            "position": {"line": 2, "column": 4},
            "message": "partial source"
        }]);
        value["plots"][0]["fruit"]["state"] = serde_json::json!("accepted");
        value["entries"][0]["gates"][0]["status"] = serde_json::json!("fail");
        value["entries"][0]["asks"][0]["kind"] = serde_json::json!("nopal.ask/v1");
        value["entries"][0]["asks"][0]["state"] = serde_json::json!("approved");

        let snapshot = parse_field(&value).unwrap();

        assert_eq!(snapshot.plots[0].progress_state(), ProgressState::Planned);
        assert_eq!(snapshot.plots[0].fruit_state(), FruitState::Accepted);
        assert_eq!(
            snapshot.plots[0].executions[0].status_state(),
            ExecutionState::Completed
        );
        assert_eq!(
            snapshot.plots[0].executions[0].outcome_state(),
            Some(ExecutionOutcome::Completed)
        );
        assert_eq!(snapshot.entries[0].gates[0].attempt, Some(1));
        assert_eq!(
            snapshot.entries[0].gates[0].status_state(),
            GateAttemptState::Fail
        );
        assert_eq!(
            snapshot.entries[0].asks[0].approval_state(),
            ApprovalState::Approved
        );
        assert!(snapshot.entries[0].asks[0].is_canonical());
        assert!(snapshot.entries[0].asks[0].is_approved());
        assert_eq!(
            snapshot.diagnostics[0].severity_state(),
            DiagnosticSeverity::Warning
        );
        assert_eq!(snapshot.diagnostics[0].position.as_ref().unwrap().line, 2);
    }

    #[test]
    fn missing_and_malformed_assurance_states_fail_closed() {
        let mut value = captured_field();
        value["plots"][0]
            .as_object_mut()
            .unwrap()
            .remove("progress");
        value["plots"][0].as_object_mut().unwrap().remove("fruit");
        value["plots"][0]["executions"][0]
            .as_object_mut()
            .unwrap()
            .remove("outcome");
        value["plots"][0]["executions"][0]["status"] = serde_json::json!("complete-ish");
        value["entries"][0]["gates"][0]["status"] = serde_json::json!("passing");
        value["entries"][0]["asks"][0]["kind"] = serde_json::json!("nopal.ask/v2");
        value["entries"][0]["asks"][0]["state"] = serde_json::json!("allow");

        let snapshot = parse_field(&value).unwrap();

        assert_eq!(
            snapshot.plots[0].progress_state(),
            ProgressState::Unavailable
        );
        assert_eq!(snapshot.plots[0].fruit_state(), FruitState::Unavailable);
        assert_eq!(
            snapshot.plots[0].executions[0].status_state(),
            ExecutionState::Unknown("complete-ish".to_owned())
        );
        assert_eq!(snapshot.plots[0].executions[0].outcome_state(), None);
        assert_eq!(
            snapshot.entries[0].gates[0].status_state(),
            GateAttemptState::Unknown("passing".to_owned())
        );
        assert_eq!(
            snapshot.entries[0].asks[0].approval_state(),
            ApprovalState::Unknown("allow".to_owned())
        );
        assert!(!snapshot.entries[0].asks[0].is_canonical());
        assert!(!snapshot.entries[0].asks[0].is_approved());
    }

    #[test]
    fn omitted_proof_required_is_unavailable_not_optional() {
        let mut value = captured_field();
        value["plots"][0]["repositories"] = serde_json::json!([{
            "repository_id": "repo-1",
            "root": "/repo",
            "configuration_root": "/repo",
            "roots": [{
                "id": "quality",
                "statement": "Quality remains green",
                "proof_requirements": [{
                    "id": "pre-pr",
                    "stage": "pre_pr",
                    "gates": ["test"],
                    "on_missing": "block",
                    "on_failure": "block"
                }]
            }]
        }]);

        let snapshot = parse_field(&value).unwrap();
        let proof = &snapshot.plots[0].repositories[0].roots[0].proof_requirements[0];

        assert!(!proof.required);
        assert!(!proof.required_present);
    }
}
