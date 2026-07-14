//! Primary field feed over `nopal field inspect --json` (`nopal.field/v1`).
//!
//! One composed query supplies the AFK RUNS section and the ask queue:
//! per live run - placement, ledger state, latest gate attempts, pending
//! asks, and optional rondo.core/v1 facts; unbound asks arrive top-level.
//! The per-run `mix rondo.run_events` tail (feeds::rondo) stays only for
//! event-level streaming beyond the field snapshot.

use std::path::PathBuf;
use std::time::Duration;

use nopal_feed_client::field::{
    FieldEntry, FieldGate, FieldPlot, parse_field as parse_field_contract,
};

use crate::feeds::asks::parse_field_ask;
use crate::feeds::{Feed, run_json_command};
use crate::state::{
    AfkRun, Ask, FeedEvent, Plot, PlotEstablishment, PlotExecution, PlotExecutionEvidence,
    PlotProofRequirement, PlotRepository, PlotRoot, PlotSession, PlotWorkspace, RunSource,
    worktree_repo_tag,
};

pub const SOURCE: &str = "field";

pub type FieldData = (Vec<Plot>, Vec<AfkRun>, Vec<Ask>);

pub struct FieldFeed {
    nopal_bin: PathBuf,
    state_dir: Option<PathBuf>,
    /// Forwarded as `--rondo-events`; attaches rondo status/evidence.
    rondo_events: Option<PathBuf>,
}

impl FieldFeed {
    pub fn new(
        nopal_bin: PathBuf,
        state_dir: Option<PathBuf>,
        rondo_events: Option<PathBuf>,
    ) -> Self {
        Self {
            nopal_bin,
            state_dir,
            rondo_events,
        }
    }
}

impl Feed for FieldFeed {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(2)
    }

    fn poll(&mut self) -> Result<Vec<FeedEvent>, String> {
        let mut argv = vec![
            self.nopal_bin.to_string_lossy().into_owned(),
            "field".to_owned(),
            "inspect".to_owned(),
        ];
        if let Some(dir) = &self.state_dir {
            argv.push("--state-dir".to_owned());
            argv.push(dir.to_string_lossy().into_owned());
        }
        if let Some(feed) = &self.rondo_events {
            argv.push("--rondo-events".to_owned());
            argv.push(feed.to_string_lossy().into_owned());
        }
        argv.push("--json".to_owned());
        let value = run_json_command(&argv, None)?;
        let (plots, runs, asks) = parse_field(&value)?;
        Ok(vec![
            FeedEvent::Plots(plots),
            FeedEvent::LedgerRuns(runs),
            FeedEvent::Asks(asks),
        ])
    }
}

/// Parse a nopal.field/v1 envelope into AFK runs plus the full ask queue
/// (per-run asks and unbound asks combined).
pub fn parse_field(value: &serde_json::Value) -> Result<FieldData, String> {
    let snapshot = parse_field_contract(value)?;

    let plots = snapshot.plots.iter().map(parse_plot).collect();
    let mut runs = Vec::new();
    let mut asks = Vec::new();
    for entry in snapshot.entries {
        let run_id = entry.run_id.clone();
        // Placement gives the absolute repo path; nopal-* worktrees group
        // under their parent repo (branch/ticket carry the specifics).
        let repo = worktree_repo_tag(&entry.placement.repo);
        let ticket = Some(entry.ticket_id.clone())
            .filter(|id| !id.is_empty() && id != "none")
            .unwrap_or_default();
        let mut run = AfkRun {
            key: format!("ledger:{run_id}"),
            source: RunSource::Ledger,
            run_id,
            repo,
            status: entry.status.clone(),
            ticket,
            branch: entry.branch.clone(),
            updated_at: entry.updated_at.clone(),
            events: Vec::new(),
            evidence: Vec::new(),
            gates: parse_gates(&entry),
        };
        // Rondo facts attached by the field query (status wins; evidence
        // pointers merge into the detail pane).
        if let Some(rondo) = &entry.rondo {
            if let Some(status) = &rondo.status {
                run.status = status.clone();
            }
            run.evidence.extend(
                rondo
                    .evidence
                    .iter()
                    .map(|pointer| (pointer.artifact_kind.clone(), pointer.uri.clone())),
            );
        }
        asks.extend(entry.asks.iter().map(parse_field_ask));
        runs.push(run);
    }
    asks.extend(snapshot.asks_unbound.iter().map(parse_field_ask));
    Ok((plots, runs, asks))
}

