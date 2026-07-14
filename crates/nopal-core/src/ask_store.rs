//! Ask store - the effectful half of `nopal.ask/v1`.
//!
//! Owns the durable state under `${state_dir}/asks/<repo_hash>/<ask_id>/`,
//! the per-ask `.lock` guard, id allocation, discovery, the lazy expiry
//! transition, and the run-ledger event emission that makes an ask's lifecycle
//! auditable per run. It reuses the run-ledger store's clock, randomness,
//! durable-write, locking, and redaction machinery verbatim so the two state
//! trees share one contract.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use ledger_json::Value;
use nopal_ledger_json as ledger_json;

use crate::ask::{self, AskContext, AskState, Resolution};
use crate::diagnostics::{Code, Diagnostic};
use crate::run_ledger as ledger_core;
use crate::run_ledger_store::{self as store, LedgerEnv, RunLock, StoreError};

const ASK_FILE: &str = "ask.json";

fn now_iso() -> String {
    ledger_core::iso_utc(store::epoch_now())
}

// ---------------------------------------------------------------------------
// Discovery and loading
// ---------------------------------------------------------------------------

/// Locate a single ask by id under this repo's ask root. A traversal-shaped id
/// is rejected before it can be joined into the search path.
pub fn find_ask_dir(env: &LedgerEnv, ask_id: &str) -> Result<PathBuf, StoreError> {
    if !ask::ask_id_valid(ask_id) {
        return Err(store::domain(
            Code::AskIdInvalid,
            ask_id,
            "invalid ask id: use a single path-safe segment [A-Za-z0-9_.-]",
        ));
    }
    let candidate = env.ask_root().join(ask_id);
    if candidate.join(ASK_FILE).is_file() {
        Ok(candidate)
    } else {
        Err(store::domain(
            Code::AskNotFound,
            ask_id,
            format!("ask not found: {ask_id}"),
        ))
    }
}

pub fn load_ask(ask_dir: &Path) -> Result<Value, StoreError> {
    let path = ask_dir.join(ASK_FILE);
    let text = fs::read_to_string(&path)?;
    ledger_json::from_str(&text).map_err(|err| {
        store::domain(
            Code::AskEntryInvalid,
            path.display().to_string(),
            format!("unreadable ask JSON: {err}"),
        )
    })
}

/// Persist the ask document. Untrusted free text (`reason`, `evidence`,
/// resolution `by`/`note`) is redacted once at its builder in `ask`, exactly
/// like the run ledger redacts each field as it enters - never a blanket
/// `redact_json` on every write, since `redact_text` is not idempotent
/// (re-running it over an already-redacted `KEY=[REDACTED]` span corrupts it).
fn write_ask(ask_dir: &Path, doc: &Value) -> Result<(), StoreError> {
    store::write_json_durable(&ask_dir.join(ASK_FILE), doc)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Run-ledger event emission
// ---------------------------------------------------------------------------

/// Append one ask lifecycle event to the backing run's ledger, when the ask is
/// backed by a run. Returns a warning diagnostic (never a hard failure) if the
/// referenced run cannot be found, so the ask operation itself still succeeds.
fn emit_run_event(
    env: &LedgerEnv,
    doc: &Value,
    event_type: &str,
    payload: Value,
) -> Result<Option<Diagnostic>, StoreError> {
    let Some(run_id) = doc.get("run_id").and_then(Value::as_str) else {
        return Ok(None);
    };
    if run_id.is_empty() {
        return Ok(None);
    }
    let flow = doc.get("flow").and_then(Value::as_str);
    let run_dir = match store::find_run_dir(env, run_id, flow) {
        Ok(dir) => dir,
        Err(StoreError::Io(err)) => return Err(StoreError::Io(err)),
        Err(StoreError::Domain(_)) => {
            return Ok(Some(Diagnostic::warning(
                Code::RunNotFound,
                run_id,
                format!(
                    "ask references run {run_id:?} but no such run ledger was found; ask recorded without a ledger event"
                ),
            )));
        }
    };
    store::append_event(&run_dir, event_type, &payload, None)?;
    Ok(None)
}

fn lifecycle_payload(doc: &Value, extra: Value) -> Value {
    let mut payload = ledger_json::json!({
        "ask_id": doc.get("ask_id").cloned().unwrap_or(Value::Null),
        "action": doc.get("action").cloned().unwrap_or(Value::Null),
        "mode": doc.get("mode").cloned().unwrap_or(Value::Null),
        "rule": doc.get("rule").cloned().unwrap_or(Value::Null),
        "state": doc.get("state").cloned().unwrap_or(Value::Null),
    });
    if let (Some(map), Value::Object(more)) = (payload.as_object_mut(), extra) {
        for (k, v) in more {
            map.insert(k, v);
        }
    }
    payload
}

// ---------------------------------------------------------------------------
// raise
// ---------------------------------------------------------------------------

pub struct RaiseArgs<'a> {
    pub session_id: &'a str,
    pub run_id: Option<&'a str>,
    pub flow: Option<&'a str>,
    pub mode: &'a str,
    pub action: &'a str,
    pub rule: Option<&'a str>,
    pub classes: &'a [String],
    pub reason: &'a str,
    pub evidence: Option<&'a str>,
    /// Non-positive means no auto-expiry (still fail-closed: stays pending).
    pub ttl_seconds: i64,
}

