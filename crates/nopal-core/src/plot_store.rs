//! Effectful store for `nopal.plot/v1` documents.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::diagnostics::{Code, Diagnostic};
use crate::plot::{Fruit, PLOT_KIND, PlotDocument, PlotSession, Seed};
use crate::plot_establishment::{self, ApplyOutcome, EstablishmentError, EstablishmentInput};
use crate::plot_execution::{self, AcceptanceInput, ExecutionError, ObservationInput};
use crate::run_ledger;
use crate::run_ledger_store::{self as ledger_store, RunLock};

const CONTEXT_KIND: &str = "nopal.field_context/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FieldContext {
    kind: String,
    field_session: String,
    selected_plot_id: String,
}

#[derive(Debug, Clone)]
pub struct PlotEnv {
    pub state_dir: PathBuf,
}

impl PlotEnv {
    pub fn discover(state_dir: Option<&Path>) -> Self {
        let state_dir = state_dir.map(Path::to_path_buf).unwrap_or_else(|| {
            std::env::var_os("NOPAL_STATE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join(".local")
                        .join("state")
                        .join("nopal")
                })
        });
        Self { state_dir }
    }

    fn plots_root(&self) -> PathBuf {
        self.state_dir.join("plots")
    }

    fn contexts_root(&self) -> PathBuf {
        self.state_dir.join("field").join("contexts")
    }
}

fn now_iso() -> String {
    run_ledger::iso_utc(ledger_store::epoch_now())
}

fn context_key(field_session: &str) -> String {
    let digest = Sha256::digest(field_session.as_bytes());
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn context_path(env: &PlotEnv, field_session: &str) -> PathBuf {
    env.contexts_root()
        .join(format!("{}.json", context_key(field_session)))
}

fn plot_path(env: &PlotEnv, plot_id: &str) -> PathBuf {
    env.plots_root().join(format!("{plot_id}.json"))
}

fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> io::Result<T> {
    let text = fs::read_to_string(path)?;
    serde_json::from_str(&text)
        .map_err(|err| invalid_data(format!("unreadable {}: {err}", path.display())))
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let text = serde_json::to_vec_pretty(value)
        .map_err(|err| invalid_data(format!("cannot serialize {}: {err}", path.display())))?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("plot.json");
    let temporary = parent.join(format!(".{name}.{}.tmp", ledger_store::token_hex(4)));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        file.write_all(&text)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    })();
    if result.is_err() && temporary.exists() {
        let _ = fs::remove_file(temporary);
    }
    result
}

pub fn load_plot(env: &PlotEnv, plot_id: &str) -> io::Result<PlotDocument> {
    if !run_ledger::identifier_valid(plot_id) {
        return Err(invalid_data("invalid Plot id"));
    }
    let plot: PlotDocument = read_json(&plot_path(env, plot_id))?;
    if plot.kind != PLOT_KIND || plot.plot_id != plot_id {
        return Err(invalid_data(format!("invalid Plot document for {plot_id}")));
    }
    plot_execution::validate_plot_snapshot(&plot).map_err(|error| {
        invalid_data(format!(
            "invalid execution facts in Plot document {plot_id}: {error}"
        ))
    })?;
    Ok(plot)
}

#[derive(Debug)]
pub enum EstablishStoreError {
    Io(io::Error),
    Domain(EstablishmentError),
}

#[derive(Debug)]
pub enum ExecutionStoreError {
    Io(io::Error),
    Domain(ExecutionError),
}

impl std::fmt::Display for ExecutionStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Domain(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for ExecutionStoreError {}

impl From<io::Error> for ExecutionStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<ExecutionError> for ExecutionStoreError {
    fn from(error: ExecutionError) -> Self {
        Self::Domain(error)
    }
}

impl std::fmt::Display for EstablishStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Domain(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for EstablishStoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Domain(error) => Some(error),
        }
    }
}

impl From<io::Error> for EstablishStoreError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<EstablishmentError> for EstablishStoreError {
    fn from(error: EstablishmentError) -> Self {
        Self::Domain(error)
    }
}

