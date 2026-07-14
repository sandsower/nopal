//! `nopal.field/v1`: the live-field state query - the pure half.
//!
//! The field renders and routes but never decides: every
//! fact it shows must come from this seam. `project` composes three stores -
//! the run ledger, the pending-asks store, and the optional `rondo.core/v1`
//! run-events feed - into one envelope keyed on the ledger run id.
//! It invents no state: every output field is a projection of an input value.
//!
//! Everything here is values in / values out. The cross-repo filesystem scan,
//! the clock, and the optional rondo feed read live in `field_store`.

use ledger_json::Value;
use nopal_ledger_json as ledger_json;
use serde::Serialize;

use crate::ask::{self, AskState};
use crate::diagnostics::{Code, Diagnostic, Severity};
use crate::plot::PlotDocument;
use crate::run_ledger::{self as run_ledger, Status};
use crate::toon::{self, Value as Toon};

pub const FIELD_KIND: &str = "nopal.field/v1";

/// Default staleness threshold: an incomplete, unfinalized run
/// whose `updated_at` is at least this many hours behind the observation
/// clock reads as `stale`. `nopal field inspect --stale-after <hours>` and `nopal
/// ledger prune --stale-after <hours>` both default to this constant so the
/// two surfaces agree on what "stale" means unless a caller overrides it.
pub const DEFAULT_STALE_AFTER_HOURS: u64 = 24;

// ---------------------------------------------------------------------------
// Inputs (produced by the effectful store, consumed by the pure projection)
// ---------------------------------------------------------------------------

/// One scanned run: its `run.json` document and its latest-attempt-per-gate
/// history (`run_ledger_store::collect_gate_history`).
#[derive(Debug, Clone)]
pub struct RunInput {
    pub entry: Value,
    pub gates: Vec<Value>,
}

/// Whether a rondo feed was requested, and if so its parsed form.
#[derive(Debug, Clone)]
pub enum RondoFeed {
    /// No `--rondo-events` was supplied.
    NotRequested,
    /// A feed path was supplied but could not be read or parsed. The store
    /// records the path-bearing warning; the projection only notes the gap.
    Unreadable,
    /// A parsed `rondo.core/v1` `run.events` response document.
    Parsed(Value),
}

/// Everything the projection needs, as plain data. Assembled by `field_store`.
#[derive(Debug, Clone)]
pub struct FieldInputs {
    /// Durable Plot facts, already validated by the Plot store.
    pub plots: Vec<PlotDocument>,
    pub runs: Vec<RunInput>,
    /// Persisted ask documents, one per pending/terminal ask across all repos.
    pub asks: Vec<Value>,
    pub rondo: RondoFeed,
    /// Include completed runs and every ask state (default: the live field only).
    pub include_all: bool,
    /// The observation clock, so overdue asks read as expired without a write.
    pub now_iso: String,
    /// Hours an incomplete, unfinalized run's `updated_at` may age before it
    /// reads as `stale` (see `is_stale`).
    pub stale_after_hours: u64,
    /// Warnings raised while scanning (unreadable runs/asks, bad rondo feed).
    pub scan_warnings: Vec<Diagnostic>,
}

