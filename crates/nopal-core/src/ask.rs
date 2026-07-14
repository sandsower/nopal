//! `nopal.ask/v1`: the durable `ask` decision lifecycle - the pure half.
//!
//! An `ask` policy decision (`nopal.policy/v1`, `Decision::Ask`) does not
//! resolve inside the session that raised it any more. Instead it is
//! persisted with enough context to decide from another process, moves through
//! a small closed state machine, and fails closed: only an explicit approval
//! becomes an `allow`, and expiry (or never being resolved) is always a
//! `deny`. Surfaces raise and resolve; the core owns the states.
//!
//! Everything here is values in / values out. The clock, filesystem, locking,
//! and run-ledger event emission live in `ask_store`.

use ledger_json::Value;
use nopal_ledger_json as ledger_json;
use serde::Serialize;

use crate::run_ledger::{self as ledger_core, HINT_LIMIT, TEXT_LIMIT};

pub const ASK_KIND: &str = "nopal.ask/v1";
pub const SCHEMA_VERSION: u64 = 1;

/// Default time-to-live for a raised ask, in seconds. Unattended (AFK) callers
/// should always pass a positive ttl so an unanswered ask expires to `deny`
/// rather than blocking forever; interactive callers may pass `0` to disable
/// auto-expiry (still safe: an unresolved ask stays `pending` = `deny`).
pub const DEFAULT_TTL_SECONDS: i64 = 3600;

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

/// The closed ask lattice. `pending` is the only non-terminal state; the other
/// three are terminal. The effective policy decision is fail-closed: only
/// `approved` yields `allow`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AskState {
    Pending,
    Approved,
    Denied,
    Expired,
}

impl AskState {
    pub const ALL: [AskState; 4] = [
        AskState::Pending,
        AskState::Approved,
        AskState::Denied,
        AskState::Expired,
    ];

    pub fn parse(s: &str) -> Option<AskState> {
        match s {
            "pending" => Some(AskState::Pending),
            "approved" => Some(AskState::Approved),
            "denied" => Some(AskState::Denied),
            "expired" => Some(AskState::Expired),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            AskState::Pending => "pending",
            AskState::Approved => "approved",
            AskState::Denied => "denied",
            AskState::Expired => "expired",
        }
    }

    pub fn is_terminal(self) -> bool {
        self != AskState::Pending
    }

    /// Fail-closed mapping to a `nopal.policy/v1` decision string. Only an
    /// explicit approval is an `allow`; pending, denied, and expired all deny.
    pub fn effective_decision(self) -> &'static str {
        match self {
            AskState::Approved => "allow",
            AskState::Pending | AskState::Denied | AskState::Expired => "deny",
        }
    }

    /// Whether a blocked caller may proceed. True only for `approved`.
    pub fn is_allow(self) -> bool {
        self == AskState::Approved
    }
}

/// A resolution verdict supplied by a human or agent from another process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Resolution {
    Approve,
    Deny,
}

impl Resolution {
    pub fn parse(s: &str) -> Option<Resolution> {
        match s {
            "approve" => Some(Resolution::Approve),
            "deny" => Some(Resolution::Deny),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Resolution::Approve => "approve",
            Resolution::Deny => "deny",
        }
    }

    /// The terminal state this verdict drives a pending ask into.
    pub fn resolved_state(self) -> AskState {
        match self {
            Resolution::Approve => AskState::Approved,
            Resolution::Deny => AskState::Denied,
        }
    }
}

// ---------------------------------------------------------------------------
// Ids and expiry
// ---------------------------------------------------------------------------

/// An ask id is one path-safe segment starting alphanumeric - the same rule
/// the run ledger uses for run ids, so a traversal-shaped id can never be
/// joined into the state dir search path.
pub fn ask_id_valid(value: &str) -> bool {
    ledger_core::identifier_valid(value)
}

pub fn new_ask_id(stamp: &str, token_hex: &str) -> String {
    format!("{stamp}-{token_hex}")
}

