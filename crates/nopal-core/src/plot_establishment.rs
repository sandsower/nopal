//! Deterministic Plot Establishment and Workspace binding semantics.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::plot::{
    FrozenWorkflow, PlotDocument, PlotEstablishment, PlotSession, RepositorySnapshot,
    SESSION_PROTOCOL_TRANSPORT_UNIX, SessionProtocolEndpoint, WorkspaceSnapshot,
};
use crate::process_artifact;
use crate::roots::RootDocument;
use crate::workflow;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EstablishmentInput {
    pub event: String,
    pub repository: RepositorySnapshot,
    pub workspace: WorkspaceSnapshot,
    pub effective_workflow: FrozenWorkflow,
    pub host_session: String,
    pub host_pane: Option<String>,
    #[serde(default)]
    pub protocol: Option<SessionProtocolEndpoint>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Established,
    Extended,
    Unchanged,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EstablishmentError {
    EventNotAllowed {
        event: String,
    },
    WorkflowSourceMismatch,
    WorkspaceConflict {
        workspace_id: String,
    },
    SessionWorkspaceConflict {
        session_id: String,
        current_workspace: String,
        requested_workspace: String,
    },
    ProtocolInvalid {
        field: &'static str,
    },
}

impl fmt::Display for EstablishmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EventNotAllowed { event } => {
                write!(formatter, "Establishment event {event:?} is not configured")
            }
            Self::WorkflowSourceMismatch => write!(
                formatter,
                "the effective Workflow must come from the primary Repository"
            ),
            Self::WorkspaceConflict { workspace_id } => write!(
                formatter,
                "Workspace {workspace_id:?} conflicts with its frozen snapshot"
            ),
            Self::SessionWorkspaceConflict {
                session_id,
                current_workspace,
                requested_workspace,
            } => write!(
                formatter,
                "Session {session_id:?} is bound to Workspace {current_workspace:?}, not {requested_workspace:?}"
            ),
            Self::ProtocolInvalid { field } => {
                write!(
                    formatter,
                    "the structured Session protocol has invalid {field}"
                )
            }
        }
    }
}

impl std::error::Error for EstablishmentError {}

#[derive(Debug)]
pub enum ResolveError {
    Io(io::Error),
    SnapshotInvalid { diagnostics: Vec<String> },
    WorkflowMissing,
    EventNotAllowed { event: String },
}

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::SnapshotInvalid { diagnostics } => {
                write!(
                    formatter,
                    "Repository configuration is invalid: {}",
                    diagnostics.join("; ")
                )
            }
            Self::WorkflowMissing => write!(
                formatter,
                "Repository configuration has no effective Workflow to establish"
            ),
            Self::EventNotAllowed { event } => {
                write!(formatter, "Establishment event {event:?} is not configured")
            }
        }
    }
}

impl std::error::Error for ResolveError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            _ => None,
        }
    }
}

impl From<io::Error> for ResolveError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub fn resolve_input(
    workspace: &Path,
    event: &str,
    host_session: &str,
    host_pane: Option<&str>,
) -> Result<EstablishmentInput, ResolveError> {
    resolve_input_with_workflow(workspace, event, host_session, host_pane, None)
}

pub fn resolve_contribution_input(
    workspace: &Path,
    event: &str,
    host_session: &str,
    host_pane: Option<&str>,
    effective_workflow: FrozenWorkflow,
) -> Result<EstablishmentInput, ResolveError> {
    resolve_input_with_workflow(
        workspace,
        event,
        host_session,
        host_pane,
        Some(effective_workflow),
    )
}