// ---------------------------------------------------------------------------
// Output envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct Placement {
    pub repo: String,
    pub repo_hash: String,
    pub branch: String,
    pub run_dir: String,
    pub flow: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvidencePointer {
    pub artifact_kind: String,
    pub uri: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RondoRunFacts {
    pub run_id: String,
    pub status: Option<String>,
    pub evidence: Vec<EvidencePointer>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldEntry {
    pub run_id: String,
    pub flow: String,
    pub skill: String,
    pub status: String,
    pub ticket_id: String,
    pub branch: String,
    pub started_at: String,
    pub updated_at: String,
    pub placement: Placement,
    /// Latest attempt per gate (name, scope, status, classification, path).
    pub gates: Vec<Value>,
    /// Rondo status/evidence when a feed run id matches this ledger run id.
    pub rondo: Option<RondoRunFacts>,
    /// Pending policy asks backed by this run.
    pub asks: Vec<Value>,
    /// Incomplete, unfinalized, and older than the staleness threshold
    /// (`--stale-after`/`DEFAULT_STALE_AFTER_HOURS`). Always computed, even
    /// under `--all`, where stale entries stay visible instead of vanishing.
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct RondoFeedStatus {
    /// `connected` when a feed parsed, else `absent`.
    pub status: &'static str,
    /// Distinct rondo run ids observed in the feed.
    pub observed_runs: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct FieldReport {
    pub kind: &'static str,
    pub ok: bool,
    pub total: usize,
    /// Count of runs matching `stale` across the whole scan, independent of
    /// `--all` (this is the population `nopal ledger prune` would select).
    pub stale_total: u64,
    pub rondo_feed: RondoFeedStatus,
    /// Additive Plot-first projection. Existing v1 consumers may ignore it.
    pub plots: Vec<PlotDocument>,
    pub entries: Vec<FieldEntry>,
    /// Pending asks with no backing run (session-scoped only).
    pub asks_unbound: Vec<Value>,
    pub diagnostics: Vec<Diagnostic>,
}

// ---------------------------------------------------------------------------
// Projection
// ---------------------------------------------------------------------------

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

fn nested_str(value: &Value, outer: &str, inner: &str) -> String {
    value
        .get(outer)
        .and_then(|v| v.get(inner))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_owned()
}

/// The ask id space is `run_id`; an ask with an empty/absent run id is
/// session-scoped only and lands in `asks_unbound`.
fn ask_run_id(ask: &Value) -> Option<String> {
    ask.get("run_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

/// The state an ask reads as *now*, materializing an overdue pending ask as
/// expired for display without mutating the stored doc (the mutation is left
/// to the ask store's own read path). Fail-closed: an overdue pending ask is
/// already a `deny`, so treating it as expired changes only the label.
fn effective_ask_state(ask: &Value, now_iso: &str) -> AskState {
    let stored = ask::state_of(ask);
    let expires = ask.get("expires_at").and_then(Value::as_str);
    if stored == AskState::Pending && ask::is_past_expiry(now_iso, expires) {
        AskState::Expired
    } else {
        stored
    }
}

/// Seconds between `updated_at` and `now_iso`, or `None` when either string
/// does not parse as a plain `run_ledger::iso_utc` timestamp (an unreadable
/// clock reads as "unknown age", never as stale by default - see `is_stale`).
fn age_seconds(updated_at: &str, now_iso: &str) -> Option<i64> {
    let now = run_ledger::epoch_from_iso(now_iso)?;
    let updated = run_ledger::epoch_from_iso(updated_at)?;
    Some(now.saturating_sub(updated))
}

/// Whole hours since `updated_at`, floored, for display only (`nopal ledger
/// prune`'s dry-run listing). `None` mirrors `age_seconds`.
pub(crate) fn age_hours(updated_at: &str, now_iso: &str) -> Option<u64> {
    age_seconds(updated_at, now_iso).map(|secs| u64::try_from(secs.max(0) / 3600).unwrap_or(0))
}

/// A run is stale when its status is incomplete, it has not been finalized
/// (`finalized_at` absent/empty - `record_finalize` stamps it even though it
/// also bumps `updated_at`, which is exactly why closed-ness cannot be
/// derived from age alone), and `updated_at` is at least `stale_after_hours`
/// old. The boundary is inclusive (age == threshold counts as stale),
/// mirroring `ask::is_past_expiry`'s fail-closed convention. An unparsable
/// clock reads as not stale rather than panicking or guessing.
pub(crate) fn is_stale(
    status: Option<Status>,
    finalized_at: &str,
    updated_at: &str,
    now_iso: &str,
    stale_after_hours: u64,
) -> bool {
    let Some(status) = status else {
        return false;
    };
    if !finalized_at.is_empty() || !status.is_incomplete() {
        return false;
    }
    let threshold = i64::try_from(stale_after_hours)
        .unwrap_or(i64::MAX)
        .saturating_mul(3600);
    age_seconds(updated_at, now_iso).is_some_and(|age| age >= threshold)
}

/// Group rondo `run.events` by run id: latest `status_changed` wins by
/// sequence, and every `evidence_recorded` pointer is kept in feed order.
fn rondo_runs(feed: &Value) -> Vec<RondoRunFacts> {
    let mut facts: Vec<RondoRunFacts> = Vec::new();
    let mut status_seq: Vec<i64> = Vec::new();
    let events = feed.get("events").and_then(Value::as_array);
    let Some(events) = events else {
        return facts;
    };
    for event in events {
        let ty = event.get("type").and_then(Value::as_str).unwrap_or("");
        let Some(run_id) = event
            .get("run_id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let idx = match facts.iter().position(|f| f.run_id == run_id) {
            Some(idx) => idx,
            None => {
                facts.push(RondoRunFacts {
                    run_id: run_id.to_owned(),
                    status: None,
                    evidence: Vec::new(),
                });
                status_seq.push(-1);
                facts.len() - 1
            }
        };
        let sequence = event.get("sequence").and_then(Value::as_i64).unwrap_or(0);
        match ty {
            "rondo.run.status_changed" if sequence >= status_seq[idx] => {
                facts[idx].status = event
                    .get("status")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                status_seq[idx] = sequence;
            }
            "rondo.run.evidence_recorded" => {
                let kind = str_field(event, "artifact_kind");
                let uri = str_field(event, "uri");
                if !uri.is_empty() {
                    facts[idx].evidence.push(EvidencePointer {
                        artifact_kind: kind,
                        uri,
                    });
                }
            }
            _ => {}
        }
    }
    facts
}

/// Project the three stores into one `nopal.field/v1` envelope. Pure and
/// deterministic: the same inputs always yield byte-identical output.
pub fn project(inputs: &FieldInputs) -> FieldReport {
    let mut diagnostics = inputs.scan_warnings.clone();
    let mut plots = inputs.plots.clone();
    plots.sort_by(|left, right| left.plot_id.cmp(&right.plot_id));

    // Pending policy asks, grouped by backing run. Default view keeps only
    // live (pending) asks; --all keeps every state.
    let mut asks_by_run: Vec<(String, Vec<Value>)> = Vec::new();
    let mut asks_unbound: Vec<Value> = Vec::new();
    let mut ask_docs: Vec<&Value> = inputs.asks.iter().collect();
    ask_docs.sort_by_key(|a| str_field(a, "ask_id"));
    for ask in ask_docs {
        let live = effective_ask_state(ask, &inputs.now_iso) == AskState::Pending;
        if !inputs.include_all && !live {
            continue;
        }
        match ask_run_id(ask) {
            Some(run_id) => match asks_by_run.iter_mut().find(|(id, _)| *id == run_id) {
                Some((_, list)) => list.push(ask.clone()),
                None => asks_by_run.push((run_id, vec![ask.clone()])),
            },
            None => asks_unbound.push(ask.clone()),
        }
    }

    // Rondo facts, matched to ledger runs by run id only (no fabricated map).
    let rondo_facts = match &inputs.rondo {
        RondoFeed::Parsed(feed) => rondo_runs(feed),
        RondoFeed::NotRequested | RondoFeed::Unreadable => Vec::new(),
    };
    let rondo_status = match &inputs.rondo {
        RondoFeed::Parsed(_) => "connected",
        RondoFeed::NotRequested | RondoFeed::Unreadable => "absent",
    };

    // Runs -> field entries, deterministically ordered.
    let mut runs: Vec<&RunInput> = inputs.runs.iter().collect();
    runs.sort_by(|a, b| {
        str_field(&a.entry, "run_id")
            .cmp(&str_field(&b.entry, "run_id"))
            .then(str_field(&a.entry, "flow").cmp(&str_field(&b.entry, "flow")))
            .then(str_field(&a.entry, "repo_hash").cmp(&str_field(&b.entry, "repo_hash")))
    });

    let mut entries = Vec::new();
    let mut matched_rondo = std::collections::BTreeSet::new();
    let mut stale_total: u64 = 0;
    for run in runs {
        let entry = &run.entry;
        let status_text = str_field(entry, "status");
        let status = Status::parse(&status_text);
        let finalized_at = str_field(entry, "finalized_at");
        let closed = !finalized_at.is_empty();
        let updated_at = str_field(entry, "updated_at");
        let stale = is_stale(
            status,
            &finalized_at,
            &updated_at,
            &inputs.now_iso,
            inputs.stale_after_hours,
        );
        if stale {
            stale_total += 1;
        }
        // Default live view: incomplete, not stale, not closed. `closed` is
        // checked independently of `stale` - a just-finalized run bumps
        // `updated_at` to now (so it reads as fresh, not stale) but must
        // still leave the live view immediately.
        let live = status.is_some_and(Status::is_incomplete) && !stale && !closed;
        if !inputs.include_all && !live {
            continue;
        }
        let run_id = str_field(entry, "run_id");
        let flow = str_field(entry, "flow");
        let asks = asks_by_run
            .iter()
            .find(|(id, _)| *id == run_id)
            .map(|(_, list)| list.clone())
            .unwrap_or_default();
        let rondo = rondo_facts.iter().find(|f| f.run_id == run_id).cloned();
        if rondo.is_some() {
            matched_rondo.insert(run_id.clone());
        }
        entries.push(FieldEntry {
            placement: Placement {
                repo: str_field(entry, "repo"),
                repo_hash: str_field(entry, "repo_hash"),
                branch: str_field(entry, "branch"),
                run_dir: nested_str(entry, "paths", "run_dir"),
                flow: flow.clone(),
            },
            run_id,
            flow,
            skill: str_field(entry, "skill"),
            status: status_text,
            ticket_id: str_field(entry, "ticket_id"),
            branch: str_field(entry, "branch"),
            started_at: str_field(entry, "started_at"),
            updated_at,
            gates: run.gates.clone(),
            rondo,
            asks,
            stale,
        });
    }

    // Honest diagnostics about the three composition seams.
    match &inputs.rondo {
        RondoFeed::NotRequested => diagnostics.push(Diagnostic {
            severity: Severity::Info,
            code: Code::FieldRondoFeedAbsent,
            path: "rondo".to_owned(),
            position: None,
            message:
                "rondo feed not connected; pass --rondo-events <feed> to attach run status/events"
                    .to_owned(),
        }),
        RondoFeed::Unreadable => diagnostics.push(Diagnostic {
            severity: Severity::Info,
            code: Code::FieldRondoFeedUnreadable,
            path: "rondo".to_owned(),
            position: None,
            message: "rondo feed unreadable; run status/events omitted".to_owned(),
        }),
        RondoFeed::Parsed(_) => {
            let unmatched = rondo_facts.len() - matched_rondo.len();
            if unmatched > 0 {
                diagnostics.push(Diagnostic {
                    severity: Severity::Info,
                    code: Code::FieldRondoUnmatched,
                    path: "rondo".to_owned(),
                    position: None,
                    message: format!(
                        "{unmatched} rondo run(s) observed with no matching ledger run; run-id bridging is future work"
                    ),
                });
            }
        }
    }
    diagnostics.push(Diagnostic {
        severity: Severity::Info,
        code: Code::FieldPartialCoverage,
        path: "field".to_owned(),
        position: None,
        message: "field view is partial by construction: only runs written through the nopal ledger are visible".to_owned(),
    });

    FieldReport {
        kind: FIELD_KIND,
        ok: true,
        total: entries.len(),
        stale_total,
        rondo_feed: RondoFeedStatus {
            status: rondo_status,
            observed_runs: rondo_facts.len(),
        },
        plots,
        entries,
        asks_unbound,
        diagnostics,
    }
}

// ---------------------------------------------------------------------------
// TOON rendering
// ---------------------------------------------------------------------------

fn cell(s: impl Into<String>) -> Toon {
    let s = s.into();
    if s.is_empty() {
        Toon::str("-")
    } else {
        Toon::str(s)
    }
}

fn entries_table(entries: &[FieldEntry]) -> Toon {
    Toon::Table {
        fields: [
            "run_id",
            "flow",
            "status",
            "stale",
            "branch",
            "repo_hash",
            "gates",
            "asks",
            "rondo",
        ]
        .iter()
        .map(|s| (*s).to_owned())
        .collect(),
        rows: entries
            .iter()
            .map(|e| {
                vec![
                    cell(e.run_id.clone()),
                    cell(e.flow.clone()),
                    cell(e.status.clone()),
                    Toon::Bool(e.stale),
                    cell(e.branch.clone()),
                    cell(e.placement.repo_hash.clone()),
                    Toon::Int(e.gates.len() as i64),
                    Toon::Int(e.asks.len() as i64),
                    cell(
                        e.rondo
                            .as_ref()
                            .map(|r| r.status.clone().unwrap_or_else(|| "observed".to_owned()))
                            .unwrap_or_default(),
                    ),
                ]
            })
            .collect(),
    }
}

pub fn report_toon(report: &FieldReport) -> String {
    toon::encode(&[
        ("kind".into(), Toon::str(report.kind)),
        ("ok".into(), Toon::Bool(report.ok)),
        ("total".into(), Toon::Int(report.total as i64)),
        ("plots_total".into(), Toon::Int(report.plots.len() as i64)),
        ("stale_total".into(), Toon::Int(report.stale_total as i64)),
        (
            "rondo_feed".into(),
            Toon::Obj(vec![
                ("status".into(), Toon::str(report.rondo_feed.status)),
                (
                    "observed_runs".into(),
                    Toon::Int(report.rondo_feed.observed_runs as i64),
                ),
            ]),
        ),
        ("field".into(), entries_table(&report.entries)),
        (
            "asks_unbound".into(),
            Toon::Int(report.asks_unbound.len() as i64),
        ),
        (
            "diagnostics".into(),
            crate::diagnostics::toon_table(&report.diagnostics),
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW: &str = "2026-07-06T12:00:00+00:00";

    fn run(run_id: &str, flow: &str, repo_hash: &str, status: &str) -> RunInput {
        run_at(
            run_id,
            flow,
            repo_hash,
            status,
            "2026-07-06T11:00:00+00:00",
            None,
        )
    }

    /// Like `run`, but with explicit `updated_at` and optional `finalized_at`
    /// so staleness/closed-ness tests can pin exact ages and boundaries.
    fn run_at(
        run_id: &str,
        flow: &str,
        repo_hash: &str,
        status: &str,
        updated_at: &str,
        finalized_at: Option<&str>,
    ) -> RunInput {
        let mut entry = ledger_json::json!({
            "kind": "run-ledger-v1",
            "run_id": run_id,
            "flow": flow,
            "repo": format!("/work/{repo_hash}"),
            "repo_hash": repo_hash,
            "branch": "feature/x",
            "skill": "kickoff",
            "ticket_id": "TASK-30",
            "status": status,
            "started_at": "2026-07-06T10:00:00+00:00",
            "updated_at": updated_at,
            "paths": {"run_dir": format!("/state/runs/{flow}/{repo_hash}/{run_id}")},
        });
        if let Some(finalized_at) = finalized_at
            && let Some(map) = entry.as_object_mut()
        {
            map.insert(
                "finalized_at".to_owned(),
                Value::String(finalized_at.to_owned()),
            );
        }
        RunInput {
            entry,
            gates: vec![ledger_json::json!({
                "name": "fmt", "scope": "repo", "attempt": 1,
                "status": "pass", "classification": "code_pass",
            })],
        }
    }

    fn ask(ask_id: &str, run_id: Option<&str>, state: &str, expires: Option<&str>) -> Value {
        ledger_json::json!({
            "kind": "nopal.ask/v1",
            "ask_id": ask_id,
            "run_id": run_id,
            "action": "git.push",
            "mode": "unattended_auto",
            "state": state,
            "expires_at": expires,
        })
    }

    fn rondo_feed() -> Value {
        ledger_json::json!({
            "events": [
                {"type": "rondo.run.status_changed", "sequence": 2, "run_id": "run-a", "status": "running"},
                {"type": "rondo.run.status_changed", "sequence": 3, "run_id": "run-a", "status": "completed"},
                {"type": "rondo.run.evidence_recorded", "sequence": 4, "run_id": "run-a",
                 "artifact_kind": "agent_events", "uri": "rondo-run://run-a/artifacts/events.ndjson"},
                {"type": "rondo.run.status_changed", "sequence": 5, "run_id": "ghost", "status": "failed"}
            ],
            "next_event_cursor": "rondo.core/v1:5"
        })
    }

    fn inputs(include_all: bool, rondo: RondoFeed) -> FieldInputs {
        FieldInputs {
            plots: Vec::new(),
            runs: vec![
                run("run-a", "kickoff", "repohash0001", "running"),
                run("run-b", "handoff", "repohash0002", "completed"),
            ],
            asks: vec![
                ask("ask-1", Some("run-a"), "pending", None),
                ask("ask-2", None, "pending", None),
                ask(
                    "ask-3",
                    Some("run-a"),
                    "pending",
                    Some("1970-01-01T00:00:00+00:00"),
                ),
                ask("ask-4", Some("run-b"), "approved", None),
            ],
            rondo,
            include_all,
            now_iso: NOW.to_owned(),
            stale_after_hours: DEFAULT_STALE_AFTER_HOURS,
            scan_warnings: Vec::new(),
        }
    }

    #[test]
    fn default_view_is_live_runs_and_pending_asks_only() {
        let report = project(&inputs(false, RondoFeed::NotRequested));
        // run-b is completed -> excluded from the live field.
        assert_eq!(report.total, 1);
        assert_eq!(report.entries[0].run_id, "run-a");
        // ask-3 is overdue -> reads as expired -> dropped; ask-4 is approved.
        assert_eq!(report.entries[0].asks.len(), 1);
        assert_eq!(report.entries[0].asks[0]["ask_id"], "ask-1");
        // ask-2 is an unbound pending ask.
        assert_eq!(report.asks_unbound.len(), 1);
        assert_eq!(report.asks_unbound[0]["ask_id"], "ask-2");
        assert_eq!(report.rondo_feed.status, "absent");
        assert!(report.entries[0].rondo.is_none());
    }

    #[test]
    fn all_view_includes_completed_runs_and_terminal_asks() {
        let report = project(&inputs(true, RondoFeed::NotRequested));
        assert_eq!(report.total, 2);
        // run-b now visible with its approved ask.
        let run_b = report.entries.iter().find(|e| e.run_id == "run-b").unwrap();
        assert_eq!(run_b.status, "completed");
        assert_eq!(run_b.asks.len(), 1);
        assert_eq!(run_b.asks[0]["state"], "approved");
    }

    #[test]
    fn placement_and_gates_are_pure_projections_of_the_ledger() {
        let report = project(&inputs(false, RondoFeed::NotRequested));
        let e = &report.entries[0];
        assert_eq!(e.placement.repo_hash, "repohash0001");
        assert_eq!(e.placement.branch, "feature/x");
        assert_eq!(
            e.placement.run_dir,
            "/state/runs/kickoff/repohash0001/run-a"
        );
        assert_eq!(e.placement.flow, "kickoff");
        assert_eq!(e.gates.len(), 1);
        assert_eq!(e.gates[0]["name"], "fmt");
    }

    #[test]
    fn rondo_facts_attach_by_run_id_and_unmatched_runs_diagnose() {
        let report = project(&inputs(false, RondoFeed::Parsed(rondo_feed())));
        assert_eq!(report.rondo_feed.status, "connected");
        assert_eq!(report.rondo_feed.observed_runs, 2); // run-a + ghost
        let rondo = report.entries[0].rondo.as_ref().unwrap();
        assert_eq!(rondo.run_id, "run-a");
        assert_eq!(rondo.status.as_deref(), Some("completed")); // latest by sequence
        assert_eq!(rondo.evidence.len(), 1);
        assert_eq!(rondo.evidence[0].artifact_kind, "agent_events");
        // ghost has no ledger run -> unmatched info diagnostic.
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == Code::FieldRondoUnmatched)
        );
    }

    #[test]
    fn partial_coverage_diagnostic_is_always_present() {
        let report = project(&inputs(false, RondoFeed::NotRequested));
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == Code::FieldPartialCoverage && d.severity == Severity::Info)
        );
    }

    #[test]
    fn projection_is_deterministic() {
        let a = project(&inputs(true, RondoFeed::Parsed(rondo_feed())));
        let b = project(&inputs(true, RondoFeed::Parsed(rondo_feed())));
        assert_eq!(report_toon(&a), report_toon(&b));
    }

    #[test]
    fn plot_execution_and_fruit_facts_project_without_mutating_assurance_facts() {
        let plot: PlotDocument = serde_json::from_value(serde_json::json!({
            "kind": "nopal.plot/v1",
            "plot_id": "plot-1",
            "title": "Execution Plot",
            "provisional": false,
            "progress": "active",
            "conditions": ["keep-condition"],
            "seed": {"source": "test", "text": "seed"},
            "intent": "Dogfood the execution flow",
            "fruit": {"state": "absent"},
            "sessions": [],
            "selected_session_id": null,
            "executions": [{
                "service_id": "rondo-core",
                "repo_id": "repo-1",
                "run_id": "run-1",
                "manifest_sha256": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "status": "completed",
                "outcome": "completed",
                "event_cursor": "rondo.core/v1:4",
                "evidence": [{
                    "artifact_kind": "delivery_artifact",
                    "uri": "rondo-run://run-1/artifacts/delivery.json"
                }],
                "created_at": "t0",
                "updated_at": "t1"
            }],
            "repositories": [{
                "repository_id": "repo-1",
                "root": "/repo",
                "configuration_root": "/repo",
                "revision": "abc",
                "process_artifact_hash": "process-1",
                "roots": [{
                    "id": "quality",
                    "statement": "Quality remains green",
                    "proof_requirements": [{
                        "id": "pre-pr",
                        "stage": "pre_pr",
                        "required": true,
                        "gates": ["test"],
                        "on_missing": "block",
                        "on_failure": "block"
                    }]
                }],
                "gate_ids": ["test"],
                "policy_hash": "policy-1"
            }],
            "created_at": "t0",
            "updated_at": "t1"
        }))
        .unwrap();
        let mut field_inputs = inputs(true, RondoFeed::NotRequested);
        field_inputs.plots = vec![plot.clone()];

        let value = serde_json::to_value(project(&field_inputs)).unwrap();
        let projected = &value["plots"][0];

        assert_eq!(projected["fruit"]["state"], "absent");
        assert_eq!(projected["executions"][0]["service_id"], "rondo-core");
        assert_eq!(projected["executions"][0]["repo_id"], "repo-1");
        assert_eq!(projected["executions"][0]["run_id"], "run-1");
        assert_eq!(projected["executions"][0]["status"], "completed");
        assert_eq!(
            projected["executions"][0]["evidence"][0]["uri"],
            "rondo-run://run-1/artifacts/delivery.json"
        );
        assert_eq!(projected["progress"], plot.progress);
        assert_eq!(projected["conditions"], serde_json::json!(plot.conditions));
        assert_eq!(
            projected["repositories"][0]["roots"],
            serde_json::to_value(&plot.repositories[0].roots).unwrap()
        );
    }

    #[test]
    fn unreadable_feed_degrades_without_failing() {
        let report = project(&inputs(false, RondoFeed::Unreadable));
        assert!(report.ok);
        assert_eq!(report.rondo_feed.status, "absent");
        assert!(
            report
                .diagnostics
                .iter()
                .any(|d| d.code == Code::FieldRondoFeedUnreadable)
        );
        assert!(
            !report
                .diagnostics
                .iter()
                .any(|d| d.code == Code::FieldRondoFeedAbsent)
        );
    }

    // -- staleness --------------------------------------------------

    fn stale_inputs(runs: Vec<RunInput>, include_all: bool, stale_after_hours: u64) -> FieldInputs {
        FieldInputs {
            plots: Vec::new(),
            runs,
            asks: Vec::new(),
            rondo: RondoFeed::NotRequested,
            include_all,
            now_iso: NOW.to_owned(),
            stale_after_hours,
            scan_warnings: Vec::new(),
        }
    }

    #[test]
    fn stale_boundary_is_inclusive_at_the_threshold() {
        // NOW = 2026-07-06T12:00:00+00:00, threshold 24h.
        let cases = [
            (
                "2026-07-05T12:00:01+00:00",
                false,
                "23h59m59s: not yet stale",
            ),
            (
                "2026-07-05T12:00:00+00:00",
                true,
                "exactly 24h: inclusive boundary",
            ),
            ("2026-07-05T11:59:59+00:00", true, "24h00m01s: stale"),
        ];
        for (updated_at, expected_stale, msg) in cases {
            let run = run_at(
                "run-x",
                "kickoff",
                "repohash0001",
                "running",
                updated_at,
                None,
            );
            let report = project(&stale_inputs(vec![run], true, 24));
            assert_eq!(report.entries[0].stale, expected_stale, "{msg}");
        }
    }

    #[test]
    fn interrupted_and_failed_are_stale_able_completed_never_is() {
        let old = "2020-01-01T00:00:00+00:00";
        let runs = vec![
            run_at("run-a", "kickoff", "repohash0001", "interrupted", old, None),
            run_at("run-b", "kickoff", "repohash0002", "failed", old, None),
            run_at("run-c", "kickoff", "repohash0003", "completed", old, None),
        ];
        let report = project(&stale_inputs(runs, true, 24));
        let stale_ids: Vec<&str> = report
            .entries
            .iter()
            .filter(|e| e.stale)
            .map(|e| e.run_id.as_str())
            .collect();
        assert_eq!(stale_ids, vec!["run-a", "run-b"]);
        assert_eq!(report.stale_total, 2);
    }

    #[test]
    fn finalized_run_is_never_stale_and_leaves_the_live_view_even_when_fresh() {
        // A just-finalized run bumps updated_at to now (fresh) but must still
        // leave the default live view immediately - the flood bug this
        // feature exists to prevent.
        let fresh = run_at(
            "run-a",
            "kickoff",
            "repohash0001",
            "interrupted",
            NOW,
            Some(NOW),
        );
        // An old finalized run must never read as stale either.
        let old = "2020-01-01T00:00:00+00:00";
        let stale_but_closed = run_at(
            "run-b",
            "kickoff",
            "repohash0002",
            "interrupted",
            old,
            Some(old),
        );
        let mut inputs = stale_inputs(vec![fresh, stale_but_closed], false, 24);

        let default_report = project(&inputs);
        assert_eq!(
            default_report.total, 0,
            "finalized runs must be absent from the default live view regardless of age"
        );
        assert_eq!(
            default_report.stale_total, 0,
            "finalized runs never count as stale"
        );

        inputs.include_all = true;
        let all_report = project(&inputs);
        assert_eq!(all_report.total, 2, "--all still surfaces closed runs");
        assert!(all_report.entries.iter().all(|e| !e.stale));
    }

    #[test]
    fn all_view_retains_stale_entries_default_view_hides_them() {
        let stale = run_at(
            "run-a",
            "kickoff",
            "repohash0001",
            "running",
            "2020-01-01T00:00:00+00:00",
            None,
        );
        let mut inputs = stale_inputs(vec![stale], false, 24);

        let default_report = project(&inputs);
        assert_eq!(default_report.total, 0);
        assert_eq!(default_report.stale_total, 1);

        inputs.include_all = true;
        let all_report = project(&inputs);
        assert_eq!(all_report.total, 1);
        assert!(all_report.entries[0].stale);
    }

    #[test]
    fn stale_after_zero_marks_every_incomplete_unclosed_run_stale() {
        let fresh = run_at("run-a", "kickoff", "repohash0001", "running", NOW, None);
        let report = project(&stale_inputs(vec![fresh], true, 0));
        assert!(report.entries[0].stale);
        assert_eq!(report.stale_total, 1);
    }
}
