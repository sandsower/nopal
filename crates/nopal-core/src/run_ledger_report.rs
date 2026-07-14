//! Output envelopes for the `nopal ledger` commands.
//!
//! Same contract as every other nopal command: one envelope per command, one
//! builder per output flavor, kinds `nopal.run_ledger.<command>/v1`. The
//! Python tool's stdout field names (`run_id`, `run_dir`, `checkpoint`,
//! `gate_log`, ...) stay in the envelope as a compatible superset of the
//! legacy contract. Domain problems (missing run, collision, bad status)
//! come back as `ok: false` plus diagnostics; hard IO stays `Err`.

use std::io;
use std::path::Path;

use ledger_json::Value;
use nopal_ledger_json as ledger_json;
use serde::Serialize;

use crate::diagnostics::Diagnostic;
use crate::run_ledger as core;
use crate::run_ledger::Status;
use crate::run_ledger_store as store;
use crate::run_ledger_store::{LedgerEnv, StoreError};
use crate::toon::{self, Value as Toon};

pub const LEDGER_INIT_KIND: &str = "nopal.run_ledger.init/v1";
pub const LEDGER_EVENT_KIND: &str = "nopal.run_ledger.event/v1";
pub const LEDGER_CHECKPOINT_KIND: &str = "nopal.run_ledger.checkpoint/v1";
pub const LEDGER_GATE_KIND: &str = "nopal.run_ledger.gate/v1";
pub const LEDGER_INTERRUPT_KIND: &str = "nopal.run_ledger.interrupt/v1";
pub const LEDGER_FINALIZE_KIND: &str = "nopal.run_ledger.finalize/v1";
pub const LEDGER_RESUME_KIND: &str = "nopal.run_ledger.resume/v1";
pub const LEDGER_DASHBOARD_KIND: &str = "nopal.run_ledger.dashboard/v1";

fn split(result: Result<(), StoreError>) -> io::Result<Vec<Diagnostic>> {
    match result {
        Ok(()) => Ok(Vec::new()),
        Err(StoreError::Domain(diag)) => Ok(vec![diag]),
        Err(StoreError::Io(err)) => Err(err),
    }
}

// ---------------------------------------------------------------------------
// init
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct InitReport {
    pub kind: &'static str,
    pub ok: bool,
    pub run_id: Option<String>,
    pub flow: String,
    pub run_dir: Option<String>,
    pub run_json: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn ledger_init(
    dir: &Path,
    state_dir: Option<&Path>,
    args: &store::InitArgs,
) -> io::Result<InitReport> {
    let env = LedgerEnv::discover(dir, state_dir);
    let flow = core::normalize_flow(args.flow, Some(args.skill));
    match store::init_run(&env, args) {
        Ok(out) => Ok(InitReport {
            kind: LEDGER_INIT_KIND,
            ok: true,
            run_id: Some(out.run_id),
            flow: out.flow,
            run_dir: Some(out.run_dir.display().to_string()),
            run_json: Some(out.run_dir.join("run.json").display().to_string()),
            diagnostics: Vec::new(),
        }),
        Err(err) => Ok(InitReport {
            kind: LEDGER_INIT_KIND,
            ok: false,
            run_id: args.run_id.map(str::to_owned),
            flow,
            run_dir: None,
            run_json: None,
            diagnostics: split(Err(err))?,
        }),
    }
}

pub fn init_toon(report: &InitReport) -> String {
    toon::encode(&[
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        ("run_id".into(), opt(&report.run_id)),
        ("flow".into(), Toon::str(report.flow.clone())),
        ("run_dir".into(), opt(&report.run_dir)),
        ("run_json".into(), opt(&report.run_json)),
        (
            "diagnostics".into(),
            crate::diagnostics::toon_table(&report.diagnostics),
        ),
    ])
}

// ---------------------------------------------------------------------------
// event / checkpoint / gate / interrupt / finalize
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct MutationReport {
    pub kind: &'static str,
    pub ok: bool,
    pub run_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checkpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_log: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub final_report: Option<Value>,
    pub diagnostics: Vec<Diagnostic>,
}

impl MutationReport {
    fn new(kind: &'static str, run_id: &str) -> MutationReport {
        MutationReport {
            kind,
            ok: true,
            run_id: run_id.to_owned(),
            event_type: None,
            checkpoint: None,
            gate_log: None,
            status: None,
            final_report: None,
            diagnostics: Vec::new(),
        }
    }

    fn fail(mut self, err: StoreError) -> io::Result<MutationReport> {
        self.ok = false;
        self.diagnostics = split(Err(err))?;
        Ok(self)
    }
}

fn located_run(
    dir: &Path,
    state_dir: Option<&Path>,
    run_id: &str,
    flow: Option<&str>,
) -> io::Result<Result<std::path::PathBuf, StoreError>> {
    let env = LedgerEnv::discover(dir, state_dir);
    match store::find_run_dir(&env, run_id, flow) {
        Ok(run_dir) => Ok(Ok(run_dir)),
        Err(StoreError::Io(err)) => Err(err),
        Err(domain) => Ok(Err(domain)),
    }
}

pub struct EventArgs<'a> {
    pub run_id: &'a str,
    pub flow: Option<&'a str>,
    pub event_type: &'a str,
    pub payload: Value,
    pub summary: Option<&'a str>,
}

pub fn ledger_event(
    dir: &Path,
    state_dir: Option<&Path>,
    args: &EventArgs,
) -> io::Result<MutationReport> {
    let mut report = MutationReport::new(LEDGER_EVENT_KIND, args.run_id);
    report.event_type = Some(args.event_type.to_owned());
    let run_dir = match located_run(dir, state_dir, args.run_id, args.flow)? {
        Ok(run_dir) => run_dir,
        Err(err) => return report.fail(err),
    };
    match store::append_event(&run_dir, args.event_type, &args.payload, args.summary) {
        Ok(_) => Ok(report),
        Err(err) => report.fail(err),
    }
}

pub struct CheckpointArgs<'a> {
    pub run_id: &'a str,
    pub flow: Option<&'a str>,
    pub name: &'a str,
    pub payload: Value,
    pub resume_hint: Option<&'a str>,
}

