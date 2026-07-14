//! `rondo.core/v1` run-event feed adapter.
//!
//! Tails registered runs through the CLI transport `mix rondo.run_events
//! --repo-id R --run-id X [--cursor C]`, resuming from the returned
//! `next_event_cursor` each poll. Events follow the schema in
//! `conformance/execution/schemas/rondo-core-run-events-v1.schema.json`:
//! rondo.service.status_changed, rondo.run.status_changed, and
//! rondo.run.evidence_recorded. Runs render from these structured events
//! and evidence pointers, never from log tails.
//!
//! Field-wide run discovery belongs to `nopal field`; this adapter tails only runs
//! registered with `--rondo-run repo_id:run_id`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use crate::feeds::{Feed, run_json_command, str_field};
use crate::state::{FeedEvent, RunEventRow};

pub const SOURCE: &str = "rondo";

/// One run registered for tailing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunSpec {
    pub repo_id: String,
    pub run_id: String,
}

impl RunSpec {
    /// Parse a `--rondo-run repo_id:run_id` flag value.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (repo_id, run_id) = text
            .split_once(':')
            .ok_or_else(|| format!("expected repo_id:run_id, got {text:?}"))?;
        if repo_id.is_empty() || run_id.is_empty() {
            return Err(format!("expected repo_id:run_id, got {text:?}"));
        }
        Ok(Self {
            repo_id: repo_id.to_owned(),
            run_id: run_id.to_owned(),
        })
    }

    fn key(&self) -> String {
        format!("rondo:{}/{}", self.repo_id, self.run_id)
    }
}

pub struct RondoFeed {
    /// Directory containing rondo's mix.exs; `mix` runs with this cwd.
    rondo_dir: PathBuf,
    specs: Vec<RunSpec>,
    cursors: HashMap<String, String>,
    /// Prefix `mise exec -- mix` when mise manages the elixir toolchain.
    use_mise: bool,
}

impl RondoFeed {
    pub fn new(rondo_dir: PathBuf, specs: Vec<RunSpec>) -> Self {
        let use_mise = std::process::Command::new("mise")
            .arg("--version")
            .output()
            .map(|out| out.status.success())
            .unwrap_or(false);
        Self {
            rondo_dir,
            specs,
            cursors: HashMap::new(),
            use_mise,
        }
    }

    fn mix_argv(&self) -> Vec<String> {
        if self.use_mise {
            vec![
                "mise".to_owned(),
                "exec".to_owned(),
                "--".to_owned(),
                "mix".to_owned(),
            ]
        } else {
            vec!["mix".to_owned()]
        }
    }
}

impl Feed for RondoFeed {
    fn name(&self) -> &'static str {
        SOURCE
    }

    fn interval(&self) -> Duration {
        // Every poll boots a BEAM VM (mix task); keep it coarse. A
        // persistent transport is the v1.1 upgrade if tighter tails matter.
        Duration::from_secs(10)
    }

    fn poll(&mut self) -> Result<Vec<FeedEvent>, String> {
        if !self.rondo_dir.join("mix.exs").exists() {
            return Err(format!(
                "no mix.exs under {} (set --rondo-dir)",
                self.rondo_dir.display()
            ));
        }
        let mut out = Vec::new();
        for spec in &self.specs {
            let key = spec.key();
            let mut argv = self.mix_argv();
            argv.extend([
                "rondo.run_events".to_owned(),
                "--repo-id".to_owned(),
                spec.repo_id.clone(),
                "--run-id".to_owned(),
                spec.run_id.clone(),
            ]);
            if let Some(cursor) = self.cursors.get(&key) {
                argv.push("--cursor".to_owned());
                argv.push(cursor.clone());
            }
            let value = run_json_command(&argv, Some(&self.rondo_dir))?;
            let page = parse_run_events(&value)?;
            self.cursors.insert(key.clone(), page.next_event_cursor);
            if !page.events.is_empty() || !page.evidence.is_empty() || page.status.is_some() {
                out.push(FeedEvent::RondoRun {
                    key,
                    repo_id: spec.repo_id.clone(),
                    run_id: spec.run_id.clone(),
                    status: page.status,
                    events: page.events,
                    evidence: page.evidence,
                });
            }
        }
        Ok(out)
    }
}