pub fn establish(
    env: &PlotEnv,
    plot_id: &str,
    input: EstablishmentInput,
) -> Result<(PlotDocument, ApplyOutcome), EstablishStoreError> {
    let _lock = RunLock::acquire(&env.plots_root())?;
    let mut plot = load_plot(env, plot_id)?;
    let outcome = plot_establishment::apply(&mut plot, input, &now_iso())?;
    if outcome != ApplyOutcome::Unchanged {
        write_json(&plot_path(env, plot_id), &plot)?;
    }
    Ok((plot, outcome))
}

pub fn record_execution_acceptance(
    env: &PlotEnv,
    plot_id: &str,
    input: AcceptanceInput,
) -> Result<(PlotDocument, plot_execution::ApplyOutcome), ExecutionStoreError> {
    let _lock = RunLock::acquire(&env.plots_root())?;
    let mut plot = load_plot(env, plot_id)?;
    let outcome = plot_execution::apply_acceptance(&mut plot, input, &now_iso())?;
    if outcome != plot_execution::ApplyOutcome::Unchanged {
        write_json(&plot_path(env, plot_id), &plot)?;
    }
    Ok((plot, outcome))
}

pub fn record_execution_observation(
    env: &PlotEnv,
    plot_id: &str,
    input: ObservationInput,
) -> Result<(PlotDocument, plot_execution::ApplyOutcome), ExecutionStoreError> {
    let _lock = RunLock::acquire(&env.plots_root())?;
    let mut plot = load_plot(env, plot_id)?;
    let outcome = plot_execution::apply_observation(&mut plot, input, &now_iso())?;
    if outcome != plot_execution::ApplyOutcome::Unchanged {
        write_json(&plot_path(env, plot_id), &plot)?;
    }
    Ok((plot, outcome))
}

pub fn ensure_provisional(env: &PlotEnv, field_session: &str) -> io::Result<PlotDocument> {
    let plots_root = env.plots_root();
    let _lock = RunLock::acquire(&plots_root)?;
    let context_path = context_path(env, field_session);
    if context_path.is_file() {
        let context: FieldContext = read_json(&context_path)?;
        if context.kind != CONTEXT_KIND || context.field_session != field_session {
            return Err(invalid_data("field context identity mismatch"));
        }
        return load_plot(env, &context.selected_plot_id);
    }

    // Derive the provisional identity from the durable Field identity so
    // an interruption after the Plot write but before the context write
    // can recover the same Plot instead of creating an orphan duplicate.
    let plot_id = format!("plot-{}", context_key(field_session));
    let path = plot_path(env, &plot_id);
    let plot = if path.is_file() {
        load_plot(env, &plot_id)?
    } else {
        let created_at = now_iso();
        let plot = PlotDocument {
            kind: PLOT_KIND.to_owned(),
            plot_id: plot_id.clone(),
            title: "New Plot".to_owned(),
            provisional: true,
            progress: "planned".to_owned(),
            conditions: Vec::new(),
            seed: Seed {
                source: "field_open".to_owned(),
                text: String::new(),
            },
            intent: String::new(),
            fruit: Fruit::default(),
            sessions: Vec::new(),
            selected_session_id: None,
            executions: Vec::new(),
            establishment: None,
            repositories: Vec::new(),
            workspaces: Vec::new(),
            created_at: created_at.clone(),
            updated_at: created_at,
        };
        write_json(&path, &plot)?;
        plot
    };
    write_json(
        &context_path,
        &FieldContext {
            kind: CONTEXT_KIND.to_owned(),
            field_session: field_session.to_owned(),
            selected_plot_id: plot_id,
        },
    )?;
    Ok(plot)
}

pub fn selected_for_field_session(
    env: &PlotEnv,
    field_session: &str,
) -> io::Result<Option<PlotDocument>> {
    let path = context_path(env, field_session);
    if !path.is_file() {
        return Ok(None);
    }
    let context: FieldContext = read_json(&path)?;
    if context.kind != CONTEXT_KIND || context.field_session != field_session {
        return Err(invalid_data("field context identity mismatch"));
    }
    load_plot(env, &context.selected_plot_id).map(Some)
}