pub fn ledger_checkpoint(
    dir: &Path,
    state_dir: Option<&Path>,
    args: &CheckpointArgs,
) -> io::Result<MutationReport> {
    let mut report = MutationReport::new(LEDGER_CHECKPOINT_KIND, args.run_id);
    let run_dir = match located_run(dir, state_dir, args.run_id, args.flow)? {
        Ok(run_dir) => run_dir,
        Err(err) => return report.fail(err),
    };
    let checkpoint_path =
        match store::record_checkpoint(&run_dir, args.name, &args.payload, args.resume_hint) {
            Ok(path) => path,
            Err(err) => return report.fail(err),
        };
    let mut event_payload = std::collections::BTreeMap::new();
    event_payload.insert("name".to_owned(), Value::String(args.name.to_owned()));
    event_payload.insert(
        "path".to_owned(),
        Value::String(checkpoint_path.display().to_string()),
    );
    event_payload.insert("payload".to_owned(), args.payload.clone());
    event_payload.insert(
        "resume_hint".to_owned(),
        args.resume_hint
            .map(|hint| Value::String(hint.to_owned()))
            .unwrap_or(Value::Null),
    );
    let event_result =
        store::append_event(&run_dir, "checkpoint", &Value::Object(event_payload), None);
    report.checkpoint = Some(checkpoint_path.display().to_string());
    match event_result {
        Ok(_) => Ok(report),
        Err(err) => report.fail(err),
    }
}

pub struct GateArgs<'a> {
    pub run_id: &'a str,
    pub flow: Option<&'a str>,
    pub name: &'a str,
    pub scope: Option<&'a str>,
    pub envelope: Value,
    pub resume_hint: Option<&'a str>,
}

pub fn ledger_gate(
    dir: &Path,
    state_dir: Option<&Path>,
    args: &GateArgs,
) -> io::Result<MutationReport> {
    let mut report = MutationReport::new(LEDGER_GATE_KIND, args.run_id);
    let run_dir = match located_run(dir, state_dir, args.run_id, args.flow)? {
        Ok(run_dir) => run_dir,
        Err(err) => return report.fail(err),
    };
    match store::record_gate(
        &run_dir,
        args.name,
        args.scope,
        &args.envelope,
        args.resume_hint,
    ) {
        Ok(out) => {
            report.gate_log = Some(out.envelope_path.display().to_string());
            report.checkpoint = Some(out.checkpoint_path.display().to_string());
            Ok(report)
        }
        Err(err) => report.fail(err),
    }
}

pub fn ledger_interrupt(
    dir: &Path,
    state_dir: Option<&Path>,
    run_id: &str,
    flow: Option<&str>,
    reason: &str,
    resume_hint: Option<&str>,
) -> io::Result<MutationReport> {
    let mut report = MutationReport::new(LEDGER_INTERRUPT_KIND, run_id);
    let run_dir = match located_run(dir, state_dir, run_id, flow)? {
        Ok(run_dir) => run_dir,
        Err(err) => return report.fail(err),
    };
    match store::record_interrupt(&run_dir, reason, resume_hint) {
        Ok(checkpoint_path) => {
            report.status = Some(Status::Interrupted.as_str().to_owned());
            report.checkpoint = Some(checkpoint_path.display().to_string());
            Ok(report)
        }
        Err(err) => report.fail(err),
    }
}

pub fn ledger_finalize(
    dir: &Path,
    state_dir: Option<&Path>,
    run_id: &str,
    flow: Option<&str>,
    status: &str,
    report_file: Option<&Path>,
) -> io::Result<MutationReport> {
    let mut report = MutationReport::new(LEDGER_FINALIZE_KIND, run_id);
    report.status = Some(status.to_owned());
    let run_dir = match located_run(dir, state_dir, run_id, flow)? {
        Ok(run_dir) => run_dir,
        Err(err) => return report.fail(err),
    };
    match store::record_finalize(&run_dir, status, report_file) {
        Ok(out) => {
            report.final_report = Some(
                out.final_report
                    .map(|p| Value::String(p.display().to_string()))
                    .unwrap_or(Value::Null),
            );
            Ok(report)
        }
        Err(err) => report.fail(err),
    }
}

pub fn mutation_toon(report: &MutationReport) -> String {
    let mut doc: Vec<(String, Toon)> = vec![
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        ("run_id".into(), Toon::str(report.run_id.clone())),
    ];
    if let Some(event_type) = &report.event_type {
        doc.push(("event_type".into(), Toon::str(event_type.clone())));
    }
    if let Some(status) = &report.status {
        doc.push(("status".into(), Toon::str(status.clone())));
    }
    if let Some(gate_log) = &report.gate_log {
        doc.push(("gate_log".into(), Toon::str(gate_log.clone())));
    }
    if let Some(checkpoint) = &report.checkpoint {
        doc.push(("checkpoint".into(), Toon::str(checkpoint.clone())));
    }
    if let Some(final_report) = &report.final_report {
        doc.push(("final_report".into(), value_cell(final_report)));
    }
    doc.push((
        "diagnostics".into(),
        crate::diagnostics::toon_table(&report.diagnostics),
    ));
    toon::encode(&doc)
}

// ---------------------------------------------------------------------------
// resume
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct ResumeReport {
    pub kind: &'static str,
    pub ok: bool,
    pub run_id: Option<String>,
    pub flow: Option<String>,
    pub run_dir: Option<String>,
    pub status: Option<String>,
    pub ticket: Option<Value>,
    pub branch: Option<String>,
    pub latest_checkpoint: Option<Value>,
    pub last_checkpoint: Option<String>,
    pub resume_hint: Option<String>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct ResumeArgs<'a> {
    pub flow: Option<&'a str>,
    pub ticket_id: Option<&'a str>,
    pub branch: Option<&'a str>,
    pub include_completed: bool,
}

fn value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::Bool(b)) => b.to_string(),
        _ => "-".to_owned(),
    }
}

