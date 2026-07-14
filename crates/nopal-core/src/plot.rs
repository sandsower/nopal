//! `nopal.plot/v1`: durable Plot and Session facts owned by Nopal Core.

use serde::{Deserialize, Serialize};

use crate::roots::RootDeclaration;

pub const PLOT_KIND: &str = "nopal.plot/v1";
pub const SESSION_PROTOCOL_KIND: &str = "nopal.session/v3";
pub const SESSION_PROTOCOL_V2_KIND: &str = "nopal.session/v2";
pub const SESSION_PROTOCOL_TRANSPORT_UNIX: &str = "unix";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Seed {
    pub source: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionProtocolEndpoint {
    pub kind: String,
    pub transport: String,
    pub address: String,
    pub state: String,
}

impl SessionProtocolEndpoint {
    pub fn unix(address: impl Into<String>, state: impl Into<String>) -> Self {
        Self::unix_with_kind(SESSION_PROTOCOL_KIND, address, state)
    }

    pub fn unix_with_kind(
        kind: impl Into<String>,
        address: impl Into<String>,
        state: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            transport: SESSION_PROTOCOL_TRANSPORT_UNIX.to_owned(),
            address: address.into(),
            state: state.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlotSession {
    pub session_id: String,
    pub mode: String,
    pub host: String,
    pub host_session: String,
    pub host_pane: Option<String>,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocol: Option<SessionProtocolEndpoint>,
    pub workspace: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlotExecution {
    pub service_id: String,
    pub repo_id: String,
    pub run_id: String,
    pub manifest_sha256: String,
    pub status: String,
    pub outcome: Option<String>,
    pub event_cursor: String,
    #[serde(default)]
    pub evidence: Vec<ExecutionEvidencePointer>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionEvidencePointer {
    pub artifact_kind: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fruit {
    pub state: String,
}

impl Default for Fruit {
    fn default() -> Self {
        Self {
            state: "absent".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlotDocument {
    pub kind: String,
    pub plot_id: String,
    pub title: String,
    pub provisional: bool,
    pub progress: String,
    pub conditions: Vec<String>,
    pub seed: Seed,
    pub intent: String,
    #[serde(default)]
    pub fruit: Fruit,
    pub sessions: Vec<PlotSession>,
    pub selected_session_id: Option<String>,
    #[serde(default)]
    pub executions: Vec<PlotExecution>,
    #[serde(default)]
    pub establishment: Option<PlotEstablishment>,
    #[serde(default)]
    pub repositories: Vec<RepositorySnapshot>,
    #[serde(default)]
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlotEstablishment {
    pub event: String,
    pub primary_repository_id: String,
    pub effective_workflow: FrozenWorkflow,
    #[serde(default)]
    pub applied_requests: Vec<String>,
    pub established_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenWorkflow {
    pub source_repository_id: String,
    pub source_hash: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RepositorySnapshot {
    pub repository_id: String,
    pub root: String,
    pub configuration_root: String,
    pub revision: Option<String>,
    pub process_artifact_hash: String,
    #[serde(default)]
    pub roots: Vec<RootDeclaration>,
    #[serde(default)]
    pub gate_ids: Vec<String>,
    pub policy_hash: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    pub workspace_id: String,
    pub repository_id: String,
    pub root: String,
    pub revision: Option<String>,
    pub kind: String,
}

#[cfg(test)]
mod tests {
    use super::{SESSION_PROTOCOL_KIND, SessionProtocolEndpoint};

    #[test]
    fn unix_session_endpoint_defaults_to_the_durable_feed_capability() {
        let endpoint = SessionProtocolEndpoint::unix("/tmp/session.sock", "ready");

        assert_eq!(SESSION_PROTOCOL_KIND, "nopal.session/v3");
        assert_eq!(endpoint.kind, "nopal.session/v3");
        assert_eq!(endpoint.transport, "unix");
    }

    #[test]
    fn unix_session_endpoint_can_preserve_an_explicit_registered_kind() {
        let endpoint = SessionProtocolEndpoint::unix_with_kind(
            "nopal.session/v2",
            "/tmp/session.sock",
            "starting",
        );

        assert_eq!(endpoint.kind, "nopal.session/v2");
        assert_eq!(endpoint.address, "/tmp/session.sock");
        assert_eq!(endpoint.state, "starting");
    }
}