/// One parsed `run.events` response page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventPage {
    pub events: Vec<RunEventRow>,
    /// Last rondo.run.status_changed status in the page, if any.
    pub status: Option<String>,
    /// Evidence pointers recorded in the page.
    pub evidence: Vec<(String, String)>,
    pub next_event_cursor: String,
}

/// Parse a rondo.core/v1 `run.events` response.
pub fn parse_run_events(value: &serde_json::Value) -> Result<EventPage, String> {
    if let Some(error) = value.get("error") {
        return Err(format!("rondo.run_events error: {error}"));
    }
    let next_event_cursor = str_field(value, "next_event_cursor");
    if next_event_cursor.is_empty() {
        return Err("run.events response has no next_event_cursor".to_owned());
    }
    let raw_events = value
        .get("events")
        .and_then(|v| v.as_array())
        .ok_or_else(|| "run.events response has no events array".to_owned())?;
    let mut events = Vec::new();
    let mut status = None;
    let mut evidence = Vec::new();
    for event in raw_events {
        let kind = str_field(event, "type");
        let detail = match kind.as_str() {
            "rondo.run.status_changed" => {
                let value = str_field(event, "status");
                status = Some(value.clone());
                value
            }
            "rondo.run.evidence_recorded" => {
                let pointer = (str_field(event, "artifact_kind"), str_field(event, "uri"));
                let detail = format!("{} {}", pointer.0, pointer.1);
                evidence.push(pointer);
                detail
            }
            "rondo.service.status_changed" => {
                format!(
                    "{} {}",
                    str_field(event, "service_id"),
                    str_field(event, "status")
                )
            }
            _ => String::new(),
        };
        events.push(RunEventRow {
            sequence: event.get("sequence").and_then(|v| v.as_u64()).unwrap_or(0),
            timestamp: str_field(event, "timestamp"),
            kind,
            detail,
        });
    }
    Ok(EventPage {
        events,
        status,
        evidence,
        next_event_cursor,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixture from conformance/execution/fixtures/run-events-resume.json.
    const FIXTURE: &str = r#"{
      "events": [
        {
          "type": "rondo.run.status_changed",
          "sequence": 3,
          "repo_id": "sample-repo",
          "run_id": "RUN-sample-0001",
          "status": "completed",
          "timestamp": "2026-05-10T15:30:40Z",
          "namespace": { "repo_id": "sample-repo", "run_id": "RUN-sample-0001" }
        },
        {
          "type": "rondo.run.evidence_recorded",
          "sequence": 4,
          "repo_id": "sample-repo",
          "run_id": "RUN-sample-0001",
          "artifact_kind": "agent_events",
          "uri": "rondo-run://RUN-sample-0001/artifacts/agent-events.ndjson",
          "timestamp": "2026-05-10T15:30:40Z",
          "namespace": { "repo_id": "sample-repo", "run_id": "RUN-sample-0001" }
        }
      ],
      "next_event_cursor": "rondo.core/v1:5"
    }"#;

    #[test]
    fn parses_conformance_fixture_page() {
        let value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let page = parse_run_events(&value).unwrap();
        assert_eq!(page.events.len(), 2);
        assert_eq!(page.status.as_deref(), Some("completed"));
        assert_eq!(page.next_event_cursor, "rondo.core/v1:5");
        assert_eq!(
            page.evidence,
            vec![(
                "agent_events".to_owned(),
                "rondo-run://RUN-sample-0001/artifacts/agent-events.ndjson".to_owned()
            )]
        );
        assert_eq!(page.events[0].sequence, 3);
        assert_eq!(page.events[0].detail, "completed");
    }

    #[test]
    fn error_payload_degrades() {
        let value = serde_json::json!({"error": ":run_not_found"});
        assert!(parse_run_events(&value).is_err());
    }

    #[test]
    fn run_spec_parses_and_rejects() {
        assert_eq!(
            RunSpec::parse("nopal:RUN-1").unwrap(),
            RunSpec {
                repo_id: "nopal".to_owned(),
                run_id: "RUN-1".to_owned()
            }
        );
        assert!(RunSpec::parse("no-colon").is_err());
        assert!(RunSpec::parse(":x").is_err());
    }
}