pub fn ledger_resume(
    dir: &Path,
    state_dir: Option<&Path>,
    args: &ResumeArgs,
) -> io::Result<ResumeReport> {
    let env = LedgerEnv::discover(dir, state_dir);
    let include_completed = args.include_completed;
    let scan = match store::scan_runs(&env, args.flow, move |status| {
        include_completed || status.is_incomplete()
    }) {
        Ok(scan) => scan,
        Err(StoreError::Io(err)) => return Err(err),
        Err(StoreError::Domain(diag)) => {
            return Ok(empty_resume(false, vec![diag]));
        }
    };
    let mut candidates: Vec<Value> = scan
        .runs
        .into_iter()
        .map(|r| r.entry)
        .filter(|entry| {
            if let Some(ticket_id) = args.ticket_id {
                // Python `run.get("ticket_id") or run.get("ticket", {}).get("id")`:
                // falsy ticket_id values fall through to the nested id.
                let entry_ticket = match entry.get("ticket_id") {
                    Some(v) if core::truthy(Some(v)) => Some(v),
                    _ => entry.get("ticket").and_then(|t| t.get("id")),
                };
                if value_text(entry_ticket) != ticket_id {
                    return false;
                }
            }
            if let Some(branch) = args.branch
                && entry.get("branch").and_then(Value::as_str) != Some(branch)
            {
                return false;
            }
            true
        })
        .collect();
    if candidates.is_empty() {
        let mut diagnostics = scan.warnings;
        diagnostics.push(Diagnostic::error(
            crate::diagnostics::Code::RunNotFound,
            "runs",
            "no matching run found",
        ));
        return Ok(empty_resume(false, diagnostics));
    }
    candidates.sort_by_key(store::recency_key);
    let selected = candidates.remove(candidates.len() - 1);
    let field = |key: &str| selected.get(key).and_then(Value::as_str).map(str::to_owned);
    Ok(ResumeReport {
        kind: LEDGER_RESUME_KIND,
        ok: true,
        run_id: field("run_id"),
        flow: field("flow"),
        run_dir: selected
            .get("paths")
            .and_then(|p| p.get("run_dir"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        status: field("status"),
        ticket: selected.get("ticket").cloned(),
        branch: field("branch"),
        latest_checkpoint: selected.get("latest_checkpoint").cloned(),
        last_checkpoint: field("last_checkpoint"),
        resume_hint: field("resume_hint"),
        diagnostics: scan.warnings,
    })
}

fn empty_resume(ok: bool, diagnostics: Vec<Diagnostic>) -> ResumeReport {
    ResumeReport {
        kind: LEDGER_RESUME_KIND,
        ok,
        run_id: None,
        flow: None,
        run_dir: None,
        status: None,
        ticket: None,
        branch: None,
        latest_checkpoint: None,
        last_checkpoint: None,
        resume_hint: None,
        diagnostics,
    }
}

pub fn resume_toon(report: &ResumeReport) -> String {
    toon::encode(&[
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        ("run_id".into(), opt(&report.run_id)),
        ("flow".into(), opt(&report.flow)),
        ("run_dir".into(), opt(&report.run_dir)),
        ("status".into(), opt(&report.status)),
        (
            "ticket".into(),
            Toon::str(value_text(report.ticket.as_ref().and_then(|t| t.get("id")))),
        ),
        ("branch".into(), opt(&report.branch)),
        (
            "latest_checkpoint".into(),
            Toon::str(value_text(
                report
                    .latest_checkpoint
                    .as_ref()
                    .and_then(|c| c.get("name")),
            )),
        ),
        ("last_checkpoint".into(), opt(&report.last_checkpoint)),
        ("resume_hint".into(), opt(&report.resume_hint)),
        (
            "diagnostics".into(),
            crate::diagnostics::toon_table(&report.diagnostics),
        ),
    ])
}

// ---------------------------------------------------------------------------
// dashboard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct DashboardReport {
    pub kind: &'static str,
    pub ok: bool,
    pub total: usize,
    pub runs: Vec<Value>,
    pub diagnostics: Vec<Diagnostic>,
}

pub struct DashboardArgs<'a> {
    pub flow: Option<&'a str>,
    pub all: bool,
    pub limit: usize,
}

pub fn ledger_dashboard(
    dir: &Path,
    state_dir: Option<&Path>,
    args: &DashboardArgs,
) -> io::Result<DashboardReport> {
    let env = LedgerEnv::discover(dir, state_dir);
    let all = args.all;
    let scan = match store::scan_runs(&env, args.flow, move |status| all || status.is_incomplete())
    {
        Ok(scan) => scan,
        Err(StoreError::Io(err)) => return Err(err),
        Err(StoreError::Domain(diag)) => {
            return Ok(DashboardReport {
                kind: LEDGER_DASHBOARD_KIND,
                ok: false,
                total: 0,
                runs: Vec::new(),
                diagnostics: vec![diag],
            });
        }
    };
    let flow_filter = args.flow.map(|f| core::normalize_flow(Some(f), None));
    let mut runs: Vec<store::ScannedRun> = scan
        .runs
        .into_iter()
        .filter(|run| match &flow_filter {
            Some(wanted) => {
                let entry_flow = run
                    .entry
                    .get("flow")
                    .and_then(Value::as_str)
                    .unwrap_or("run");
                &core::normalize_flow(Some(entry_flow), None) == wanted
            }
            None => true,
        })
        .collect();
    runs.sort_by_key(|run| std::cmp::Reverse(store::recency_key(&run.entry)));
    let entries: Vec<Value> = runs
        .iter()
        .map(|run| dashboard_entry(&run.entry, &run.run_dir, args.limit))
        .collect();
    Ok(DashboardReport {
        kind: LEDGER_DASHBOARD_KIND,
        ok: true,
        total: entries.len(),
        runs: entries,
        diagnostics: scan.warnings,
    })
}