/// Whether `expires_at` has passed as of `now`. Both are `iso_utc` strings
/// (fixed width, `+00:00` offset), so lexicographic order is chronological
/// order. An empty/absent `expires_at` never expires.
pub fn is_past_expiry(now_iso: &str, expires_at: Option<&str>) -> bool {
    match expires_at {
        Some(expires) if !expires.is_empty() => now_iso >= expires,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Document builder
// ---------------------------------------------------------------------------

pub struct AskContext<'a> {
    pub ask_id: &'a str,
    pub repo: &'a str,
    pub repo_hash: &'a str,
    pub session_id: &'a str,
    pub run_id: Option<&'a str>,
    pub flow: Option<&'a str>,
    pub mode: &'a str,
    pub action: &'a str,
    pub rule: Option<&'a str>,
    pub classes: &'a [String],
    pub reason: &'a str,
    pub evidence: Option<&'a str>,
    pub created_at: &'a str,
    pub expires_at: Option<&'a str>,
}

/// The initial `ask.json` document. Untrusted free-text context (`reason`,
/// `evidence`) is redacted here, once, at the point it enters - the same
/// field-at-entry discipline the run ledger uses. The store deliberately does
/// not re-redact the whole document on later writes (`redact_text` is not
/// idempotent, so a second pass would corrupt an already-redacted span).
pub fn new_ask_doc(ctx: &AskContext) -> Value {
    let classes: Vec<Value> = ctx
        .classes
        .iter()
        .map(|c| Value::String(c.clone()))
        .collect();
    ledger_json::json!({
        "kind": ASK_KIND,
        "schema_version": SCHEMA_VERSION,
        "ask_id": ctx.ask_id,
        "repo": ctx.repo,
        "repo_hash": ctx.repo_hash,
        "session_id": ctx.session_id,
        "run_id": opt_str(ctx.run_id),
        "flow": opt_str(ctx.flow),
        "state": AskState::Pending.as_str(),
        "mode": ctx.mode,
        "action": ctx.action,
        "rule": opt_str(ctx.rule),
        "classes": classes,
        "reason": ledger_core::redact_text(ctx.reason, TEXT_LIMIT),
        "evidence": ctx
            .evidence
            .filter(|e| !e.is_empty())
            .map(|e| Value::String(ledger_core::redact_text(e, TEXT_LIMIT)))
            .unwrap_or(Value::Null),
        "resolution": Value::Null,
        "created_at": ctx.created_at,
        "updated_at": ctx.created_at,
        "expires_at": opt_str(ctx.expires_at),
    })
}

/// The `resolution` sub-document written when a pending ask is resolved. The
/// note is redacted with the tighter hint limit.
pub fn resolution_doc(
    resolution: Resolution,
    by: &str,
    note: Option<&str>,
    now_iso: &str,
) -> Value {
    ledger_json::json!({
        "decision": resolution.as_str(),
        "by": ledger_core::redact_text(by, HINT_LIMIT),
        "at": now_iso,
        "note": note
            .filter(|n| !n.is_empty())
            .map(|n| Value::String(ledger_core::redact_text(n, HINT_LIMIT)))
            .unwrap_or(Value::Null),
    })
}

fn opt_str(value: Option<&str>) -> Value {
    match value.filter(|s| !s.is_empty()) {
        Some(s) => Value::String(s.to_owned()),
        None => Value::Null,
    }
}

/// Read the `state` field of a persisted ask, defaulting to `pending` for a
/// document missing or carrying an unknown state (fail-closed: an unreadable
/// state is treated as not-yet-approved).
pub fn state_of(doc: &Value) -> AskState {
    doc.get("state")
        .and_then(Value::as_str)
        .and_then(AskState::parse)
        .unwrap_or(AskState::Pending)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_round_trip_and_fail_closed() {
        for state in AskState::ALL {
            assert_eq!(AskState::parse(state.as_str()), Some(state));
        }
        assert_eq!(AskState::parse("bogus"), None);
        assert!(AskState::Approved.is_allow());
        assert_eq!(AskState::Approved.effective_decision(), "allow");
        for denying in [AskState::Pending, AskState::Denied, AskState::Expired] {
            assert!(!denying.is_allow(), "{denying:?} must not allow");
            assert_eq!(denying.effective_decision(), "deny");
        }
        assert!(!AskState::Pending.is_terminal());
        assert!(AskState::Expired.is_terminal());
    }

    #[test]
    fn resolution_maps_to_terminal_state() {
        assert_eq!(Resolution::parse("approve"), Some(Resolution::Approve));
        assert_eq!(Resolution::parse("deny"), Some(Resolution::Deny));
        assert_eq!(Resolution::parse("maybe"), None);
        assert_eq!(Resolution::Approve.resolved_state(), AskState::Approved);
        assert_eq!(Resolution::Deny.resolved_state(), AskState::Denied);
    }

    #[test]
    fn expiry_is_lexicographic_on_iso() {
        let now = "2026-07-06T12:00:00+00:00";
        assert!(is_past_expiry(now, Some("2026-07-06T11:59:59+00:00")));
        assert!(!is_past_expiry(now, Some("2026-07-06T12:00:01+00:00")));
        // Boundary: equal instants count as expired (fail-closed).
        assert!(is_past_expiry(now, Some(now)));
        // No deadline never expires.
        assert!(!is_past_expiry(now, None));
        assert!(!is_past_expiry(now, Some("")));
    }

    #[test]
    fn ask_ids_follow_the_run_id_segment_rule() {
        assert!(ask_id_valid("20260706T120000Z-a1b2c3"));
        assert!(!ask_id_valid("../escape"));
        assert!(!ask_id_valid("has/slash"));
        assert_eq!(
            new_ask_id("20260706T120000Z", "a1b2c3"),
            "20260706T120000Z-a1b2c3"
        );
    }

    #[test]
    fn new_ask_doc_has_shape_and_redacts_context() {
        let doc = new_ask_doc(&AskContext {
            ask_id: "a1",
            repo: "/tmp/repo",
            repo_hash: "hash0000",
            session_id: "sess-1",
            run_id: Some("r1"),
            flow: Some("kickoff"),
            mode: "unattended_auto",
            action: "git.push",
            rule: Some("allow-push"),
            classes: &["git_remote".to_owned(), "secret_bearing".to_owned()],
            reason: "needs a push; TOKEN=leakme",
            evidence: Some("see run r1"),
            created_at: "2026-07-06T12:00:00+00:00",
            expires_at: Some("2026-07-06T13:00:00+00:00"),
        });
        assert_eq!(doc["kind"], ASK_KIND);
        assert_eq!(doc["schema_version"], 1);
        assert_eq!(doc["state"], "pending");
        assert_eq!(doc["run_id"], "r1");
        assert_eq!(doc["action"], "git.push");
        assert_eq!(doc["rule"], "allow-push");
        assert_eq!(doc["classes"][1], "secret_bearing");
        assert_eq!(doc["reason"], "needs a push; TOKEN=[REDACTED]");
        assert_eq!(doc["resolution"], Value::Null);
        assert_eq!(state_of(&doc), AskState::Pending);
    }

    #[test]
    fn absent_optionals_are_null() {
        let doc = new_ask_doc(&AskContext {
            ask_id: "a1",
            repo: "/tmp/repo",
            repo_hash: "hash0000",
            session_id: "sess-1",
            run_id: None,
            flow: None,
            mode: "manual",
            action: "fs.write",
            rule: None,
            classes: &[],
            reason: "why",
            evidence: None,
            created_at: "2026-07-06T12:00:00+00:00",
            expires_at: None,
        });
        assert_eq!(doc["run_id"], Value::Null);
        assert_eq!(doc["flow"], Value::Null);
        assert_eq!(doc["rule"], Value::Null);
        assert_eq!(doc["evidence"], Value::Null);
        assert_eq!(doc["expires_at"], Value::Null);
    }

    #[test]
    fn resolution_doc_redacts_by_and_note() {
        let doc = resolution_doc(
            Resolution::Approve,
            "vic",
            Some("ok; secret=hunter2"),
            "2026-07-06T12:05:00+00:00",
        );
        assert_eq!(doc["decision"], "approve");
        assert_eq!(doc["by"], "vic");
        assert_eq!(doc["note"], "ok; secret=[REDACTED]");
    }
}
