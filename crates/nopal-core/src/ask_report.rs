//! Output envelopes for the `nopal ask` commands.
//!
//! Same contract as every other nopal command: one envelope per command, one
//! builder per output flavor, kinds `nopal.ask.<command>/v1`. Domain problems
//! (ask not found, already resolved, expired) come back as `ok: false` plus
//! diagnostics; hard IO stays `Err`. The `await` builder additionally polls
//! the store until the ask is terminal or a wall-clock timeout elapses - the
//! v1 poll-based resolution-routing mechanism.

use std::io;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use ledger_json::Value;
use nopal_ledger_json as ledger_json;
use serde::Serialize;

use crate::ask::{self, AskState, Resolution};
use crate::ask_store::{self as store, AskListing, RaiseArgs, ResolveOutcome};
use crate::diagnostics::Diagnostic;
use crate::run_ledger_store::{LedgerEnv, StoreError};
use crate::toon::{self, Value as Toon};

pub const ASK_RAISE_KIND: &str = "nopal.ask.raise/v1";
pub const ASK_LIST_KIND: &str = "nopal.ask.list/v1";
pub const ASK_SHOW_KIND: &str = "nopal.ask.show/v1";
pub const ASK_RESOLVE_KIND: &str = "nopal.ask.resolve/v1";
pub const ASK_AWAIT_KIND: &str = "nopal.ask.await/v1";

fn split(err: StoreError) -> io::Result<Vec<Diagnostic>> {
    match err {
        StoreError::Domain(diag) => Ok(vec![diag]),
        StoreError::Io(err) => Err(err),
    }
}

fn value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) if !s.is_empty() => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => "-".to_owned(),
    }
}