/// Mirrors Python `_dashboard_run_entry`, including its truthiness filters:
/// falsy `ticket`/`latest_checkpoint`/`resume_hint`/`interruption`/
/// `finalized_at` values are treated as absent, and `run_dir` is copied
/// verbatim (null included) whenever `paths` is truthy.
fn dashboard_entry(run: &Value, run_dir: &Path, gate_limit: usize) -> Value {
    let ticket = match run.get("ticket") {
        Some(t) if core::truthy(Some(t)) => t.clone(),
        _ => {
            ledger_json::json!({"id": run.get("ticket_id").cloned().unwrap_or_else(|| Value::String("none".into()))})
        }
    };
    let mut entry = ledger_json::json!({
        "run_id": run.get("run_id").cloned().unwrap_or(Value::Null),
        "flow": run.get("flow").cloned().unwrap_or(Value::Null),
        "status": run.get("status").cloned().unwrap_or(Value::Null),
        "started_at": run.get("started_at").cloned().unwrap_or(Value::Null),
        "updated_at": run.get("updated_at").cloned().unwrap_or(Value::Null),
        "ticket": ticket,
        "branch": run.get("branch").cloned().unwrap_or(Value::Null),
    });
    let Some(map) = entry.as_object_mut() else {
        return entry;
    };
    if let Some(cp) = run
        .get("latest_checkpoint")
        .filter(|v| core::truthy(Some(v)))
    {
        map.insert(
            "latest_checkpoint".to_owned(),
            ledger_json::json!({
                "name": cp.get("name").cloned().unwrap_or(Value::Null),
                "timestamp": cp.get("timestamp").cloned().unwrap_or(Value::Null),
            }),
        );
    }
    if let Some(hint) = run.get("resume_hint").filter(|v| core::truthy(Some(v))) {
        map.insert("resume_hint".to_owned(), hint.clone());
    }
    if let Some(interruption) = run.get("interruption").filter(|v| core::truthy(Some(v))) {
        map.insert(
            "interruption".to_owned(),
            ledger_json::json!({
                "reason": interruption.get("reason").cloned().unwrap_or(Value::Null),
                "timestamp": interruption.get("timestamp").cloned().unwrap_or(Value::Null),
            }),
        );
    }
    if let Some(finalized_at) = run.get("finalized_at").filter(|v| core::truthy(Some(v))) {
        map.insert("finalized_at".to_owned(), finalized_at.clone());
    }
    if let Some(paths) = run.get("paths").filter(|v| core::truthy(Some(v))) {
        map.insert(
            "run_dir".to_owned(),
            paths.get("run_dir").cloned().unwrap_or(Value::Null),
        );
        let finalized = core::truthy(run.get("finalized_at"));
        if let Some(report_path) = paths.get("final_report").and_then(Value::as_str)
            && finalized
            && Path::new(report_path).is_file()
        {
            map.insert(
                "final_report".to_owned(),
                Value::String(report_path.to_owned()),
            );
        }
    }
    let gates = store::collect_gate_history(run_dir, gate_limit);
    if !gates.is_empty() {
        map.insert("gates".to_owned(), Value::Array(gates));
    }
    entry
}