pub struct RaiseOutcome {
    pub ask_id: String,
    pub ask_dir: PathBuf,
    pub state: AskState,
    pub expires_at: Option<String>,
    pub warnings: Vec<Diagnostic>,
}

pub fn raise_ask(env: &LedgerEnv, args: &RaiseArgs) -> Result<RaiseOutcome, StoreError> {
    let root = env.ask_root();
    fs::create_dir_all(&root)?;

    let created_at = now_iso();
    let expires_at = (args.ttl_seconds > 0)
        .then(|| ledger_core::iso_utc(store::epoch_now().saturating_add(args.ttl_seconds)));

    let mut ask_id = ask::new_ask_id(&store::now_stamp(), &store::token_hex(3));
    let mut suffix = 1u32;
    let ask_dir = loop {
        let candidate = root.join(&ask_id);
        match fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                suffix += 1;
                ask_id = format!(
                    "{}-{suffix}",
                    ask::new_ask_id(&store::now_stamp(), &store::token_hex(3))
                );
            }
            Err(err) => return Err(err.into()),
        }
    };

    let repo_text = env.repo.display().to_string();
    let doc = ask::new_ask_doc(&AskContext {
        ask_id: &ask_id,
        repo: &repo_text,
        repo_hash: &env.repo_hash,
        session_id: args.session_id,
        run_id: args.run_id,
        flow: args.flow,
        mode: args.mode,
        action: args.action,
        rule: args.rule,
        classes: args.classes,
        reason: args.reason,
        evidence: args.evidence,
        created_at: &created_at,
        expires_at: expires_at.as_deref(),
    });

    let _lock = RunLock::acquire(&ask_dir)?;
    write_ask(&ask_dir, &doc)?;
    let warning = emit_run_event(
        env,
        &doc,
        "ask_raised",
        lifecycle_payload(
            &doc,
            ledger_json::json!({
                "session_id": args.session_id,
                "expires_at": expires_at.clone().map(Value::String).unwrap_or(Value::Null),
            }),
        ),
    )?;

    Ok(RaiseOutcome {
        ask_id,
        ask_dir,
        state: AskState::Pending,
        expires_at,
        warnings: warning.into_iter().collect(),
    })
}

// ---------------------------------------------------------------------------
// expiry
// ---------------------------------------------------------------------------

