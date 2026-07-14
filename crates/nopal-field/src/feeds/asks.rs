//! Ask resolution and nopal.ask/v1 object parsing.
//!
//! Listing now arrives through the composed `nopal field` query
//! (feeds::field); this module keeps the shared nopal.ask/v1 object
//! parser and the resolver that routes approve/deny keystrokes to
//! `nopal ask resolve`. The decision semantics live in Nopal Core; the field
//! only routes the operator's keystroke.

use std::path::PathBuf;

use crate::feeds::{run_json_command, str_field};
use crate::state::Ask;
use nopal_feed_client::field::FieldAsk;

/// Routes ask resolution to the `nopal ask` surface.
pub struct AskClient {
    nopal_bin: PathBuf,
    state_dir: Option<PathBuf>,
}

impl AskClient {
    pub fn new(nopal_bin: PathBuf, state_dir: Option<PathBuf>) -> Self {
        Self {
            nopal_bin,
            state_dir,
        }
    }

    /// Route an approve/deny keystroke to `nopal ask resolve`.
    pub fn resolve(&self, ask_id: &str, decision: &str, by: &str) -> Result<(), String> {
        let mut argv = vec![self.nopal_bin.to_string_lossy().into_owned(), "ask".into()];
        if let Some(dir) = &self.state_dir {
            argv.push("--state-dir".into());
            argv.push(dir.to_string_lossy().into_owned());
        }
        argv.extend(
            [
                "resolve",
                "--id",
                ask_id,
                "--decision",
                decision,
                "--by",
                by,
                "--json",
            ]
            .map(str::to_owned),
        );
        let value = run_json_command(&argv, None)?;
        if value.get("ok").and_then(|v| v.as_bool()) == Some(true) {
            Ok(())
        } else {
            let diagnostics = value
                .get("diagnostics")
                .map(|d| d.to_string())
                .unwrap_or_default();
            Err(format!("ask resolve failed: {diagnostics}"))
        }
    }
}

/// Parse one nopal.ask/v1 object (as embedded in field envelopes and ask
/// list reports) into a sidebar ask.
pub fn parse_ask(ask: &serde_json::Value) -> Ask {
    Ask {
        ask_id: str_field(ask, "ask_id"),
        action: str_field(ask, "action"),
        reason: str_field(ask, "reason"),
        session_id: str_field(ask, "session_id"),
        repo: short_repo(&str_field(ask, "repo")),
        state: str_field(ask, "state"),
        created_at: str_field(ask, "created_at"),
        expires_at: str_field(ask, "expires_at"),
    }
}

/// Convert the host-neutral field client shape into the field's sidebar
/// model. The contract parser lives outside the field so other clients do
/// not have to depend on UI state types.
pub fn parse_field_ask(ask: &FieldAsk) -> Ask {
    Ask {
        ask_id: ask.ask_id.clone(),
        action: ask.action.clone(),
        reason: ask.reason.clone(),
        session_id: ask.session_id.clone(),
        repo: short_repo(&ask.repo),
        state: ask.state.clone(),
        created_at: ask.created_at.clone(),
        expires_at: ask.expires_at.clone(),
    }
}

/// The ask store records the repo as an absolute path; the sidebar tag is
/// its basename.
fn short_repo(repo: &str) -> String {
    repo.rsplit('/')
        .next()
        .filter(|tail| !tail.is_empty())
        .unwrap_or(repo)
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captured nopal.ask/v1 object from `nopal ask list --json` (e55fe87).
    const FIXTURE: &str = r#"{
      "action": "git.push",
      "ask_id": "20260706T230943Z-ae3d97",
      "classes": [],
      "created_at": "2026-07-06T23:09:43+00:00",
      "evidence": null,
      "expires_at": "2026-07-07T00:09:43+00:00",
      "flow": null,
      "kind": "nopal.ask/v1",
      "mode": "nopal_tui",
      "reason": "push to main branch",
      "repo": "/home/alex/projects/teotl",
      "repo_hash": "9030d801a642",
      "resolution": null,
      "rule": null,
      "run_id": null,
      "schema_version": 1,
      "session_id": "seat-1",
      "state": "pending",
      "updated_at": "2026-07-06T23:09:43+00:00"
    }"#;

    #[test]
    fn parses_captured_ask_object() {
        let value: serde_json::Value = serde_json::from_str(FIXTURE).unwrap();
        let ask = parse_ask(&value);
        assert_eq!(ask.ask_id, "20260706T230943Z-ae3d97");
        assert_eq!(ask.action, "git.push");
        assert_eq!(ask.reason, "push to main branch");
        assert_eq!(ask.session_id, "seat-1");
        assert_eq!(ask.repo, "teotl");
        assert_eq!(ask.state, "pending");
    }
}