fn parse_plot(plot: &FieldPlot) -> Plot {
    Plot {
        plot_id: plot.plot_id.clone(),
        title: plot.title.clone(),
        provisional: plot.provisional,
        progress: plot.progress.clone(),
        conditions: plot.conditions.clone(),
        seed_source: plot.seed.source.clone(),
        seed_text: plot.seed.text.clone(),
        intent: plot.intent.clone(),
        fruit_state: plot.fruit.state.clone(),
        executions: plot
            .executions
            .iter()
            .map(|execution| PlotExecution {
                service_id: execution.service_id.clone(),
                repo_id: execution.repo_id.clone(),
                run_id: execution.run_id.clone(),
                manifest_sha256: execution.manifest_sha256.clone(),
                status: execution.status.clone(),
                outcome: execution.outcome.clone(),
                event_cursor: execution.event_cursor.clone(),
                evidence: execution
                    .evidence
                    .iter()
                    .map(|pointer| PlotExecutionEvidence {
                        artifact_kind: pointer.artifact_kind.clone(),
                        uri: pointer.uri.clone(),
                    })
                    .collect(),
                created_at: execution.created_at.clone(),
                updated_at: execution.updated_at.clone(),
            })
            .collect(),
        sessions: plot
            .sessions
            .iter()
            .map(|session| PlotSession {
                session_id: session.session_id.clone(),
                mode: session.mode.clone(),
                host: session.host.clone(),
                host_session: session.host_session.clone(),
                host_pane: session.host_pane.clone(),
                state: session.state.clone(),
                workspace: session.workspace.clone(),
            })
            .collect(),
        selected_session_id: plot.selected_session_id.clone(),
        establishment: plot
            .establishment
            .as_ref()
            .map(|establishment| PlotEstablishment {
                event: establishment.event.clone(),
                primary_repository_id: establishment.primary_repository_id.clone(),
                workflow_source_repository_id: establishment
                    .effective_workflow
                    .source_repository_id
                    .clone(),
                workflow_source_hash: establishment.effective_workflow.source_hash.clone(),
            }),
        repositories: plot
            .repositories
            .iter()
            .map(|repository| PlotRepository {
                repository_id: repository.repository_id.clone(),
                root: repository.root.clone(),
                configuration_root: repository.configuration_root.clone(),
                revision: repository.revision.clone(),
                roots: repository
                    .roots
                    .iter()
                    .map(|root| PlotRoot {
                        id: root.id.clone(),
                        statement: root.statement.clone(),
                        proof_requirements: root
                            .proof_requirements
                            .iter()
                            .map(|proof| PlotProofRequirement {
                                id: proof.id.clone(),
                                stage: proof.stage.clone(),
                                required: proof.required,
                                gates: proof.gates.clone(),
                                on_missing: proof.on_missing.clone(),
                                on_failure: proof.on_failure.clone(),
                            })
                            .collect(),
                    })
                    .collect(),
                gate_ids: repository.gate_ids.clone(),
            })
            .collect(),
        workspaces: plot
            .workspaces
            .iter()
            .map(|workspace| PlotWorkspace {
                workspace_id: workspace.workspace_id.clone(),
                repository_id: workspace.repository_id.clone(),
                root: workspace.root.clone(),
                revision: workspace.revision.clone(),
                kind: workspace.kind.clone(),
            })
            .collect(),
    }
}

/// Latest gate attempts as `name(scope): status` summary rows.
fn parse_gates(entry: &FieldEntry) -> Vec<String> {
    entry.gates.iter().map(format_gate).collect()
}