/// Materialize a lazily-observed expiry if the loaded ask is pending and past
/// its deadline. Assumes the ask lock is already held. Returns the (possibly
/// updated) doc and any ledger warning.
fn materialize_expiry_locked(
    env: &LedgerEnv,
    ask_dir: &Path,
    mut doc: Value,
    now: &str,
) -> Result<(Value, Option<Diagnostic>), StoreError> {
    let expires = doc
        .get("expires_at")
        .and_then(Value::as_str)
        .map(str::to_owned);
    if ask::state_of(&doc) != AskState::Pending || !ask::is_past_expiry(now, expires.as_deref()) {
        return Ok((doc, None));
    }
    if let Some(map) = doc.as_object_mut() {
        map.insert(
            "state".to_owned(),
            Value::String(AskState::Expired.as_str().to_owned()),
        );
        map.insert("updated_at".to_owned(), Value::String(now.to_owned()));
    }
    write_ask(ask_dir, &doc)?;
    let warning = emit_run_event(
        env,
        &doc,
        "ask_expired",
        lifecycle_payload(&doc, ledger_json::json!({"expired_at": now})),
    )?;
    Ok((doc, warning))
}

/// Load an ask, materializing an overdue expiry under its lock. This is the
/// read path every consumer uses so expiry is never silently deferred.
pub fn refresh_expiry(env: &LedgerEnv, ask_dir: &Path) -> Result<Value, StoreError> {
    let _lock = RunLock::acquire(ask_dir)?;
    let doc = load_ask(ask_dir)?;
    let (doc, _warning) = materialize_expiry_locked(env, ask_dir, doc, &now_iso())?;
    Ok(doc)
}

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

pub struct ResolveOutcome {
    pub doc: Value,
    pub warnings: Vec<Diagnostic>,
}

pub fn resolve_ask(
    env: &LedgerEnv,
    ask_id: &str,
    resolution: Resolution,
    by: &str,
    note: Option<&str>,
) -> Result<ResolveOutcome, StoreError> {
    let ask_dir = find_ask_dir(env, ask_id)?;
    let _lock = RunLock::acquire(&ask_dir)?;
    let now = now_iso();
    let (doc, _warning) = materialize_expiry_locked(env, &ask_dir, load_ask(&ask_dir)?, &now)?;

    match ask::state_of(&doc) {
        AskState::Pending => {}
        AskState::Expired => {
            return Err(store::domain(
                Code::AskExpired,
                ask_id,
                format!("ask {ask_id} has expired and fails closed (deny); it cannot be resolved"),
            ));
        }
        other => {
            return Err(store::domain(
                Code::AskAlreadyResolved,
                ask_id,
                format!(
                    "ask {ask_id} is already {} and cannot be resolved again",
                    other.as_str()
                ),
            ));
        }
    }

    let mut doc = doc;
    let new_state = resolution.resolved_state();
    if let Some(map) = doc.as_object_mut() {
        map.insert(
            "state".to_owned(),
            Value::String(new_state.as_str().to_owned()),
        );
        map.insert("updated_at".to_owned(), Value::String(now.clone()));
        map.insert(
            "resolution".to_owned(),
            ask::resolution_doc(resolution, by, note, &now),
        );
    }
    write_ask(&ask_dir, &doc)?;
    let warning = emit_run_event(
        env,
        &doc,
        "ask_resolved",
        lifecycle_payload(
            &doc,
            ledger_json::json!({
                "decision": resolution.as_str(),
                "by": ledger_core::redact_text(by, ledger_core::HINT_LIMIT),
            }),
        ),
    )?;

    Ok(ResolveOutcome {
        doc,
        warnings: warning.into_iter().collect(),
    })
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

pub struct AskListing {
    pub asks: Vec<Value>,
    pub warnings: Vec<Diagnostic>,
}

/// All asks for this repo (optionally filtered to one state), each refreshed
/// for expiry. Sorted by ask id for a deterministic surface.
pub fn list_asks(
    env: &LedgerEnv,
    state_filter: Option<AskState>,
) -> Result<AskListing, StoreError> {
    let root = env.ask_root();
    let mut asks = Vec::new();
    let mut warnings = Vec::new();
    if !root.is_dir() {
        return Ok(AskListing { asks, warnings });
    }
    let mut dirs: Vec<PathBuf> = fs::read_dir(&root)?
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| p.join(ASK_FILE).is_file())
        .collect();
    dirs.sort();
    for ask_dir in dirs {
        match refresh_expiry(env, &ask_dir) {
            Ok(doc) => {
                if state_filter.is_none_or(|wanted| ask::state_of(&doc) == wanted) {
                    asks.push(doc);
                }
            }
            Err(StoreError::Io(err)) => return Err(StoreError::Io(err)),
            Err(StoreError::Domain(diag)) => warnings.push(Diagnostic::warning(
                diag.code,
                diag.path,
                format!("skipping unreadable ask: {}", diag.message),
            )),
        }
    }
    Ok(AskListing { asks, warnings })
}