pub fn bind_session(
    env: &PlotEnv,
    plot_id: &str,
    host_session: &str,
    host_pane: Option<&str>,
) -> io::Result<PlotDocument> {
    let _lock = RunLock::acquire(&env.plots_root())?;
    let mut plot = load_plot(env, plot_id)?;
    let updated_at = now_iso();
    let selected_session_id = if let Some(session) = plot
        .sessions
        .iter_mut()
        .find(|session| session.host_session == host_session)
    {
        session.host_pane = host_pane.map(str::to_owned);
        session.state = "active".to_owned();
        session.updated_at = updated_at.clone();
        session.session_id.clone()
    } else {
        let session_id = format!(
            "session-{}-{}",
            ledger_store::now_stamp(),
            ledger_store::token_hex(4)
        );
        plot.sessions.push(PlotSession {
            session_id: session_id.clone(),
            mode: "interactive".to_owned(),
            host: "pi".to_owned(),
            host_session: host_session.to_owned(),
            host_pane: host_pane.map(str::to_owned),
            state: "active".to_owned(),
            protocol: None,
            workspace: None,
            created_at: updated_at.clone(),
            updated_at: updated_at.clone(),
        });
        session_id
    };
    plot.selected_session_id = Some(selected_session_id);
    plot.updated_at = updated_at;
    write_json(&plot_path(env, plot_id), &plot)?;
    Ok(plot)
}