fn format_gate(gate: &FieldGate) -> String {
    format!("{}({}): {}", gate.name, gate.scope, gate.status)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Trimmed capture of `nopal field inspect --json` on main (c4324d3).
    const FIXTURE: &str = r#"{
      "kind": "nopal.field/v1",
      "ok": true,
      "total": 1,
      "rondo_feed": { "status": "absent", "observed_runs": 0 },
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
          "repo_id": "repository-1",
          "run_id": "RUN-PLOT-1",
          "manifest_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
          "status": "completed",
          "outcome": "completed",
          "event_cursor": "rondo.core/v1:7",
          "evidence": [
            { "artifact_kind": "delivery_artifact", "uri": "rondo-run://RUN-PLOT-1/artifacts/delivery.json" }
          ],
          "created_at": "t",
          "updated_at": "t2"
        }],
        "sessions": [{
          "session_id": "session-1",
          "mode": "interactive",
          "host": "pi",
          "host_session": "nopal-work",
          "host_pane": "%4",
          "state": "active",
          "workspace": null,
          "created_at": "t",
          "updated_at": "t"
        }],
        "selected_session_id": "session-1",
        "repositories": [{
          "repository_id": "repository-1",
          "root": "/repo",
          "configuration_root": "/repo",
          "roots": [{
            "id": "dogfood-quality",
            "statement": "The complete flow is usable",
            "proof_requirements": [{
              "id": "full-gates",
              "stage": "pre_pr",
              "required": true,
              "gates": ["test", "clippy"],
              "on_missing": "block",
              "on_failure": "block"
            }]
          }],
          "gate_ids": ["test", "clippy"]
        }],
        "created_at": "t",
        "updated_at": "t"
      }],
      "entries": [
        {
          "run_id": "20260707T000433Z-fca3ad",
          "flow": "implement",
          "skill": "implement",
          "status": "running",
          "ticket_id": "TASK-15",
          "branch": "nopal/task-15",
          "started_at": "2026-07-07T00:04:33+00:00",
          "updated_at": "2026-07-07T00:04:33+00:00",
          "placement": {
            "repo": "/home/alex/projects/teotl",
            "repo_hash": "9030d801a642",
            "branch": "nopal/task-15",
            "run_dir": "/tmp/state/runs/implement/9030d801a642/20260707T000433Z-fca3ad",
            "flow": "implement"
          },
          "gates": [
            { "attempt": 1, "name": "tests", "scope": "repo", "status": "pass", "path": "/x" }
          ],
          "rondo": {
            "run_id": "RUN-1",
            "status": "completed",
            "evidence": [
              { "artifact_kind": "agent_events", "uri": "rondo-run://RUN-1/artifacts/a.ndjson" }
            ]
          },
          "asks": [
            {
              "action": "git.push", "ask_id": "a-bound", "reason": "bound ask",
              "repo": "/home/alex/projects/teotl", "session_id": "run-1",
              "state": "pending", "created_at": "t", "expires_at": "t2"
            }
          ]
        }
      ],
      "asks_unbound": [
        {
          "action": "net.fetch", "ask_id": "a-unbound", "reason": "unbound ask",
          "repo": "/home/alex/projects/rondo", "session_id": "seat-1",
          "state": "pending", "created_at": "t", "expires_at": "t2"
        }
      ],
      "diagnostics": []
    }"#;

    #[test]
    fn parses_captured_field_envelope() {
        let value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let (plots, runs, asks) = parse_field(&value).unwrap();
        assert_eq!(plots.len(), 1);
        assert_eq!(plots[0].plot_id, "plot-1");
        assert_eq!(plots[0].sessions[0].host_session, "nopal-work");
        assert_eq!(plots[0].fruit_state, "absent");
        assert_eq!(plots[0].executions.len(), 1);
        let execution = &plots[0].executions[0];
        assert_eq!(execution.service_id, "rondo-core");
        assert_eq!(execution.repo_id, "repository-1");
        assert_eq!(execution.run_id, "RUN-PLOT-1");
        assert_eq!(execution.manifest_sha256, "a".repeat(64));
        assert_eq!(execution.status, "completed");
        assert_eq!(execution.outcome.as_deref(), Some("completed"));
        assert_eq!(execution.event_cursor, "rondo.core/v1:7");
        assert_eq!(execution.created_at, "t");
        assert_eq!(execution.updated_at, "t2");
        assert_eq!(execution.evidence[0].artifact_kind, "delivery_artifact");
        assert_eq!(
            execution.evidence[0].uri,
            "rondo-run://RUN-PLOT-1/artifacts/delivery.json"
        );
        let root = &plots[0].repositories[0].roots[0];
        assert_eq!(root.statement, "The complete flow is usable");
        let proof = &root.proof_requirements[0];
        assert_eq!(proof.stage, "pre_pr");
        assert!(proof.required);
        assert_eq!(proof.gates, vec!["test", "clippy"]);
        assert_eq!(proof.on_missing, "block");
        assert_eq!(proof.on_failure, "block");
        assert_eq!(runs.len(), 1);
        let run = &runs[0];
        assert_eq!(run.key, "ledger:20260707T000433Z-fca3ad");
        assert_eq!(run.repo, "teotl");
        assert_eq!(run.ticket, "TASK-15");
        assert_eq!(run.gates, vec!["tests(repo): pass".to_owned()]);
        // Rondo facts win the status and contribute evidence.
        assert_eq!(run.status, "completed");
        assert_eq!(
            run.evidence,
            vec![(
                "agent_events".to_owned(),
                "rondo-run://RUN-1/artifacts/a.ndjson".to_owned()
            )]
        );
        // Bound and unbound asks combine into one queue.
        let ids: Vec<&str> = asks.iter().map(|ask| ask.ask_id.as_str()).collect();
        assert_eq!(ids, vec!["a-bound", "a-unbound"]);
        assert_eq!(asks[1].repo, "rondo");
    }

    #[test]
    fn rejects_foreign_kinds() {
        let value = serde_json::json!({"kind": "nopal.status/v1"});
        assert!(parse_field(&value).is_err());
    }
}