#[cfg(test)]
mod tests {
    // These unit tests deliberately do not pin the global clock env var:
    // cargo runs them on parallel threads, and mutating process env from
    // multiple threads is unsafe. To exercise expiry deterministically we
    // instead rewrite the stored deadline to a fixed past instant and let the
    // real clock observe it as overdue (the cross-process CLI tests pin the
    // clock per invocation, which is race-free).
    use super::*;
    use crate::run_ledger_store::InitArgs;

    const LONG_AGO: &str = "1970-01-01T00:00:01+00:00";

    fn temp_env() -> (tempfile::TempDir, LedgerEnv) {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let env = LedgerEnv {
            state_dir: dir.path().join("state"),
            repo: dir.path().join("repo"),
            repo_hash: "testhash0000".to_owned(),
        };
        (dir, env)
    }

    fn raise(env: &LedgerEnv, run_id: Option<&str>, ttl: i64) -> RaiseOutcome {
        raise_ask(
            env,
            &RaiseArgs {
                session_id: "sess-1",
                run_id,
                flow: run_id.map(|_| "kickoff"),
                mode: "unattended_auto",
                action: "git.push",
                rule: Some("ask-push"),
                classes: &["git_remote".to_owned()],
                reason: "needs a push",
                evidence: None,
                ttl_seconds: ttl,
            },
        )
        .unwrap_or_else(|_| panic!("raise"))
    }

    /// Force a persisted ask's deadline into the past so the next read expires
    /// it, without touching the process clock.
    fn backdate_deadline(ask_dir: &Path) {
        let mut doc = load_ask(ask_dir).unwrap_or_else(|_| panic!("load for backdate"));
        if let Some(map) = doc.as_object_mut() {
            map.insert("expires_at".to_owned(), Value::String(LONG_AGO.to_owned()));
        }
        store::write_json_durable(&ask_dir.join(ASK_FILE), &doc)
            .unwrap_or_else(|_| panic!("backdate write"));
    }

    #[test]
    fn raise_persists_pending_ask_discoverable_by_id() {
        let (_tmp, env) = temp_env();
        let out = raise(&env, None, 3600);
        assert_eq!(out.state, AskState::Pending);
        assert!(out.expires_at.is_some());
        let dir = find_ask_dir(&env, &out.ask_id).unwrap_or_else(|_| panic!("find"));
        let doc = load_ask(&dir).unwrap_or_else(|_| panic!("load"));
        assert_eq!(doc["state"], "pending");
        assert_eq!(doc["action"], "git.push");
        assert_eq!(doc["session_id"], "sess-1");
        assert_eq!(ask::state_of(&doc), AskState::Pending);
    }