pub fn dashboard_toon(report: &DashboardReport) -> String {
    let runs_table = Toon::Table {
        fields: [
            "run_id",
            "flow",
            "status",
            "ticket",
            "branch",
            "updated",
            "checkpoint",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect(),
        rows: report
            .runs
            .iter()
            .map(|run| {
                vec![
                    Toon::str(value_text(run.get("run_id"))),
                    Toon::str(value_text(run.get("flow"))),
                    Toon::str(value_text(run.get("status"))),
                    Toon::str(value_text(run.get("ticket").and_then(|t| t.get("id")))),
                    Toon::str(value_text(run.get("branch"))),
                    Toon::str(value_text(run.get("updated_at"))),
                    Toon::str(value_text(
                        run.get("latest_checkpoint").and_then(|c| c.get("name")),
                    )),
                ]
            })
            .collect(),
    };
    let mut gate_rows = Vec::new();
    for run in &report.runs {
        let run_id = value_text(run.get("run_id"));
        if let Some(Value::Array(gates)) = run.get("gates") {
            for gate in gates {
                gate_rows.push(vec![
                    Toon::str(run_id.clone()),
                    Toon::str(format!(
                        "{}/{}",
                        value_text(gate.get("scope")),
                        value_text(gate.get("name"))
                    )),
                    Toon::str(value_text(gate.get("attempt"))),
                    Toon::str(value_text(gate.get("status"))),
                    Toon::str(match gate.get("classification") {
                        Some(Value::String(s)) => s.clone(),
                        _ => "-".to_owned(),
                    }),
                ]);
            }
        }
    }
    let gates_table = Toon::Table {
        fields: ["run_id", "gate", "attempt", "status", "classification"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        rows: gate_rows,
    };
    toon::encode(&[
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        ("total".into(), Toon::Int(report.total as i64)),
        ("runs".into(), runs_table),
        ("gates".into(), gates_table),
        (
            "diagnostics".into(),
            crate::diagnostics::toon_table(&report.diagnostics),
        ),
    ])
}

// ---------------------------------------------------------------------------
// pointer
// ---------------------------------------------------------------------------

pub const LEDGER_POINTER_KIND: &str = "nopal.run_ledger.pointer/v1";

const NOPAL_POINTER_REL: &str = ".nopal/checkpoints/latest.json";
const BEISLID_POINTER_REL: &str = ".beislid/checkpoints/latest.json";

/// One entry from the pointer file's `latest` map, flattened. Event names
/// and `source_skill` are open vocabulary: skills keep writing whatever
/// tokens they like, and nopal passes them through as data.
#[derive(Debug, Clone, Serialize)]
pub struct PointerEntry {
    pub event: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ticket: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_skill: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PointerReport {
    pub kind: &'static str,
    pub ok: bool,
    /// Project-relative path of the pointer file that was read, or `None`
    /// when neither the nopal nor the beislid location exists.
    pub source: Option<String>,
    pub entries: Vec<PointerEntry>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Read the checkpoint pointer file: `.nopal/checkpoints/latest.json`
/// first, `.beislid/checkpoints/latest.json` as fallback. Repo-local
/// (`--dir`); the ledger state dir does not apply here. Neither file
/// existing is `ok: true, entries: []` - this is a read of skill-written
/// side-effect state, not a ledger requirement.
pub fn ledger_pointer(dir: &Path) -> io::Result<PointerReport> {
    let (source, text) =
        if let Some(text) = crate::validate::read_optional(&dir.join(NOPAL_POINTER_REL))? {
            (NOPAL_POINTER_REL.to_owned(), text)
        } else if let Some(text) = crate::validate::read_optional(&dir.join(BEISLID_POINTER_REL))? {
            (BEISLID_POINTER_REL.to_owned(), text)
        } else {
            return Ok(PointerReport {
                kind: LEDGER_POINTER_KIND,
                ok: true,
                source: None,
                entries: Vec::new(),
                diagnostics: Vec::new(),
            });
        };

    let mut diagnostics = Vec::new();
    let root: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(err) => {
            diagnostics.push(Diagnostic::error(
                crate::diagnostics::Code::LedgerEntryInvalid,
                source.clone(),
                format!("malformed JSON in checkpoint pointer file: {err}"),
            ));
            return Ok(PointerReport {
                kind: LEDGER_POINTER_KIND,
                ok: false,
                source: Some(source),
                entries: Vec::new(),
                diagnostics,
            });
        }
    };

    let Some(latest) = root.get("latest").and_then(serde_json::Value::as_object) else {
        diagnostics.push(Diagnostic::error(
            crate::diagnostics::Code::LedgerEntryInvalid,
            source.clone(),
            "\"latest\" must be an object",
        ));
        return Ok(PointerReport {
            kind: LEDGER_POINTER_KIND,
            ok: false,
            source: Some(source),
            entries: Vec::new(),
            diagnostics,
        });
    };

    let mut entries = Vec::new();
    for (event, body) in latest {
        let path = body
            .get("path")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if path.is_empty() || path.starts_with('/') || path.contains("..") {
            diagnostics.push(Diagnostic::warning(
                crate::diagnostics::Code::LedgerEntryInvalid,
                source.clone(),
                format!("dropping checkpoint entry {event:?}: unsafe path {path:?}"),
            ));
            continue;
        }
        entries.push(PointerEntry {
            event: event.clone(),
            path: path.to_owned(),
            ticket: body.get("ticket").cloned(),
            branch: body
                .get("branch")
                .and_then(|value| value.as_str())
                .map(str::to_owned),
            source_skill: body
                .get("source_skill")
                .and_then(|value| value.as_str())
                .map(str::to_owned),
            written_at: body
                .get("written_at")
                .and_then(|value| value.as_str())
                .map(str::to_owned),
        });
    }
    entries.sort_by(|a, b| a.event.cmp(&b.event));

    Ok(PointerReport {
        kind: LEDGER_POINTER_KIND,
        ok: true,
        source: Some(source),
        entries,
        diagnostics,
    })
}

pub fn pointer_toon(report: &PointerReport) -> String {
    toon::encode(&[
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        (
            "source".into(),
            report
                .source
                .clone()
                .map_or_else(|| Toon::str("-"), Toon::str),
        ),
        (
            "entries".into(),
            Toon::Table {
                fields: [
                    "event",
                    "path",
                    "ticket_id",
                    "branch",
                    "source_skill",
                    "written_at",
                ]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
                rows: report
                    .entries
                    .iter()
                    .map(|entry| {
                        vec![
                            Toon::str(entry.event.clone()),
                            Toon::str(entry.path.clone()),
                            Toon::str(
                                entry
                                    .ticket
                                    .as_ref()
                                    .and_then(|ticket| ticket.get("id"))
                                    .and_then(|id| id.as_str())
                                    .unwrap_or("-")
                                    .to_owned(),
                            ),
                            opt(&entry.branch),
                            opt(&entry.source_skill),
                            opt(&entry.written_at),
                        ]
                    })
                    .collect(),
            },
        ),
        (
            "diagnostics".into(),
            crate::diagnostics::toon_table(&report.diagnostics),
        ),
    ])
}

// ---------------------------------------------------------------------------
// prune
// ---------------------------------------------------------------------------

pub const LEDGER_PRUNE_KIND: &str = "nopal.run_ledger.prune/v1";

pub struct PruneArgs {
    pub stale_after_hours: u64,
    pub apply: bool,
}

/// One incomplete, unfinalized run older than the staleness threshold - the
/// same population `nopal field` counts in `stale_total` (see `field::is_stale`
/// for the shared rule). `finalized` is only ever `true` under `--apply`.
#[derive(Debug, Clone, Serialize)]
pub struct PruneCandidate {
    pub run_id: String,
    pub flow: String,
    pub repo: String,
    pub status: String,
    pub updated_at: String,
    pub age_hours: u64,
    pub run_dir: String,
    pub finalized: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct PruneReport {
    pub kind: &'static str,
    pub ok: bool,
    pub apply: bool,
    pub stale_after_hours: u64,
    pub selected: usize,
    pub applied: usize,
    pub candidates: Vec<PruneCandidate>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Global, state-dir-wide scan (every repo, every flow - `dir` only resolves
/// the state dir, exactly like `field_store::field_status`; unlike every other
/// `nopal ledger` command, pruning is not scoped to one repo, since that is
/// where the mess accumulates). Selects incomplete, unfinalized runs whose
/// `updated_at` is at least `stale_after_hours` old - the identical rule
/// `nopal field` uses for `stale` (`field::is_stale`), so the two surfaces
/// always agree on what counts as stale. Dry-run (`apply: false`) never
/// writes; `apply: true` finalizes each selected run as `interrupted`
/// through the existing `record_finalize` plumbing (atomic write, lock,
/// `finalized_at`) - no new run.json fields, no new statuses.
pub fn ledger_prune(
    dir: &Path,
    state_dir: Option<&Path>,
    args: &PruneArgs,
) -> io::Result<PruneReport> {
    let env = LedgerEnv::discover(dir, state_dir);
    let mut warnings = Vec::new();
    let runs = crate::field_store::scan_all_runs(&env.state_dir, &mut warnings)?;
    let now_iso = store::now_iso();

    let mut candidates: Vec<PruneCandidate> = Vec::new();
    for run in &runs {
        let entry = &run.entry;
        let status_text = entry
            .get("status")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let status = Status::parse(&status_text);
        let finalized_at = entry
            .get("finalized_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        let updated_at = entry
            .get("updated_at")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        if !crate::field::is_stale(
            status,
            &finalized_at,
            &updated_at,
            &now_iso,
            args.stale_after_hours,
        ) {
            continue;
        }
        let run_dir = entry
            .get("paths")
            .and_then(|p| p.get("run_dir"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned();
        candidates.push(PruneCandidate {
            run_id: entry
                .get("run_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            flow: entry
                .get("flow")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            repo: entry
                .get("repo")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_owned(),
            status: status_text,
            age_hours: crate::field::age_hours(&updated_at, &now_iso).unwrap_or(0),
            updated_at,
            run_dir,
            finalized: false,
        });
    }
    candidates.sort_by(|a, b| a.run_id.cmp(&b.run_id).then(a.flow.cmp(&b.flow)));

    let selected = candidates.len();
    let mut applied = 0usize;
    if args.apply {
        for candidate in &mut candidates {
            if candidate.run_dir.is_empty() {
                warnings.push(Diagnostic::warning(
                    crate::diagnostics::Code::LedgerEntryInvalid,
                    candidate.run_id.clone(),
                    "run.json has no paths.run_dir; skipping finalize".to_owned(),
                ));
                continue;
            }
            match store::record_finalize(
                Path::new(&candidate.run_dir),
                Status::Interrupted.as_str(),
                None,
            ) {
                Ok(_) => {
                    candidate.finalized = true;
                    applied += 1;
                }
                Err(StoreError::Io(err)) => return Err(err),
                Err(StoreError::Domain(diag)) => warnings.push(diag),
            }
        }
    }

    Ok(PruneReport {
        kind: LEDGER_PRUNE_KIND,
        ok: true,
        apply: args.apply,
        stale_after_hours: args.stale_after_hours,
        selected,
        applied,
        candidates,
        diagnostics: warnings,
    })
}

pub fn prune_toon(report: &PruneReport) -> String {
    let candidates_table = Toon::Table {
        fields: ["run_id", "flow", "repo", "status", "age_hours", "finalized"]
            .iter()
            .map(|s| (*s).to_owned())
            .collect(),
        rows: report
            .candidates
            .iter()
            .map(|c| {
                vec![
                    Toon::str(c.run_id.clone()),
                    Toon::str(c.flow.clone()),
                    Toon::str(c.repo.clone()),
                    Toon::str(c.status.clone()),
                    Toon::Int(c.age_hours as i64),
                    Toon::Bool(c.finalized),
                ]
            })
            .collect(),
    };
    toon::encode(&[
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        ("apply".into(), Toon::Bool(report.apply)),
        (
            "stale_after_hours".into(),
            Toon::Int(report.stale_after_hours as i64),
        ),
        ("selected".into(), Toon::Int(report.selected as i64)),
        ("applied".into(), Toon::Int(report.applied as i64)),
        ("candidates".into(), candidates_table),
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

fn value_cell(value: &Value) -> Toon {
    match value {
        Value::String(s) => Toon::str(s.clone()),
        Value::Null => Toon::str("-"),
        other => Toon::str(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::run_ledger_store::InitArgs;

    fn temp_setup() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let state = tmp.path().join("state");
        let repo = tmp.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        (tmp, state, repo)
    }

    fn init<'a>(
        repo: &Path,
        state: &Path,
        run_id: Option<&'a str>,
        flow: Option<&'a str>,
    ) -> InitReport {
        ledger_init(
            repo,
            Some(state),
            &InitArgs {
                skill: "kickoff",
                flow,
                ticket_id: "TASK-19",
                ticket_title: "Port",
                ticket_url: "",
                branch: Some("feature/x"),
                run_id,
            },
        )
        .unwrap()
    }

    #[test]
    fn init_report_roundtrips_and_collision_flips_ok() {
        let (_tmp, state, repo) = temp_setup();
        let ok = init(&repo, &state, Some("fixed"), None);
        assert!(ok.ok);
        assert_eq!(ok.run_id.as_deref(), Some("fixed"));
        assert!(
            ok.run_json
                .as_deref()
                .is_some_and(|p| p.ends_with("run.json"))
        );
        let toon = init_toon(&ok);
        assert!(toon.contains("kind: nopal.run_ledger.init/v1"));

        let collision = init(&repo, &state, Some("fixed"), None);
        assert!(!collision.ok);
        assert_eq!(
            collision.diagnostics[0].code,
            crate::diagnostics::Code::RunIdCollision
        );
    }

    #[test]
    fn full_lifecycle_reports_stay_ok_and_resume_finds_latest() {
        let (_tmp, state, repo) = temp_setup();
        let created = init(&repo, &state, Some("r1"), None);
        assert!(created.ok);

        let event = ledger_event(
            &repo,
            Some(&state),
            &EventArgs {
                run_id: "r1",
                flow: None,
                event_type: "step",
                payload: ledger_json::json!({"n": 1}),
                summary: Some("did a step"),
            },
        )
        .unwrap();
        assert!(event.ok);

        let checkpoint = ledger_checkpoint(
            &repo,
            Some(&state),
            &CheckpointArgs {
                run_id: "r1",
                flow: None,
                name: "mid",
                payload: ledger_json::json!({}),
                resume_hint: Some("resume mid"),
            },
        )
        .unwrap();
        assert!(checkpoint.ok);
        assert!(checkpoint.checkpoint.is_some());

        let gate = ledger_gate(
            &repo,
            Some(&state),
            &GateArgs {
                run_id: "r1",
                flow: None,
                name: "fmt",
                scope: None,
                envelope: ledger_json::json!({"status": "pass", "gate": {"name": "fmt", "timestamp": "T"}}),
                resume_hint: None,
            },
        )
        .unwrap();
        assert!(gate.ok);
        assert!(
            gate.gate_log
                .as_deref()
                .is_some_and(|p| p.ends_with("envelope.json"))
        );

        let resume = ledger_resume(
            &repo,
            Some(&state),
            &ResumeArgs {
                flow: None,
                ticket_id: Some("TASK-19"),
                branch: Some("feature/x"),
                include_completed: false,
            },
        )
        .unwrap();
        assert!(resume.ok);
        assert_eq!(resume.run_id.as_deref(), Some("r1"));
        // The gate checkpoint's default hint overwrites the earlier one,
        // exactly like the Python tool.
        assert_eq!(
            resume.resume_hint.as_deref(),
            Some("continue after reviewing gate result")
        );

        let interrupt =
            ledger_interrupt(&repo, Some(&state), "r1", None, "pause", Some("hint")).unwrap();
        assert!(interrupt.ok);
        assert_eq!(interrupt.status.as_deref(), Some("interrupted"));

        let finalize = ledger_finalize(&repo, Some(&state), "r1", None, "completed", None).unwrap();
        assert!(finalize.ok);
        assert_eq!(finalize.final_report, Some(Value::Null));

        // completed runs vanish from default resume...
        let gone = ledger_resume(
            &repo,
            Some(&state),
            &ResumeArgs {
                flow: None,
                ticket_id: None,
                branch: None,
                include_completed: false,
            },
        )
        .unwrap();
        assert!(!gone.ok);
        // ...and come back with --include-completed.
        let back = ledger_resume(
            &repo,
            Some(&state),
            &ResumeArgs {
                flow: None,
                ticket_id: None,
                branch: None,
                include_completed: true,
            },
        )
        .unwrap();
        assert!(back.ok);
        assert_eq!(back.status.as_deref(), Some("completed"));
    }

    #[test]
    fn missing_run_is_a_domain_failure_not_io() {
        let (_tmp, state, repo) = temp_setup();
        let report = ledger_event(
            &repo,
            Some(&state),
            &EventArgs {
                run_id: "absent",
                flow: None,
                event_type: "x",
                payload: Value::Null,
                summary: None,
            },
        )
        .unwrap();
        assert!(!report.ok);
        assert_eq!(
            report.diagnostics[0].code,
            crate::diagnostics::Code::RunNotFound
        );
        let toon = mutation_toon(&report);
        assert!(toon.contains("run_not_found"));
    }

    #[test]
    fn dashboard_reports_runs_and_gate_classification() {
        let (_tmp, state, repo) = temp_setup();
        init(&repo, &state, Some("r1"), None);
        ledger_gate(
            &repo,
            Some(&state),
            &GateArgs {
                run_id: "r1",
                flow: None,
                name: "clippy",
                scope: Some("repo"),
                envelope: ledger_json::json!({
                    "status": "fail",
                    "environment_failure": true,
                    "gate": {"name": "clippy", "timestamp": "T2"},
                }),
                resume_hint: None,
            },
        )
        .unwrap();
        init(&repo, &state, Some("r2"), Some("other"));

        let dashboard = ledger_dashboard(
            &repo,
            Some(&state),
            &DashboardArgs {
                flow: None,
                all: false,
                limit: 5,
            },
        )
        .unwrap();
        assert!(dashboard.ok);
        assert_eq!(dashboard.total, 2);
        let r1 = dashboard
            .runs
            .iter()
            .find(|r| r.get("run_id") == Some(&Value::String("r1".into())))
            .unwrap();
        assert_eq!(r1["gates"][0]["classification"], "environment_failure");

        let filtered = ledger_dashboard(
            &repo,
            Some(&state),
            &DashboardArgs {
                flow: Some("other"),
                all: false,
                limit: 5,
            },
        )
        .unwrap();
        assert_eq!(filtered.total, 1);

        let toon = dashboard_toon(&dashboard);
        assert!(toon.contains("kind: nopal.run_ledger.dashboard/v1"));
        assert!(toon.contains("environment_failure"));
    }

    // -------------------------------------------------------------------
    // pointer
    // -------------------------------------------------------------------

    #[test]
    fn pointer_missing_both_files_is_ok_with_empty_entries() {
        let (_tmp, _state, repo) = temp_setup();
        let report = ledger_pointer(&repo).unwrap();
        assert!(report.ok);
        assert_eq!(report.source, None);
        assert_eq!(report.entries.len(), 0);
        assert_eq!(report.diagnostics, vec![]);
    }

    #[test]
    fn pointer_prefers_nopal_location_over_beislid_fallback() {
        let (_tmp, _state, repo) = temp_setup();
        std::fs::create_dir_all(repo.join(".nopal/checkpoints")).unwrap();
        std::fs::write(
            repo.join(".nopal/checkpoints/latest.json"),
            r#"{ "latest": { "kickoff_start": {
                "event": "kickoff_start", "path": "plans/nopal.md",
                "source_skill": "kickoff", "written_at": "2026-07-06T00:00:00Z"
            } } }"#,
        )
        .unwrap();
        std::fs::create_dir_all(repo.join(".beislid/checkpoints")).unwrap();
        std::fs::write(
            repo.join(".beislid/checkpoints/latest.json"),
            r#"{ "latest": { "kickoff_start": {
                "event": "kickoff_start", "path": "plans/beislid.md"
            } } }"#,
        )
        .unwrap();

        let report = ledger_pointer(&repo).unwrap();
        assert!(report.ok);
        assert_eq!(report.source, Some(NOPAL_POINTER_REL.to_owned()));
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].path, "plans/nopal.md");
    }

    #[test]
    fn pointer_falls_back_to_beislid_location() {
        let (_tmp, _state, repo) = temp_setup();
        std::fs::create_dir_all(repo.join(".beislid/checkpoints")).unwrap();
        std::fs::write(
            repo.join(".beislid/checkpoints/latest.json"),
            r#"{ "latest": { "spec_approved": {
                "event": "spec_approved", "path": "plans/x.md",
                "ticket": { "id": "TASK-1", "title": "T" },
                "branch": "nopal/x", "source_skill": "spec",
                "written_at": "2026-07-06T00:00:00Z"
            } } }"#,
        )
        .unwrap();

        let report = ledger_pointer(&repo).unwrap();
        assert!(report.ok);
        assert_eq!(report.source, Some(BEISLID_POINTER_REL.to_owned()));
        assert_eq!(report.entries.len(), 1);
        let entry = &report.entries[0];
        assert_eq!(entry.event, "spec_approved");
        assert_eq!(entry.path, "plans/x.md");
        assert_eq!(entry.branch, Some("nopal/x".to_owned()));
        assert_eq!(entry.source_skill, Some("spec".to_owned()));
        assert_eq!(
            entry
                .ticket
                .as_ref()
                .and_then(|t| t.get("id"))
                .and_then(|v| v.as_str()),
            Some("TASK-1")
        );
    }

    #[test]
    fn pointer_drops_unsafe_paths_with_a_warning() {
        let (_tmp, _state, repo) = temp_setup();
        std::fs::create_dir_all(repo.join(".nopal/checkpoints")).unwrap();
        std::fs::write(
            repo.join(".nopal/checkpoints/latest.json"),
            r#"{ "latest": {
                "absolute": { "event": "absolute", "path": "/etc/passwd" },
                "traversal": { "event": "traversal", "path": "../../secret.md" },
                "empty": { "event": "empty", "path": "" },
                "missing": { "event": "missing" },
                "safe": { "event": "safe", "path": "plans/ok.md" }
            } }"#,
        )
        .unwrap();

        let report = ledger_pointer(&repo).unwrap();
        assert!(report.ok);
        assert_eq!(report.entries.len(), 1);
        assert_eq!(report.entries[0].event, "safe");
        assert_eq!(report.diagnostics.len(), 4);
        assert!(
            report
                .diagnostics
                .iter()
                .all(|d| d.severity == crate::diagnostics::Severity::Warning)
        );
    }

    #[test]
    fn pointer_malformed_json_is_a_diagnostic_error() {
        let (_tmp, _state, repo) = temp_setup();
        std::fs::create_dir_all(repo.join(".nopal/checkpoints")).unwrap();
        std::fs::write(repo.join(".nopal/checkpoints/latest.json"), "{ not json").unwrap();

        let report = ledger_pointer(&repo).unwrap();
        assert!(!report.ok);
        assert_eq!(report.entries.len(), 0);
        assert_eq!(
            report.diagnostics[0].code,
            crate::diagnostics::Code::LedgerEntryInvalid
        );
    }

    #[test]
    fn pointer_non_object_latest_is_a_diagnostic_error() {
        let (_tmp, _state, repo) = temp_setup();
        std::fs::create_dir_all(repo.join(".nopal/checkpoints")).unwrap();
        std::fs::write(
            repo.join(".nopal/checkpoints/latest.json"),
            r#"{ "latest": [] }"#,
        )
        .unwrap();

        let report = ledger_pointer(&repo).unwrap();
        assert!(!report.ok);
        assert_eq!(
            report.diagnostics[0].code,
            crate::diagnostics::Code::LedgerEntryInvalid
        );
    }

    // -------------------------------------------------------------------
    // prune
    // -------------------------------------------------------------------

    /// Backdate a run's `updated_at` directly on disk (bypassing every store
    /// command) so staleness tests don't need to pin the real clock.
    fn age_run_dir(state: &Path, flow: &str, run_id: &str, updated_at: &str) -> std::path::PathBuf {
        let run_dir = state
            .join("runs")
            .join(flow)
            .join("unknown-repo")
            .join(run_id);
        let path = run_dir.join("run.json");
        let mut value = store::read_json(&path).unwrap();
        if let Some(map) = value.as_object_mut() {
            map.insert(
                "updated_at".to_owned(),
                Value::String(updated_at.to_owned()),
            );
        }
        store::write_json_durable(&path, &value).unwrap();
        run_dir
    }

    #[test]
    fn prune_dry_run_lists_stale_candidates_without_writing() {
        let (_tmp, state, repo) = temp_setup();
        let stale = init(&repo, &state, Some("stale-1"), None);
        assert!(stale.ok);
        let run_dir = age_run_dir(&state, "kickoff", "stale-1", "2020-01-01T00:00:00+00:00");
        let before = std::fs::read_to_string(run_dir.join("run.json")).unwrap();

        let fresh = init(&repo, &state, Some("fresh-1"), None);
        assert!(fresh.ok);

        let report = ledger_prune(
            &repo,
            Some(&state),
            &PruneArgs {
                stale_after_hours: 24,
                apply: false,
            },
        )
        .unwrap();
        assert!(report.ok);
        assert!(!report.apply);
        assert_eq!(report.selected, 1);
        assert_eq!(report.applied, 0);
        assert_eq!(report.candidates[0].run_id, "stale-1");
        assert!(!report.candidates[0].finalized);

        let after = std::fs::read_to_string(run_dir.join("run.json")).unwrap();
        assert_eq!(before, after, "dry-run must leave run.json byte-identical");

        let toon = prune_toon(&report);
        assert!(toon.contains("kind: nopal.run_ledger.prune/v1"));
        assert!(toon.contains("stale-1"));
    }

    #[test]
    fn prune_apply_finalizes_selected_runs_and_they_drop_out_of_the_live_view() {
        let (_tmp, state, repo) = temp_setup();
        init(&repo, &state, Some("stale-1"), None);
        let run_dir = age_run_dir(&state, "kickoff", "stale-1", "2020-01-01T00:00:00+00:00");

        let report = ledger_prune(
            &repo,
            Some(&state),
            &PruneArgs {
                stale_after_hours: 24,
                apply: true,
            },
        )
        .unwrap();
        assert_eq!(report.selected, 1);
        assert_eq!(report.applied, 1);
        assert!(report.candidates[0].finalized);

        let run = store::read_json(&run_dir.join("run.json")).unwrap();
        assert_eq!(run["status"], "interrupted");
        assert!(run.get("finalized_at").is_some());

        // Finalize bumped updated_at to now, but closed-ness (not staleness)
        // is what must exclude it - a second prune pass selects nothing.
        let again = ledger_prune(
            &repo,
            Some(&state),
            &PruneArgs {
                stale_after_hours: 24,
                apply: false,
            },
        )
        .unwrap();
        assert_eq!(again.selected, 0);

        // And it must also be gone from field's default live view.
        let field_report = crate::field_store::field_status(
            &repo,
            Some(&state),
            None,
            false,
            crate::field::DEFAULT_STALE_AFTER_HOURS,
        )
        .unwrap();
        assert_eq!(
            field_report.total, 0,
            "finalized run must leave the default field view immediately"
        );
    }

    #[test]
    fn pointer_toon_and_json_come_from_the_same_report() {
        let (_tmp, _state, repo) = temp_setup();
        std::fs::create_dir_all(repo.join(".nopal/checkpoints")).unwrap();
        std::fs::write(
            repo.join(".nopal/checkpoints/latest.json"),
            r#"{ "latest": { "kickoff_start": {
                "event": "kickoff_start", "path": "plans/x.md"
            } } }"#,
        )
        .unwrap();

        let report = ledger_pointer(&repo).unwrap();
        let toon = pointer_toon(&report);
        let json = serde_json::to_value(&report).unwrap();
        assert!(toon.contains("kind: nopal.run_ledger.pointer/v1"));
        assert_eq!(json["kind"], "nopal.run_ledger.pointer/v1");
        assert_eq!(json["source"], ".nopal/checkpoints/latest.json");
    }
}