fn resolve_input_with_workflow(
    workspace: &Path,
    event: &str,
    host_session: &str,
    host_pane: Option<&str>,
    frozen_workflow: Option<FrozenWorkflow>,
) -> Result<EstablishmentInput, ResolveError> {
    let workspace_root = std::fs::canonicalize(workspace)?;
    let configuration_root = crate::discover::project_root(&workspace_root);
    let artifact = process_artifact::build(&configuration_root)?;
    if !artifact.ok() {
        return Err(ResolveError::SnapshotInvalid {
            diagnostics: artifact
                .diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.clone())
                .collect(),
        });
    }
    let candidate_workflow = artifact.modules.get("workflow").cloned();
    if frozen_workflow.is_none() {
        let workflow_value = candidate_workflow
            .as_ref()
            .ok_or(ResolveError::WorkflowMissing)?;
        if !workflow::establishment_events(workflow_value).contains(&event) {
            return Err(ResolveError::EventNotAllowed {
                event: event.to_owned(),
            });
        }
    }

    let common_dir = PathBuf::from(git_required(
        &configuration_root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )?);
    let common_dir = std::fs::canonicalize(common_dir)?;
    let repository_root = common_dir
        .file_name()
        .filter(|name| *name == ".git")
        .and_then(|_| common_dir.parent())
        .map_or_else(|| configuration_root.clone(), Path::to_path_buf);
    let repository_id = format!(
        "repository-{}",
        &hex_digest(common_dir.to_string_lossy().as_bytes())[..16]
    );
    let workspace_id = format!(
        "workspace-{}",
        &hex_digest(format!("{repository_id}\0{}", workspace_root.to_string_lossy()).as_bytes())
            [..16]
    );
    let revision = git_optional(&configuration_root, &["rev-parse", "HEAD"]);
    let artifact_json = process_artifact::artifact_json(&artifact)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let roots = artifact
        .modules
        .get("roots")
        .cloned()
        .map(serde_json::from_value::<RootDocument>)
        .transpose()
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?
        .map_or_else(Vec::new, |document| document.roots);
    let mut gate_ids: Vec<String> = artifact
        .modules
        .get("gates")
        .and_then(|value| value.get("gates"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|gate| gate.get("id").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
        .collect();
    gate_ids.sort();
    let effective_workflow = match frozen_workflow {
        Some(workflow) => workflow,
        None => FrozenWorkflow {
            source_repository_id: repository_id.clone(),
            source_hash: source_hash(&artifact, "workflow").ok_or(ResolveError::WorkflowMissing)?,
            value: candidate_workflow.ok_or(ResolveError::WorkflowMissing)?,
        },
    };
    let policy_hash = source_hash(&artifact, "policy");

    Ok(EstablishmentInput {
        event: event.to_owned(),
        repository: RepositorySnapshot {
            repository_id: repository_id.clone(),
            root: repository_root.to_string_lossy().into_owned(),
            configuration_root: configuration_root.to_string_lossy().into_owned(),
            revision: revision.clone(),
            process_artifact_hash: hex_digest(artifact_json.as_bytes()),
            roots,
            gate_ids,
            policy_hash,
        },
        workspace: WorkspaceSnapshot {
            workspace_id,
            repository_id: repository_id.clone(),
            root: workspace_root.to_string_lossy().into_owned(),
            revision,
            kind: if workspace_root == repository_root {
                "primary".to_owned()
            } else {
                "worktree".to_owned()
            },
        },
        effective_workflow,
        host_session: host_session.to_owned(),
        host_pane: host_pane.map(str::to_owned),
        protocol: None,
    })
}

fn source_hash(artifact: &process_artifact::ProcessArtifact, role: &str) -> Option<String> {
    artifact
        .sources
        .iter()
        .find(|source| source.role == role)
        .and_then(|source| source.hash.clone())
}

fn git_required(root: &Path, args: &[&str]) -> io::Result<String> {
    let output = Command::new("git").args(args).current_dir(root).output()?;
    if !output.status.success() {
        return Err(io::Error::other("cannot resolve Git Repository identity"));
    }
    let value = String::from_utf8(output.stdout)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    Ok(value.trim().to_owned())
}

fn git_optional(root: &Path, args: &[&str]) -> Option<String> {
    git_required(root, args)
        .ok()
        .filter(|value| !value.is_empty())
}

pub fn apply(
    plot: &mut PlotDocument,
    input: EstablishmentInput,
    now: &str,
) -> Result<ApplyOutcome, EstablishmentError> {
    if let Some(protocol) = &input.protocol {
        validate_protocol_endpoint(protocol)?;
    }
    let first_establishment = plot.establishment.is_none();
    let effective_workflow = plot
        .establishment
        .as_ref()
        .map_or(&input.effective_workflow, |establishment| {
            &establishment.effective_workflow
        });
    if !workflow::establishment_events(&effective_workflow.value).contains(&input.event.as_str()) {
        return Err(EstablishmentError::EventNotAllowed { event: input.event });
    }
    if first_establishment
        && input.effective_workflow.source_repository_id != input.repository.repository_id
    {
        return Err(EstablishmentError::WorkflowSourceMismatch);
    }
    if let Some(existing) = plot
        .workspaces
        .iter()
        .find(|workspace| workspace.workspace_id == input.workspace.workspace_id)
        && !same_workspace_identity(existing, &input.workspace)
    {
        return Err(EstablishmentError::WorkspaceConflict {
            workspace_id: input.workspace.workspace_id,
        });
    }
    if let Some(session) = plot
        .sessions
        .iter()
        .find(|session| session.host_session == input.host_session)
        && let Some(current_workspace) = &session.workspace
        && current_workspace != &input.workspace.workspace_id
    {
        return Err(EstablishmentError::SessionWorkspaceConflict {
            session_id: session.session_id.clone(),
            current_workspace: current_workspace.clone(),
            requested_workspace: input.workspace.workspace_id,
        });
    }

    let request_fingerprint = fingerprint(&input);
    if plot.establishment.as_ref().is_some_and(|establishment| {
        establishment
            .applied_requests
            .contains(&request_fingerprint)
    }) {
        return Ok(ApplyOutcome::Unchanged);
    }

    let mut changed = false;
    if first_establishment {
        plot.establishment = Some(PlotEstablishment {
            event: input.event.clone(),
            primary_repository_id: input.repository.repository_id.clone(),
            effective_workflow: input.effective_workflow.clone(),
            applied_requests: Vec::new(),
            established_at: now.to_owned(),
        });
        plot.provisional = false;
        changed = true;
    }
    if !plot
        .repositories
        .iter()
        .any(|repository| repository.repository_id == input.repository.repository_id)
    {
        plot.repositories.push(input.repository.clone());
        plot.repositories
            .sort_by(|left, right| left.repository_id.cmp(&right.repository_id));
        changed = true;
    }
    if !plot
        .workspaces
        .iter()
        .any(|workspace| workspace.workspace_id == input.workspace.workspace_id)
    {
        plot.workspaces.push(input.workspace.clone());
        plot.workspaces
            .sort_by(|left, right| left.workspace_id.cmp(&right.workspace_id));
        changed = true;
    }

    let selected_session_id = if let Some(session) = plot
        .sessions
        .iter_mut()
        .find(|session| session.host_session == input.host_session)
    {
        let mut session_changed = false;
        if session.workspace.is_none() {
            session.workspace = Some(input.workspace.workspace_id.clone());
            changed = true;
            session_changed = true;
        }
        if session.host_pane != input.host_pane {
            session.host_pane = input.host_pane.clone();
            changed = true;
            session_changed = true;
        }
        if let Some(protocol) = &input.protocol
            && session.protocol.as_ref() != Some(protocol)
        {
            session.protocol = Some(protocol.clone());
            changed = true;
            session_changed = true;
        }
        if session_changed {
            session.updated_at = now.to_owned();
        }
        session.session_id.clone()
    } else {
        let session_id = stable_session_id(&plot.plot_id, &input.host_session);
        plot.sessions.push(PlotSession {
            session_id: session_id.clone(),
            mode: "interactive".to_owned(),
            host: "pi".to_owned(),
            host_session: input.host_session.clone(),
            host_pane: input.host_pane.clone(),
            state: "active".to_owned(),
            protocol: input.protocol.clone(),
            workspace: Some(input.workspace.workspace_id.clone()),
            created_at: now.to_owned(),
            updated_at: now.to_owned(),
        });
        changed = true;
        session_id
    };
    plot.selected_session_id = Some(selected_session_id);
    if !changed {
        return Ok(ApplyOutcome::Unchanged);
    }
    if let Some(establishment) = &mut plot.establishment {
        establishment.applied_requests.push(request_fingerprint);
    }
    plot.updated_at = now.to_owned();
    Ok(if first_establishment {
        ApplyOutcome::Established
    } else {
        ApplyOutcome::Extended
    })
}

fn validate_protocol_endpoint(
    protocol: &SessionProtocolEndpoint,
) -> Result<(), EstablishmentError> {
    for (field, value) in [
        ("kind", protocol.kind.as_str()),
        ("address", protocol.address.as_str()),
        ("state", protocol.state.as_str()),
    ] {
        if value.trim().is_empty() || value.chars().any(char::is_control) {
            return Err(EstablishmentError::ProtocolInvalid { field });
        }
    }
    if protocol.transport != SESSION_PROTOCOL_TRANSPORT_UNIX {
        return Err(EstablishmentError::ProtocolInvalid { field: "transport" });
    }
    Ok(())
}

fn same_workspace_identity(left: &WorkspaceSnapshot, right: &WorkspaceSnapshot) -> bool {
    left.workspace_id == right.workspace_id
        && left.repository_id == right.repository_id
        && left.root == right.root
        && left.kind == right.kind
}

fn fingerprint(input: &EstablishmentInput) -> String {
    let value = serde_json::json!({
        "event": input.event,
        "repository": input.repository,
        "workspace": input.workspace,
        "host_session": input.host_session,
        "host_pane": input.host_pane,
        "protocol": input.protocol,
    });
    let bytes = serde_json::to_vec(&value).unwrap_or_default();
    hex_digest(&bytes)
}

fn stable_session_id(plot_id: &str, host_session: &str) -> String {
    let digest = hex_digest(format!("{plot_id}\0{host_session}").as_bytes());
    format!("session-{}", &digest[..16])
}

fn hex_digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plot::{
        FrozenWorkflow, PlotDocument, RepositorySnapshot, SessionProtocolEndpoint,
        WorkspaceSnapshot,
    };
    use crate::roots::{ProofRequirement, RootDeclaration};
    use std::fs;
    use std::process::Command;

    fn plot() -> PlotDocument {
        serde_json::from_value(serde_json::json!({
            "kind": "nopal.plot/v1",
            "plot_id": "plot-1",
            "title": "New Plot",
            "provisional": true,
            "progress": "planned",
            "conditions": [],
            "seed": {"source": "field_open", "text": "keep me"},
            "intent": "",
            "sessions": [{
                "session_id": "session-1",
                "mode": "interactive",
                "host": "pi",
                "host_session": "nopal-work",
                "host_pane": "%4",
                "state": "active",
                "workspace": null,
                "created_at": "t0",
                "updated_at": "t0"
            }],
            "selected_session_id": "session-1",
            "created_at": "t0",
            "updated_at": "t0"
        }))
        .unwrap()
    }

    fn input(workspace_id: &str, host_session: &str) -> EstablishmentInput {
        EstablishmentInput {
            event: "kickoff_context_ready".to_owned(),
            repository: RepositorySnapshot {
                repository_id: "repository-1".to_owned(),
                root: "/repo".to_owned(),
                configuration_root: "/repo".to_owned(),
                revision: Some("abc".to_owned()),
                process_artifact_hash: "artifact-1".to_owned(),
                roots: vec![RootDeclaration {
                    id: "quality".to_owned(),
                    statement: "Quality remains green".to_owned(),
                    proof_requirements: vec![ProofRequirement {
                        id: "pre-pr".to_owned(),
                        stage: "pre_pr".to_owned(),
                        required: true,
                        gates: vec!["test".to_owned()],
                        on_missing: "block".to_owned(),
                        on_failure: "block".to_owned(),
                    }],
                }],
                gate_ids: vec!["test".to_owned()],
                policy_hash: Some("policy-1".to_owned()),
            },
            workspace: WorkspaceSnapshot {
                workspace_id: workspace_id.to_owned(),
                repository_id: "repository-1".to_owned(),
                root: format!("/repo/{workspace_id}"),
                revision: Some("abc".to_owned()),
                kind: "worktree".to_owned(),
            },
            effective_workflow: FrozenWorkflow {
                source_repository_id: "repository-1".to_owned(),
                source_hash: "workflow-1".to_owned(),
                value: serde_json::json!({
                    "version": "nopal.workflow/v1",
                    "establishment": {"events": ["kickoff_context_ready"]}
                }),
            },
            host_session: host_session.to_owned(),
            host_pane: Some("%4".to_owned()),
            protocol: None,
        }
    }

    fn protocol(address: &str, state: &str) -> SessionProtocolEndpoint {
        SessionProtocolEndpoint {
            kind: "nopal.session/v2".to_owned(),
            transport: "unix".to_owned(),
            address: address.to_owned(),
            state: state.to_owned(),
        }
    }

    #[test]
    fn establishment_preserves_plot_seed_and_session_history() {
        let mut plot = plot();
        let before_id = plot.plot_id.clone();
        let before_seed = plot.seed.clone();
        let outcome = apply(&mut plot, input("workspace-1", "nopal-work"), "t1").unwrap();

        assert_eq!(outcome, ApplyOutcome::Established);
        assert_eq!(plot.plot_id, before_id);
        assert_eq!(plot.seed, before_seed);
        assert!(!plot.provisional);
        assert_eq!(plot.sessions.len(), 1);
        assert_eq!(plot.sessions[0].session_id, "session-1");
        assert_eq!(plot.sessions[0].workspace.as_deref(), Some("workspace-1"));
        assert_eq!(plot.repositories.len(), 1);
        assert_eq!(plot.workspaces.len(), 1);
    }

    #[test]
    fn exact_replay_is_idempotent() {
        let mut plot = plot();
        let request = input("workspace-1", "nopal-work");
        apply(&mut plot, request.clone(), "t1").unwrap();
        let established = plot.clone();

        let outcome = apply(&mut plot, request, "t2").unwrap();

        assert_eq!(outcome, ApplyOutcome::Unchanged);
        assert_eq!(plot, established);
    }

    #[test]
    fn protocol_endpoint_binds_to_the_selected_session_and_updates_idempotently() {
        let mut plot = plot();
        let mut first = input("workspace-1", "nopal-work");
        first.protocol = Some(protocol("/tmp/nopal-session-1.sock", "starting"));

        assert_eq!(
            apply(&mut plot, first, "t1").unwrap(),
            ApplyOutcome::Established
        );
        assert_eq!(
            plot.sessions[0].protocol.as_ref(),
            Some(&protocol("/tmp/nopal-session-1.sock", "starting"))
        );

        let mut ready = input("workspace-1", "nopal-work");
        ready.protocol = Some(protocol("/tmp/nopal-session-1.sock", "ready"));
        assert_eq!(
            apply(&mut plot, ready.clone(), "t2").unwrap(),
            ApplyOutcome::Extended
        );
        assert_eq!(
            plot.sessions[0].protocol.as_ref(),
            Some(&protocol("/tmp/nopal-session-1.sock", "ready"))
        );
        assert_eq!(
            plot.sessions[0]
                .protocol
                .as_ref()
                .expect("bound protocol")
                .kind,
            "nopal.session/v2"
        );
        assert_eq!(plot.sessions[0].updated_at, "t2");

        let established = plot.clone();
        assert_eq!(
            apply(&mut plot, ready, "t3").unwrap(),
            ApplyOutcome::Unchanged
        );
        assert_eq!(plot, established);
    }

    #[test]
    fn protocol_endpoint_rejects_unsafe_fields_before_mutating_the_plot() {
        let cases = [
            ("kind", "", "unix", "/tmp/session.sock", "ready"),
            ("kind", "   ", "unix", "/tmp/session.sock", "ready"),
            ("kind", "bad\nkind", "unix", "/tmp/session.sock", "ready"),
            (
                "transport",
                "nopal.session/v2",
                "tcp",
                "/tmp/session.sock",
                "ready",
            ),
            ("address", "nopal.session/v2", "unix", "\t", "ready"),
            ("address", "nopal.session/v2", "unix", "bad\0path", "ready"),
            (
                "state",
                "nopal.session/v2",
                "unix",
                "/tmp/session.sock",
                " ",
            ),
            (
                "state",
                "nopal.session/v2",
                "unix",
                "/tmp/session.sock",
                "bad\nstate",
            ),
        ];

        for (field, kind, transport, address, state) in cases {
            let mut plot = plot();
            let original = plot.clone();
            let mut request = input("workspace-1", "nopal-work");
            request.protocol = Some(SessionProtocolEndpoint {
                kind: kind.to_owned(),
                transport: transport.to_owned(),
                address: address.to_owned(),
                state: state.to_owned(),
            });

            assert_eq!(
                apply(&mut plot, request, "t1"),
                Err(EstablishmentError::ProtocolInvalid { field })
            );
            assert_eq!(plot, original, "invalid {field} must not mutate Plot state");
        }
    }

    #[test]
    fn protocol_endpoint_preserves_a_safe_future_kind() {
        let mut plot = plot();
        let mut request = input("workspace-1", "nopal-work");
        request.protocol = Some(SessionProtocolEndpoint {
            kind: "nopal.session/v99-preview".to_owned(),
            transport: "unix".to_owned(),
            address: "/tmp/future-session.sock".to_owned(),
            state: "starting".to_owned(),
        });

        apply(&mut plot, request, "t1").expect("safe future kind is additive");

        assert_eq!(
            plot.sessions[0]
                .protocol
                .as_ref()
                .expect("protocol persisted")
                .kind,
            "nopal.session/v99-preview"
        );
    }

    #[test]
    fn protocol_absence_preserves_an_existing_endpoint() {
        let mut plot = plot();
        let mut first = input("workspace-1", "nopal-work");
        first.protocol = Some(protocol("/tmp/nopal-session-1.sock", "ready"));
        apply(&mut plot, first, "t1").unwrap();

        let without_protocol = input("workspace-1", "nopal-work");
        assert_eq!(
            apply(&mut plot, without_protocol, "t2").unwrap(),
            ApplyOutcome::Unchanged
        );
        assert_eq!(
            plot.sessions[0].protocol.as_ref(),
            Some(&protocol("/tmp/nopal-session-1.sock", "ready"))
        );
    }

    #[test]
    fn later_revision_keeps_the_frozen_workspace_snapshot_without_conflict() {
        let mut plot = plot();
        let first = input("workspace-1", "nopal-work");
        apply(&mut plot, first, "t1").unwrap();
        let frozen_repository = plot.repositories[0].clone();
        let frozen_workspace = plot.workspaces[0].clone();
        let mut later = input("workspace-1", "nopal-work");
        later.repository.revision = Some("def".to_owned());
        later.repository.process_artifact_hash = "artifact-2".to_owned();
        later.workspace.revision = Some("def".to_owned());

        let outcome = apply(&mut plot, later, "t2").unwrap();

        assert_eq!(outcome, ApplyOutcome::Unchanged);
        assert_eq!(plot.repositories[0], frozen_repository);
        assert_eq!(plot.workspaces[0], frozen_workspace);
    }

    #[test]
    fn a_workspace_identity_change_still_fails_closed() {
        let mut plot = plot();
        apply(&mut plot, input("workspace-1", "nopal-work"), "t1").unwrap();
        let mut conflicting = input("workspace-1", "nopal-other");
        conflicting.workspace.root = "/different".to_owned();

        let error = apply(&mut plot, conflicting, "t2").unwrap_err();

        assert!(matches!(
            error,
            EstablishmentError::WorkspaceConflict { .. }
        ));
    }

    #[test]
    fn a_session_cannot_move_between_workspaces() {
        let mut plot = plot();
        apply(&mut plot, input("workspace-1", "nopal-work"), "t1").unwrap();

        let error = apply(&mut plot, input("workspace-2", "nopal-work"), "t2").unwrap_err();

        assert!(matches!(
            error,
            EstablishmentError::SessionWorkspaceConflict { .. }
        ));
        assert_eq!(plot.sessions[0].workspace.as_deref(), Some("workspace-1"));
    }

    #[test]
    fn a_second_workspace_creates_a_second_session_under_one_plot() {
        let mut plot = plot();
        apply(&mut plot, input("workspace-1", "nopal-work"), "t1").unwrap();
        let workflow = plot
            .establishment
            .as_ref()
            .unwrap()
            .effective_workflow
            .clone();

        let outcome = apply(&mut plot, input("workspace-2", "nopal-other"), "t2").unwrap();

        assert_eq!(outcome, ApplyOutcome::Extended);
        assert_eq!(plot.sessions.len(), 2);
        assert_eq!(plot.workspaces.len(), 2);
        assert_eq!(plot.sessions[1].workspace.as_deref(), Some("workspace-2"));
        assert_eq!(
            plot.establishment.as_ref().unwrap().effective_workflow,
            workflow
        );
    }

    #[test]
    fn resolver_builds_authoritative_snapshots_from_repository_config() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(directory.path())
                .status()
                .unwrap()
                .success()
        );
        let nopal = directory.path().join(".nopal");
        fs::create_dir(&nopal).unwrap();
        fs::write(
            nopal.join("nopal.jsonc"),
            r#"{"version":"nopal.project/v1","profile":"minimal"}"#,
        )
        .unwrap();
        fs::write(
            nopal.join("workflow.jsonc"),
            r#"{
                "version":"nopal.workflow/v1",
                "establishment":{"events":["kickoff_context_ready"]}
            }"#,
        )
        .unwrap();
        fs::write(
            nopal.join("roots.jsonc"),
            r#"{
                "version":"nopal.roots/v1",
                "roots":[{
                    "id":"quality","statement":"Quality stays green",
                    "proof_requirements":[{
                        "id":"proof","stage":"pre_pr","required":true,
                        "gates":["test"],"on_missing":"block","on_failure":"block"
                    }]
                }]
            }"#,
        )
        .unwrap();
        fs::write(
            nopal.join("gates.jsonc"),
            r#"{"version":"nopal.gates/v1","gates":[{"id":"test","stage":"pre_pr","command":"cargo test"}]}"#,
        )
        .unwrap();

        let input = resolve_input(
            directory.path(),
            "kickoff_context_ready",
            "nopal-work",
            Some("%4"),
        )
        .unwrap();

        assert!(input.repository.repository_id.starts_with("repository-"));
        assert_eq!(input.repository.roots[0].id, "quality");
        assert_eq!(input.repository.gate_ids, ["test"]);
        assert_eq!(
            input.workspace.repository_id,
            input.repository.repository_id
        );
        assert_eq!(input.workspace.kind, "primary");
        assert_eq!(
            input.effective_workflow.source_repository_id,
            input.repository.repository_id
        );
    }

    #[test]
    fn resolver_rejects_an_unconfigured_establishment_event() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(directory.path())
                .status()
                .unwrap()
                .success()
        );
        let nopal = directory.path().join(".nopal");
        fs::create_dir(&nopal).unwrap();
        fs::write(
            nopal.join("nopal.jsonc"),
            r#"{"version":"nopal.project/v1","profile":"minimal"}"#,
        )
        .unwrap();
        fs::write(
            nopal.join("workflow.jsonc"),
            r#"{"version":"nopal.workflow/v1","establishment":{"events":["ready"]}}"#,
        )
        .unwrap();

        let error = resolve_input(directory.path(), "wrong", "nopal-work", None).unwrap_err();

        assert!(matches!(error, ResolveError::EventNotAllowed { .. }));
    }

    #[test]
    fn contribution_resolver_uses_frozen_workflow_without_requiring_a_competing_one() {
        let directory = tempfile::tempdir().unwrap();
        assert!(
            Command::new("git")
                .args(["init", "-q"])
                .current_dir(directory.path())
                .status()
                .unwrap()
                .success()
        );
        let nopal = directory.path().join(".nopal");
        fs::create_dir(&nopal).unwrap();
        fs::write(
            nopal.join("nopal.jsonc"),
            r#"{"version":"nopal.project/v1","profile":"minimal"}"#,
        )
        .unwrap();
        fs::write(
            nopal.join("roots.jsonc"),
            r#"{
                "version":"nopal.roots/v1",
                "roots":[{"id":"secondary","statement":"Secondary repository quality","proof_requirements":[]}]
            }"#,
        )
        .unwrap();
        let frozen = input("workspace-1", "nopal-work").effective_workflow;

        let contribution = resolve_contribution_input(
            directory.path(),
            "kickoff_context_ready",
            "secondary-session",
            Some("%9"),
            frozen.clone(),
        )
        .unwrap();

        assert_eq!(contribution.effective_workflow, frozen);
        assert_eq!(contribution.repository.roots[0].id, "secondary");
        assert_ne!(
            contribution.repository.repository_id,
            contribution.effective_workflow.source_repository_id
        );
    }
}