    #[test]
    fn approve_then_double_resolve_is_rejected() {
        let (_tmp, env) = temp_env();
        let out = raise(&env, None, 3600);
        let resolved = resolve_ask(&env, &out.ask_id, Resolution::Approve, "vic", Some("ok"))
            .unwrap_or_else(|_| panic!("resolve"));
        assert_eq!(resolved.doc["state"], "approved");
        assert_eq!(resolved.doc["resolution"]["decision"], "approve");
        assert_eq!(resolved.doc["resolution"]["by"], "vic");
        match resolve_ask(&env, &out.ask_id, Resolution::Deny, "vic", None) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::AskAlreadyResolved),
            other => panic!("expected already-resolved, ok={}", other.is_ok()),
        }
    }

    #[test]
    fn expiry_fails_closed_and_blocks_resolution() {
        let (_tmp, env) = temp_env();
        let out = raise(&env, None, 60);
        let dir = find_ask_dir(&env, &out.ask_id).unwrap_or_else(|_| panic!("find"));
        backdate_deadline(&dir);
        // A read materializes the expiry.
        let doc = refresh_expiry(&env, &dir).unwrap_or_else(|_| panic!("refresh"));
        assert_eq!(doc["state"], "expired");
        assert_eq!(ask::state_of(&doc).effective_decision(), "deny");
        match resolve_ask(&env, &out.ask_id, Resolution::Approve, "vic", None) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::AskExpired),
            other => panic!("expected expired, ok={}", other.is_ok()),
        }
    }

    #[test]
    fn no_ttl_ask_never_expires_and_stays_deny_until_resolved() {
        let (_tmp, env) = temp_env();
        let out = raise(&env, None, 0);
        let dir = find_ask_dir(&env, &out.ask_id).unwrap_or_else(|_| panic!("find"));
        let doc = load_ask(&dir).unwrap_or_else(|_| panic!("load"));
        assert_eq!(doc["expires_at"], Value::Null);
        let refreshed = refresh_expiry(&env, &dir).unwrap_or_else(|_| panic!("refresh"));
        assert_eq!(refreshed["state"], "pending");
        assert_eq!(ask::state_of(&refreshed).effective_decision(), "deny");
    }

    #[test]
    fn list_filters_by_state_and_refreshes_expiry() {
        let (_tmp, env) = temp_env();
        let pending = raise(&env, None, 0);
        let to_expire = raise(&env, None, 60);
        resolve_ask(&env, &pending.ask_id, Resolution::Approve, "vic", None)
            .unwrap_or_else(|_| panic!("resolve"));
        backdate_deadline(
            &find_ask_dir(&env, &to_expire.ask_id).unwrap_or_else(|_| panic!("find")),
        );
        let approved =
            list_asks(&env, Some(AskState::Approved)).unwrap_or_else(|_| panic!("list approved"));
        assert_eq!(approved.asks.len(), 1);
        let expired =
            list_asks(&env, Some(AskState::Expired)).unwrap_or_else(|_| panic!("list expired"));
        assert_eq!(expired.asks.len(), 1);
        assert_eq!(expired.asks[0]["ask_id"], to_expire.ask_id.as_str());
        let all = list_asks(&env, None).unwrap_or_else(|_| panic!("list all"));
        assert_eq!(all.asks.len(), 2);
    }

    #[test]
    fn run_backed_ask_writes_ledger_events() {
        let (_tmp, env) = temp_env();
        let run = store::init_run(
            &env,
            &InitArgs {
                skill: "kickoff",
                flow: Some("kickoff"),
                ticket_id: "TASK-29",
                ticket_title: "Asks",
                ticket_url: "",
                branch: Some("feature/x"),
                run_id: Some("r1"),
            },
        )
        .unwrap_or_else(|_| panic!("init run"));
        let out = raise(&env, Some("r1"), 3600);
        assert!(out.warnings.is_empty());
        resolve_ask(&env, &out.ask_id, Resolution::Deny, "vic", Some("nope"))
            .unwrap_or_else(|_| panic!("resolve"));
        let events = fs::read_to_string(run.run_dir.join("events.jsonl")).unwrap_or_default();
        assert!(events.contains("\"type\": \"ask_raised\""), "{events}");
        assert!(events.contains("\"type\": \"ask_resolved\""), "{events}");
    }

    #[test]
    fn missing_run_reference_warns_but_ask_survives() {
        let (_tmp, env) = temp_env();
        let out = raise(&env, Some("ghost"), 3600);
        assert_eq!(out.warnings.len(), 1);
        assert_eq!(out.warnings[0].code, Code::RunNotFound);
        // The ask itself is still persisted and resolvable.
        let dir = find_ask_dir(&env, &out.ask_id).unwrap_or_else(|_| panic!("find"));
        assert!(dir.join(ASK_FILE).is_file());
    }
}