pub fn scan(env: &PlotEnv, warnings: &mut Vec<Diagnostic>) -> io::Result<Vec<PlotDocument>> {
    let root = env.plots_root();
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = fs::read_dir(root)?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension == "json")
        })
        .collect();
    paths.sort();
    let mut plots = Vec::new();
    for path in paths {
        let result = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| invalid_data(format!("invalid Plot path {}", path.display())))
            .and_then(|plot_id| load_plot(env, plot_id));
        match result {
            Ok(plot) => plots.push(plot),
            Err(error) => warnings.push(Diagnostic::warning(
                Code::PlotSnapshotInvalid,
                Path::new("plots")
                    .join(path.file_name().unwrap_or_default())
                    .display()
                    .to_string(),
                format!("skipping unreadable Plot snapshot: {error}"),
            )),
        }
    }
    Ok(plots)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::{
        ExecutionEvidencePointer, FrozenWorkflow, PLOT_KIND, RepositorySnapshot, WorkspaceSnapshot,
    };
    use crate::plot_establishment::{ApplyOutcome, EstablishmentInput};
    use crate::plot_execution::ObservationInput;

    fn establishment_input() -> EstablishmentInput {
        EstablishmentInput {
            event: "kickoff_context_ready".to_owned(),
            repository: RepositorySnapshot {
                repository_id: "repository-1".to_owned(),
                root: "/repo".to_owned(),
                configuration_root: "/repo".to_owned(),
                revision: Some("abc".to_owned()),
                process_artifact_hash: "artifact-1".to_owned(),
                roots: Vec::new(),
                gate_ids: vec!["test".to_owned()],
                policy_hash: None,
            },
            workspace: WorkspaceSnapshot {
                workspace_id: "workspace-1".to_owned(),
                repository_id: "repository-1".to_owned(),
                root: "/repo".to_owned(),
                revision: Some("abc".to_owned()),
                kind: "primary".to_owned(),
            },
            effective_workflow: FrozenWorkflow {
                source_repository_id: "repository-1".to_owned(),
                source_hash: "workflow-1".to_owned(),
                value: serde_json::json!({
                    "version": "nopal.workflow/v1",
                    "establishment": {"events": ["kickoff_context_ready"]}
                }),
            },
            host_session: "nopal-work".to_owned(),
            host_pane: Some("%4".to_owned()),
            protocol: None,
        }
    }

    fn establish_plot(env: &PlotEnv) -> PlotDocument {
        let plot = ensure_provisional(env, "nopal").unwrap();
        establish(env, &plot.plot_id, establishment_input())
            .unwrap()
            .0
    }

    fn accepted_execution(run_id: &str) -> AcceptanceInput {
        AcceptanceInput {
            service_id: "rondo-core".to_owned(),
            repo_id: "repository-1".to_owned(),
            run_id: run_id.to_owned(),
            manifest_sha256: "a".repeat(64),
            status: "running".to_owned(),
            event_cursor: "rondo.core/v1:0".to_owned(),
        }
    }

    #[test]
    fn ensure_is_idempotent_for_one_field_context() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));

        let first = ensure_provisional(&env, "nopal").unwrap();
        let second = ensure_provisional(&env, "nopal").unwrap();

        assert_eq!(first, second);
        assert_eq!(first.kind, PLOT_KIND);
        assert!(first.provisional);
        assert_eq!(first.progress, "planned");
        assert!(first.sessions.is_empty());
        assert_eq!(scan(&env, &mut Vec::new()).unwrap(), vec![first]);
    }

    #[test]
    fn concurrent_first_opens_converge_on_one_plot_and_session_binding() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let env = env.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    let plot = ensure_provisional(&env, "nopal").unwrap();
                    bind_session(&env, &plot.plot_id, "nopal-work", Some("%4")).unwrap()
                })
            })
            .collect();
        let plots: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert!(plots.iter().all(|plot| plot.plot_id == plots[0].plot_id));
        let stored = scan(&env, &mut Vec::new()).unwrap();
        assert_eq!(stored.len(), 1);
        assert_eq!(stored[0].sessions.len(), 1);
        assert_eq!(stored[0].sessions[0].host_session, "nopal-work");
    }

    #[test]
    fn ensure_recovers_when_the_plot_write_outlives_the_context_write() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));

        let first = ensure_provisional(&env, "nopal").unwrap();
        fs::remove_file(context_path(&env, "nopal")).unwrap();
        let recovered = ensure_provisional(&env, "nopal").unwrap();

        assert_eq!(recovered, first);
        assert_eq!(scan(&env, &mut Vec::new()).unwrap(), vec![first]);
    }

    #[test]
    fn binding_one_host_session_is_idempotent_and_selects_it() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = ensure_provisional(&env, "nopal").unwrap();

        let first = bind_session(&env, &plot.plot_id, "nopal-plot", Some("%7")).unwrap();
        let second = bind_session(&env, &plot.plot_id, "nopal-plot", Some("%8")).unwrap();

        assert_eq!(first.sessions.len(), 1);
        assert_eq!(second.sessions.len(), 1);
        assert_eq!(first.sessions[0].session_id, second.sessions[0].session_id);
        assert_eq!(second.sessions[0].host_pane.as_deref(), Some("%8"));
        assert_eq!(
            second.selected_session_id.as_deref(),
            Some(second.sessions[0].session_id.as_str())
        );
    }

    #[test]
    fn scanning_missing_state_is_read_only() {
        let dir = tempfile::tempdir().unwrap();
        let state = dir.path().join("absent");
        let env = PlotEnv::discover(Some(&state));

        assert!(scan(&env, &mut Vec::new()).unwrap().is_empty());
        assert!(!state.exists());
    }

    #[test]
    fn scan_skips_a_document_whose_identity_does_not_match_its_path() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = ensure_provisional(&env, "nopal").unwrap();
        let wrong_path = plot_path(&env, "plot-wrong");
        write_json(&wrong_path, &plot).unwrap();

        let mut warnings = Vec::new();
        let plots = scan(&env, &mut warnings).unwrap();

        assert_eq!(plots, vec![plot]);
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, Code::PlotSnapshotInvalid);
        assert!(warnings[0].message.contains("invalid Plot document"));
    }

    #[test]
    fn concurrent_establishment_converges_on_one_snapshot_and_session() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = ensure_provisional(&env, "nopal").unwrap();
        let input = establishment_input();

        let first_env = env.clone();
        let first_plot_id = plot.plot_id.clone();
        let first_input = input.clone();
        let first = std::thread::spawn(move || {
            establish(&first_env, &first_plot_id, first_input)
                .unwrap()
                .1
        });
        let second_env = env.clone();
        let second_plot_id = plot.plot_id.clone();
        let second =
            std::thread::spawn(move || establish(&second_env, &second_plot_id, input).unwrap().1);
        let outcomes = [first.join().unwrap(), second.join().unwrap()];

        assert!(outcomes.contains(&ApplyOutcome::Established));
        assert!(outcomes.contains(&ApplyOutcome::Unchanged));
        let stored = load_plot(&env, &plot.plot_id).unwrap();
        assert_eq!(stored.repositories.len(), 1);
        assert_eq!(stored.workspaces.len(), 1);
        assert_eq!(stored.sessions.len(), 1);
    }

    #[test]
    fn execution_acceptance_and_observation_survive_store_reconstruction() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = establish_plot(&env);

        record_execution_acceptance(&env, &plot.plot_id, accepted_execution("run-1")).unwrap();
        record_execution_observation(
            &env,
            &plot.plot_id,
            ObservationInput {
                repo_id: "repository-1".to_owned(),
                run_id: "run-1".to_owned(),
                status: "completed".to_owned(),
                event_cursor: "rondo.core/v1:4".to_owned(),
                evidence: vec![ExecutionEvidencePointer {
                    artifact_kind: "delivery_artifact".to_owned(),
                    uri: "rondo-run://run-1/artifacts/delivery.json".to_owned(),
                }],
            },
        )
        .unwrap();

        let reconstructed = PlotEnv::discover(Some(dir.path()));
        let stored = load_plot(&reconstructed, &plot.plot_id).unwrap();
        assert_eq!(stored.sessions, plot.sessions);
        assert_eq!(stored.executions.len(), 1);
        assert_eq!(stored.executions[0].run_id, "run-1");
        assert_eq!(stored.executions[0].outcome.as_deref(), Some("completed"));
        assert_eq!(stored.executions[0].event_cursor, "rondo.core/v1:4");
        assert_eq!(stored.executions[0].evidence.len(), 1);

        let field = crate::field_store::field_status(
            dir.path(),
            Some(dir.path()),
            None,
            true,
            crate::field::DEFAULT_STALE_AFTER_HOURS,
        )
        .unwrap();
        assert_eq!(field.plots, vec![stored]);
    }

    #[test]
    fn concurrent_exact_execution_acceptance_converges_on_one_record() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = establish_plot(&env);
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let env = env.clone();
                let plot_id = plot.plot_id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    record_execution_acceptance(&env, &plot_id, accepted_execution("run-1"))
                        .unwrap()
                        .1
                })
            })
            .collect();
        let outcomes: Vec<_> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();

        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| **outcome == plot_execution::ApplyOutcome::Added)
                .count(),
            1
        );
        let stored = load_plot(&env, &plot.plot_id).unwrap();
        assert_eq!(stored.executions.len(), 1);
    }

    #[test]
    fn exact_replay_after_terminal_observation_reuses_the_durable_execution() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = establish_plot(&env);
        record_execution_acceptance(&env, &plot.plot_id, accepted_execution("run-1")).unwrap();
        record_execution_observation(
            &env,
            &plot.plot_id,
            ObservationInput {
                repo_id: "repository-1".to_owned(),
                run_id: "run-1".to_owned(),
                status: "completed".to_owned(),
                event_cursor: "rondo.core/v1:4".to_owned(),
                evidence: Vec::new(),
            },
        )
        .unwrap();
        let terminal = load_plot(&env, &plot.plot_id).unwrap();

        let (replayed, outcome) =
            record_execution_acceptance(&env, &plot.plot_id, accepted_execution("run-1")).unwrap();

        assert_eq!(outcome, plot_execution::ApplyOutcome::Unchanged);
        assert_eq!(replayed, terminal);
        assert_eq!(replayed.executions.len(), 1);
        assert_eq!(replayed.executions[0].outcome.as_deref(), Some("completed"));
        assert_eq!(replayed.executions[0].event_cursor, "rondo.core/v1:4");
    }

    #[test]
    fn concurrent_observations_preserve_the_high_water_cursor_and_evidence_union() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = establish_plot(&env);
        record_execution_acceptance(&env, &plot.plot_id, accepted_execution("run-1")).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(4));
        let handles: Vec<_> = (1..=4)
            .map(|position| {
                let env = env.clone();
                let plot_id = plot.plot_id.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    record_execution_observation(
                        &env,
                        &plot_id,
                        ObservationInput {
                            repo_id: "repository-1".to_owned(),
                            run_id: "run-1".to_owned(),
                            status: "running".to_owned(),
                            event_cursor: format!("rondo.core/v1:{position}"),
                            evidence: vec![ExecutionEvidencePointer {
                                artifact_kind: format!("artifact-{position}"),
                                uri: format!("rondo-run://run-1/artifacts/{position}"),
                            }],
                        },
                    )
                    .unwrap();
                })
            })
            .collect();
        for handle in handles {
            handle.join().unwrap();
        }

        let stored = load_plot(&env, &plot.plot_id).unwrap();
        assert_eq!(stored.executions.len(), 1);
        assert_eq!(stored.executions[0].event_cursor, "rondo.core/v1:4");
        assert_eq!(stored.executions[0].evidence.len(), 4);
    }

    #[test]
    fn rejected_observation_leaves_the_durable_plot_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = establish_plot(&env);
        record_execution_acceptance(&env, &plot.plot_id, accepted_execution("run-1")).unwrap();
        let path = plot_path(&env, &plot.plot_id);
        let before = fs::read(&path).unwrap();

        let result = record_execution_observation(
            &env,
            &plot.plot_id,
            ObservationInput {
                repo_id: "repository-1".to_owned(),
                run_id: "run-foreign".to_owned(),
                status: "running".to_owned(),
                event_cursor: "rondo.core/v1:0".to_owned(),
                evidence: Vec::new(),
            },
        );

        assert!(matches!(
            result,
            Err(ExecutionStoreError::Domain(
                ExecutionError::ExecutionNotFound
            ))
        ));
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn rejected_acceptance_identity_conflict_leaves_the_durable_plot_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = establish_plot(&env);
        record_execution_acceptance(&env, &plot.plot_id, accepted_execution("run-1")).unwrap();
        let path = plot_path(&env, &plot.plot_id);
        let before = fs::read(&path).unwrap();
        let mut conflict = accepted_execution("run-1");
        conflict.manifest_sha256 = "b".repeat(64);

        let result = record_execution_acceptance(&env, &plot.plot_id, conflict);

        assert!(matches!(
            result,
            Err(ExecutionStoreError::Domain(
                ExecutionError::IdentityConflict
            ))
        ));
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn rejected_terminal_regression_leaves_the_durable_plot_byte_identical() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = establish_plot(&env);
        record_execution_acceptance(&env, &plot.plot_id, accepted_execution("run-1")).unwrap();
        record_execution_observation(
            &env,
            &plot.plot_id,
            ObservationInput {
                repo_id: "repository-1".to_owned(),
                run_id: "run-1".to_owned(),
                status: "completed".to_owned(),
                event_cursor: "rondo.core/v1:4".to_owned(),
                evidence: Vec::new(),
            },
        )
        .unwrap();
        let path = plot_path(&env, &plot.plot_id);
        let before = fs::read(&path).unwrap();

        let result = record_execution_observation(
            &env,
            &plot.plot_id,
            ObservationInput {
                repo_id: "repository-1".to_owned(),
                run_id: "run-1".to_owned(),
                status: "running".to_owned(),
                event_cursor: "rondo.core/v1:5".to_owned(),
                evidence: Vec::new(),
            },
        );

        assert!(matches!(
            result,
            Err(ExecutionStoreError::Domain(
                ExecutionError::TerminalConflict
            ))
        ));
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn scan_degrades_a_plot_with_duplicate_execution_identity() {
        let dir = tempfile::tempdir().unwrap();
        let env = PlotEnv::discover(Some(dir.path()));
        let plot = establish_plot(&env);
        let mut stored =
            record_execution_acceptance(&env, &plot.plot_id, accepted_execution("run-1"))
                .unwrap()
                .0;
        stored.executions.push(stored.executions[0].clone());
        write_json(&plot_path(&env, &plot.plot_id), &stored).unwrap();

        let mut warnings = Vec::new();
        assert!(scan(&env, &mut warnings).unwrap().is_empty());
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].code, Code::PlotSnapshotInvalid);
        assert!(warnings[0].message.contains("execution identity conflicts"));
    }
}