// ---------------------------------------------------------------------------
// raise
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RaiseReport {
    pub kind: &'static str,
    pub ok: bool,
    pub ask_id: Option<String>,
    pub state: Option<String>,
    pub effective_decision: Option<String>,
    pub expires_at: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn ask_raise(
    dir: &Path,
    state_dir: Option<&Path>,
    args: &RaiseArgs,
) -> io::Result<RaiseReport> {
    let env = LedgerEnv::discover(dir, state_dir);
    match store::raise_ask(&env, args) {
        Ok(out) => Ok(RaiseReport {
            kind: ASK_RAISE_KIND,
            ok: true,
            ask_id: Some(out.ask_id),
            state: Some(out.state.as_str().to_owned()),
            effective_decision: Some(out.state.effective_decision().to_owned()),
            expires_at: out.expires_at,
            diagnostics: out.warnings,
        }),
        Err(err) => Ok(RaiseReport {
            kind: ASK_RAISE_KIND,
            ok: false,
            ask_id: None,
            state: None,
            effective_decision: None,
            expires_at: None,
            diagnostics: split(err)?,
        }),
    }
}

pub fn raise_toon(report: &RaiseReport) -> String {
    toon::encode(&[
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        ("ask_id".into(), opt(&report.ask_id)),
        ("state".into(), opt(&report.state)),
        ("effective_decision".into(), opt(&report.effective_decision)),
        ("expires_at".into(), opt(&report.expires_at)),
        (
            "diagnostics".into(),
            crate::diagnostics::toon_table(&report.diagnostics),
        ),
    ])
}

// ---------------------------------------------------------------------------
// list
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ListReport {
    pub kind: &'static str,
    pub ok: bool,
    pub total: usize,
    pub asks: Vec<Value>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn ask_list(
    dir: &Path,
    state_dir: Option<&Path>,
    state_filter: Option<AskState>,
) -> io::Result<ListReport> {
    let env = LedgerEnv::discover(dir, state_dir);
    match store::list_asks(&env, state_filter) {
        Ok(AskListing { asks, warnings }) => Ok(ListReport {
            kind: ASK_LIST_KIND,
            ok: true,
            total: asks.len(),
            asks,
            diagnostics: warnings,
        }),
        Err(err) => Ok(ListReport {
            kind: ASK_LIST_KIND,
            ok: false,
            total: 0,
            asks: Vec::new(),
            diagnostics: split(err)?,
        }),
    }
}

fn ask_row(ask: &Value) -> Vec<Toon> {
    let state = ask::state_of(ask);
    vec![
        Toon::str(value_text(ask.get("ask_id"))),
        Toon::str(state.as_str()),
        Toon::str(state.effective_decision()),
        Toon::str(value_text(ask.get("action"))),
        Toon::str(value_text(ask.get("mode"))),
        Toon::str(value_text(ask.get("session_id"))),
        Toon::str(value_text(ask.get("run_id"))),
        Toon::str(value_text(ask.get("expires_at"))),
    ]
}

fn asks_table(asks: &[Value]) -> Toon {
    Toon::Table {
        fields: [
            "ask_id",
            "state",
            "decision",
            "action",
            "mode",
            "session",
            "run_id",
            "expires_at",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect(),
        rows: asks.iter().map(ask_row).collect(),
    }
}

pub fn list_toon(report: &ListReport) -> String {
    toon::encode(&[
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        ("total".into(), Toon::Int(report.total as i64)),
        ("asks".into(), asks_table(&report.asks)),
        (
            "diagnostics".into(),
            crate::diagnostics::toon_table(&report.diagnostics),
        ),
    ])
}

// ---------------------------------------------------------------------------
// show
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ShowReport {
    pub kind: &'static str,
    pub ok: bool,
    pub ask: Option<Value>,
    pub effective_decision: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn ask_show(dir: &Path, state_dir: Option<&Path>, ask_id: &str) -> io::Result<ShowReport> {
    let env = LedgerEnv::discover(dir, state_dir);
    let ask_dir = match store::find_ask_dir(&env, ask_id) {
        Ok(dir) => dir,
        Err(err) => return Ok(show_fail(split(err)?)),
    };
    match store::refresh_expiry(&env, &ask_dir) {
        Ok(doc) => {
            let decision = ask::state_of(&doc).effective_decision().to_owned();
            Ok(ShowReport {
                kind: ASK_SHOW_KIND,
                ok: true,
                ask: Some(doc),
                effective_decision: Some(decision),
                diagnostics: Vec::new(),
            })
        }
        Err(err) => Ok(show_fail(split(err)?)),
    }
}

fn show_fail(diagnostics: Vec<Diagnostic>) -> ShowReport {
    ShowReport {
        kind: ASK_SHOW_KIND,
        ok: false,
        ask: None,
        effective_decision: None,
        diagnostics,
    }
}

pub fn show_toon(report: &ShowReport) -> String {
    let mut doc: Vec<(String, Toon)> = vec![
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
    ];
    if let Some(ask) = &report.ask {
        for field in [
            "ask_id",
            "state",
            "mode",
            "action",
            "rule",
            "session_id",
            "run_id",
            "flow",
            "reason",
            "evidence",
            "created_at",
            "updated_at",
            "expires_at",
        ] {
            doc.push((field.into(), Toon::str(value_text(ask.get(field)))));
        }
        doc.push(("effective_decision".into(), opt(&report.effective_decision)));
        if let Some(Value::Array(classes)) = ask.get("classes") {
            doc.push((
                "classes".into(),
                Toon::Arr(
                    classes
                        .iter()
                        .map(|c| Toon::str(value_text(Some(c))))
                        .collect(),
                ),
            ));
        }
        if let Some(res) = ask.get("resolution").filter(|v| **v != Value::Null) {
            doc.push((
                "resolution".into(),
                Toon::Obj(vec![
                    (
                        "decision".into(),
                        Toon::str(value_text(res.get("decision"))),
                    ),
                    ("by".into(), Toon::str(value_text(res.get("by")))),
                    ("at".into(), Toon::str(value_text(res.get("at")))),
                    ("note".into(), Toon::str(value_text(res.get("note")))),
                ]),
            ));
        }
    }
    doc.push((
        "diagnostics".into(),
        crate::diagnostics::toon_table(&report.diagnostics),
    ));
    toon::encode(&doc)
}

// ---------------------------------------------------------------------------
// resolve
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ResolveReport {
    pub kind: &'static str,
    pub ok: bool,
    pub ask_id: String,
    pub state: Option<String>,
    pub effective_decision: Option<String>,
    pub resolution: Option<Value>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn ask_resolve(
    dir: &Path,
    state_dir: Option<&Path>,
    ask_id: &str,
    resolution: Resolution,
    by: &str,
    note: Option<&str>,
) -> io::Result<ResolveReport> {
    let env = LedgerEnv::discover(dir, state_dir);
    match store::resolve_ask(&env, ask_id, resolution, by, note) {
        Ok(ResolveOutcome { doc, warnings }) => {
            let state = ask::state_of(&doc);
            Ok(ResolveReport {
                kind: ASK_RESOLVE_KIND,
                ok: true,
                ask_id: ask_id.to_owned(),
                state: Some(state.as_str().to_owned()),
                effective_decision: Some(state.effective_decision().to_owned()),
                resolution: doc.get("resolution").cloned(),
                diagnostics: warnings,
            })
        }
        Err(err) => Ok(ResolveReport {
            kind: ASK_RESOLVE_KIND,
            ok: false,
            ask_id: ask_id.to_owned(),
            state: None,
            effective_decision: None,
            resolution: None,
            diagnostics: split(err)?,
        }),
    }
}

pub fn resolve_toon(report: &ResolveReport) -> String {
    let resolution = report.resolution.as_ref().filter(|v| **v != Value::Null);
    toon::encode(&[
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        ("ask_id".into(), Toon::str(report.ask_id.clone())),
        ("state".into(), opt(&report.state)),
        ("effective_decision".into(), opt(&report.effective_decision)),
        (
            "decision".into(),
            Toon::str(value_text(resolution.and_then(|r| r.get("decision")))),
        ),
        (
            "by".into(),
            Toon::str(value_text(resolution.and_then(|r| r.get("by")))),
        ),
        (
            "diagnostics".into(),
            crate::diagnostics::toon_table(&report.diagnostics),
        ),
    ])
}

// ---------------------------------------------------------------------------
// await (poll-based resolution routing)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct AwaitReport {
    pub kind: &'static str,
    pub ok: bool,
    pub ask_id: String,
    pub state: Option<String>,
    pub effective_decision: Option<String>,
    pub resolution: Option<Value>,
    /// True when the timeout elapsed with the ask still pending.
    pub timed_out: bool,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct AwaitArgs<'a> {
    pub ask_id: &'a str,
    pub timeout_seconds: u64,
    pub poll_ms: u64,
}

/// Poll the store until the ask is terminal or the timeout elapses. Expiry is
/// materialized on each poll, so an unattended ask that lapses returns
/// `expired` (deny) rather than hanging. `--timeout-seconds 0` polls exactly
/// once. Callers should branch on `effective_decision` (or the CLI exit code):
/// only `approved` unblocks the original session/run.
pub fn ask_await(
    dir: &Path,
    state_dir: Option<&Path>,
    args: &AwaitArgs,
) -> io::Result<AwaitReport> {
    let env = LedgerEnv::discover(dir, state_dir);
    let ask_dir = match store::find_ask_dir(&env, args.ask_id) {
        Ok(dir) => dir,
        Err(err) => return Ok(await_fail(args.ask_id, split(err)?)),
    };

    let deadline = Instant::now() + Duration::from_secs(args.timeout_seconds);
    let poll = Duration::from_millis(args.poll_ms.max(1));
    loop {
        let doc = match store::refresh_expiry(&env, &ask_dir) {
            Ok(doc) => doc,
            Err(err) => return Ok(await_fail(args.ask_id, split(err)?)),
        };
        let state = ask::state_of(&doc);
        if state.is_terminal() || Instant::now() >= deadline {
            return Ok(AwaitReport {
                kind: ASK_AWAIT_KIND,
                ok: true,
                ask_id: args.ask_id.to_owned(),
                state: Some(state.as_str().to_owned()),
                effective_decision: Some(state.effective_decision().to_owned()),
                resolution: doc.get("resolution").cloned().filter(|v| *v != Value::Null),
                timed_out: !state.is_terminal(),
                diagnostics: Vec::new(),
            });
        }
        thread::sleep(poll);
    }
}

fn await_fail(ask_id: &str, diagnostics: Vec<Diagnostic>) -> AwaitReport {
    AwaitReport {
        kind: ASK_AWAIT_KIND,
        ok: false,
        ask_id: ask_id.to_owned(),
        state: None,
        effective_decision: None,
        resolution: None,
        timed_out: false,
        diagnostics,
    }
}

/// Process exit code for `nopal ask await`, encoding the outcome so a shell or
/// AFK caller can fail closed without parsing the payload: 0 approved, 3
/// denied or expired, 4 still pending at timeout, 1 domain error (not found).
pub fn await_exit_code(report: &AwaitReport) -> u8 {
    if !report.ok {
        return 1;
    }
    match report.state.as_deref().and_then(AskState::parse) {
        Some(AskState::Approved) => 0,
        Some(AskState::Denied | AskState::Expired) => 3,
        _ => 4,
    }
}

pub fn await_toon(report: &AwaitReport) -> String {
    toon::encode(&[
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        ("ask_id".into(), Toon::str(report.ask_id.clone())),
        ("state".into(), opt(&report.state)),
        ("effective_decision".into(), opt(&report.effective_decision)),
        ("timed_out".into(), Toon::Bool(report.timed_out)),
        (
            "diagnostics".into(),
            crate::diagnostics::toon_table(&report.diagnostics),
        ),
    ])
}

// ---------------------------------------------------------------------------
// shared cells
// ---------------------------------------------------------------------------

fn opt(value: &Option<String>) -> Toon {
    Toon::str(value.clone().unwrap_or_else(|| "-".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        (tmp, state, repo)
    }

    fn raise_args<'a>() -> RaiseArgs<'a> {
        RaiseArgs {
            session_id: "sess-1",
            run_id: None,
            flow: None,
            mode: "unattended_auto",
            action: "git.push",
            rule: Some("ask-push"),
            classes: &[],
            reason: "please push",
            evidence: None,
            ttl_seconds: 0,
        }
    }

    #[test]
    fn raise_list_show_resolve_await_roundtrip() {
        let (_tmp, state, repo) = temp_setup();
        let raised = ask_raise(&repo, Some(&state), &raise_args()).unwrap();
        assert!(raised.ok);
        let ask_id = raised.ask_id.clone().unwrap();
        assert_eq!(raised.state.as_deref(), Some("pending"));
        assert_eq!(raised.effective_decision.as_deref(), Some("deny"));
        assert!(raise_toon(&raised).contains("kind: nopal.ask.raise/v1"));

        // Visible from a fresh discovery (another process would see the same).
        let listed = ask_list(&repo, Some(&state), Some(AskState::Pending)).unwrap();
        assert_eq!(listed.total, 1);
        assert!(list_toon(&listed).contains(&ask_id));

        let shown = ask_show(&repo, Some(&state), &ask_id).unwrap();
        assert!(shown.ok);
        assert_eq!(shown.effective_decision.as_deref(), Some("deny"));
        assert!(show_toon(&shown).contains("action: git.push"));

        // await before resolution: still pending, times out immediately.
        let waiting = ask_await(
            &repo,
            Some(&state),
            &AwaitArgs {
                ask_id: &ask_id,
                timeout_seconds: 0,
                poll_ms: 10,
            },
        )
        .unwrap();
        assert!(waiting.timed_out);
        assert_eq!(await_exit_code(&waiting), 4);

        // Approve, then await unblocks with exit 0.
        let resolved = ask_resolve(
            &repo,
            Some(&state),
            &ask_id,
            Resolution::Approve,
            "vic",
            Some("go"),
        )
        .unwrap();
        assert!(resolved.ok);
        assert_eq!(resolved.effective_decision.as_deref(), Some("allow"));
        assert!(resolve_toon(&resolved).contains("decision: approve"));

        let unblocked = ask_await(
            &repo,
            Some(&state),
            &AwaitArgs {
                ask_id: &ask_id,
                timeout_seconds: 0,
                poll_ms: 10,
            },
        )
        .unwrap();
        assert_eq!(unblocked.state.as_deref(), Some("approved"));
        assert_eq!(await_exit_code(&unblocked), 0);
        assert!(!unblocked.timed_out);
    }

    #[test]
    fn resolve_missing_ask_is_domain_failure() {
        let (_tmp, state, repo) = temp_setup();
        let report = ask_resolve(
            &repo,
            Some(&state),
            "absent",
            Resolution::Approve,
            "vic",
            None,
        )
        .unwrap();
        assert!(!report.ok);
        assert_eq!(
            report.diagnostics[0].code,
            crate::diagnostics::Code::AskNotFound
        );
        assert!(resolve_toon(&report).contains("ask_not_found"));
    }

    #[test]
    fn await_denied_ask_exits_fail_closed() {
        let (_tmp, state, repo) = temp_setup();
        let raised = ask_raise(&repo, Some(&state), &raise_args()).unwrap();
        let ask_id = raised.ask_id.unwrap();
        ask_resolve(&repo, Some(&state), &ask_id, Resolution::Deny, "vic", None).unwrap();
        let waited = ask_await(
            &repo,
            Some(&state),
            &AwaitArgs {
                ask_id: &ask_id,
                timeout_seconds: 0,
                poll_ms: 10,
            },
        )
        .unwrap();
        assert_eq!(waited.state.as_deref(), Some("denied"));
        assert_eq!(waited.effective_decision.as_deref(), Some("deny"));
        assert_eq!(await_exit_code(&waited), 3);
    }
}
