//! Run ledger store - the effectful half of `run-ledger-v1`.
//!
//! Owns everything `run_ledger` deliberately does not: the durable atomic
//! write path (temp file, fsync, rename, fsync parent dir), the `.lock`
//! exclusive-flock guard (std `File::lock`, non-reentrant - sequence holds,
//! never nest), attempt-dir allocation, run discovery under
//! `${BEISLID_STATE_DIR:-~/.local/state/beislid}/runs/<flow>/<repo_hash>/`,
//! and the git subprocess probes (repo root, root-commit hash, branch).
//!
//! `NOPAL_LEDGER_TEST_EPOCH` / `NOPAL_LEDGER_TEST_TOKEN` pin the clock and the
//! run-id token; they exist for the interop write-equivalence tests and are
//! honored unconditionally because a pinned ledger is still a valid ledger.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use ledger_json::Value;
use nopal_ledger_json as ledger_json;

use crate::diagnostics::{Code, Diagnostic};
use crate::run_ledger as core;
use crate::run_ledger::Status;

/// Store failures split into hard IO (CLI exit 2) and domain problems that
/// belong in the envelope as diagnostics (CLI exit 1).
#[derive(Debug)]
pub enum StoreError {
    Io(io::Error),
    Domain(Diagnostic),
}

impl From<io::Error> for StoreError {
    fn from(err: io::Error) -> StoreError {
        StoreError::Io(err)
    }
}

pub(crate) fn domain(
    code: Code,
    path: impl Into<String>,
    message: impl Into<String>,
) -> StoreError {
    StoreError::Domain(Diagnostic::error(code, path, message))
}

// ---------------------------------------------------------------------------
// Effect seams: clock, randomness, environment
// ---------------------------------------------------------------------------

pub(crate) fn epoch_now() -> i64 {
    if let Some(pinned) = std::env::var_os("NOPAL_LEDGER_TEST_EPOCH")
        .and_then(|v| v.into_string().ok())
        .and_then(|v| v.parse::<i64>().ok())
    {
        return pinned;
    }
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(elapsed) => i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX),
        Err(_) => 0,
    }
}

pub(crate) fn now_iso() -> String {
    core::iso_utc(epoch_now())
}

pub(crate) fn now_stamp() -> String {
    core::stamp_utc(epoch_now())
}

/// Python `secrets.token_hex(n)`.
pub(crate) fn token_hex(bytes: usize) -> String {
    if let Some(pinned) =
        std::env::var_os("NOPAL_LEDGER_TEST_TOKEN").and_then(|v| v.into_string().ok())
    {
        return pinned;
    }
    let mut buf = vec![0u8; bytes];
    if getrandom::fill(&mut buf).is_err() {
        // Randomness only makes ids unique; the collision loop below is the
        // correctness backstop, so a degraded fallback beats aborting.
        let fallback = epoch_now().to_le_bytes();
        buf.iter_mut()
            .zip(fallback.iter().cycle())
            .for_each(|(b, f)| *b = *f);
    }
    buf.iter().map(|b| format!("{b:02x}")).collect()
}

/// Python `Path.resolve()` (non-strict): absolutize against the cwd, then
/// resolve symlinks for the longest existing prefix and reattach the rest.
/// The Python tool resolves both the state dir and the repo root, and those
/// resolved forms are embedded in `run.json`, so skipping this would make
/// the two tools write different bytes into the same tree (macOS `/var` vs
/// `/private/var` being the everyday case).
///
/// `pub(crate)`: `discover::project_root` reuses this to put the
/// starting dir and the `git rev-parse --show-toplevel` output through the
/// same resolution before comparing them - otherwise the `/var` vs
/// `/private/var` mismatch above would make an exact-match ancestor walk
/// silently miss the toplevel.
pub(crate) fn resolve_like_python(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut existing = absolute.clone();
    let mut tail = Vec::new();
    loop {
        match fs::canonicalize(&existing) {
            Ok(resolved) => {
                let mut result = resolved;
                for component in tail.iter().rev() {
                    result.push(component);
                }
                return result;
            }
            Err(_) => match (existing.parent(), existing.file_name()) {
                (Some(parent), Some(name)) => {
                    tail.push(name.to_owned());
                    existing = parent.to_path_buf();
                }
                _ => return absolute,
            },
        }
    }
}

/// Resolved effect context for one invocation.
pub struct LedgerEnv {
    pub state_dir: PathBuf,
    pub repo: PathBuf,
    pub repo_hash: String,
}

impl LedgerEnv {
    /// `dir` is the caller's repo root candidate (the CLI `--dir`);
    /// `state_dir_flag` beats `BEISLID_STATE_DIR` beats the XDG default.
    pub fn discover(dir: &Path, state_dir_flag: Option<&Path>) -> LedgerEnv {
        let state_dir = state_dir_flag.map(Path::to_path_buf).unwrap_or_else(|| {
            std::env::var_os("BEISLID_STATE_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    std::env::var_os("HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| PathBuf::from("."))
                        .join(".local")
                        .join("state")
                        .join("beislid")
                })
        });
        let repo = repo_root(dir);
        let repo_hash = repo_hash_of(&repo);
        LedgerEnv {
            state_dir: resolve_like_python(&state_dir),
            repo,
            repo_hash,
        }
    }

    pub fn run_root(&self, flow: &str) -> PathBuf {
        self.state_dir.join("runs").join(flow).join(&self.repo_hash)
    }

    /// Root for this repo's persisted policy asks. Sibling of
    /// `runs/`, keyed by repo hash (not flow): an ask belongs to a session or
    /// run, not a flow. A field lists a repo's asks by scanning this dir and
    /// can sweep every repo with `asks/*/`.
    pub fn ask_root(&self) -> PathBuf {
        self.state_dir.join("asks").join(&self.repo_hash)
    }
}

// ---------------------------------------------------------------------------
// Git probes
// ---------------------------------------------------------------------------
//
// `pub(crate)`: `discover::project_root` reuses `git_stdout` for its
// own `rev-parse --show-toplevel` probe rather than duplicating the
// subprocess/UTF-8/empty-output handling below.

pub(crate) fn git_stdout(dir: &Path, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    if text.is_empty() { None } else { Some(text) }
}

fn repo_root(dir: &Path) -> PathBuf {
    let root = git_stdout(dir, &["rev-parse", "--show-toplevel"])
        .map(PathBuf::from)
        .unwrap_or_else(|| dir.to_path_buf());
    resolve_like_python(&root)
}

/// First 12 chars of the lexically-first root commit, else `unknown-repo`.
fn repo_hash_of(repo: &Path) -> String {
    match git_stdout(repo, &["rev-list", "--max-parents=0", "HEAD"]) {
        Some(out) => {
            let mut roots: Vec<&str> = out
                .lines()
                .map(str::trim)
                .filter(|l| !l.is_empty())
                .collect();
            roots.sort_unstable();
            roots
                .first()
                .map(|r| r.chars().take(12).collect())
                .unwrap_or_else(|| "unknown-repo".to_owned())
        }
        None => "unknown-repo".to_owned(),
    }
}

fn current_branch(repo: &Path) -> String {
    git_stdout(repo, &["branch", "--show-current"]).unwrap_or_else(|| "unknown".to_owned())
}

// ---------------------------------------------------------------------------
// Durable writes and locking
// ---------------------------------------------------------------------------

/// Python `write_json`: temp file in the target dir, write, fsync, rename
/// over the target, fsync the parent dir; the temp file never survives.
pub fn write_json_durable(path: &Path, value: &Value) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("payload.json");
    let tmp = parent.join(format!(".{name}.{}.tmp", token_hex(4)));
    let result = (|| -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(core::json_pretty(value).as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&tmp, path)?;
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() && tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

pub fn read_json(path: &Path) -> Result<Value, StoreError> {
    let text = fs::read_to_string(path)?;
    ledger_json::from_str(&text).map_err(|err| {
        domain(
            Code::LedgerEntryInvalid,
            path.display().to_string(),
            format!("unreadable ledger JSON: {err}"),
        )
    })
}

/// Exclusive cross-process lock on `<run_dir>/.lock`.
///
/// The lock lives on a dedicated file because `write_json_durable` replaces
/// JSON files by rename, which would detach a lock held on the old inode.
/// Like the Python guard it is NOT reentrant: acquiring while the same
/// process already holds it deadlocks. Callers sequence holds, never nest.
pub struct RunLock {
    file: File,
}

impl RunLock {
    pub fn acquire(run_dir: &Path) -> io::Result<RunLock> {
        fs::create_dir_all(run_dir)?;
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(run_dir.join(".lock"))?;
        file.lock()?;
        Ok(RunLock { file })
    }
}

impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

// ---------------------------------------------------------------------------
// Run discovery
// ---------------------------------------------------------------------------

/// Roots that may contain this repo's runs: one per flow, or one when the
/// flow is known. (Legacy flat `runs/<hash>` is deliberately not scanned.)
fn candidate_roots(env: &LedgerEnv, flow: Option<&str>) -> io::Result<Vec<PathBuf>> {
    if let Some(flow) = flow.filter(|f| !f.is_empty()) {
        return Ok(vec![env.run_root(&core::normalize_flow(Some(flow), None))]);
    }
    let runs_root = env.state_dir.join("runs");
    let mut roots = Vec::new();
    if runs_root.is_dir() {
        let mut flows: Vec<PathBuf> = fs::read_dir(&runs_root)?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        flows.sort();
        for flow_dir in flows {
            roots.push(flow_dir.join(&env.repo_hash));
        }
    }
    Ok(roots)
}

pub fn find_run_dir(
    env: &LedgerEnv,
    run_id: &str,
    flow: Option<&str>,
) -> Result<PathBuf, StoreError> {
    // Hardening beyond the Python tool (which validates only on init): a
    // traversal-shaped id would otherwise be joined into the search path and
    // could read or write outside the state dir. Ids that fail this check
    // could never have been created by init, so nothing legitimate is lost.
    if !core::identifier_valid(run_id) {
        return Err(domain(
            Code::RunIdInvalid,
            run_id,
            "invalid run id: use a single path-safe segment [A-Za-z0-9_.-]",
        ));
    }
    let mut matches = Vec::new();
    for root in candidate_roots(env, flow)? {
        let candidate = root.join(run_id);
        if candidate.join("run.json").is_file() {
            matches.push(candidate);
        }
    }
    match matches.len() {
        0 => Err(domain(
            Code::RunNotFound,
            run_id,
            format!("run not found: {run_id}"),
        )),
        1 => Ok(matches.remove(0)),
        _ => Err(domain(
            Code::RunAmbiguous,
            run_id,
            format!("run id is ambiguous; pass --flow to disambiguate: {run_id}"),
        )),
    }
}

// ---------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------

pub struct InitArgs<'a> {
    pub skill: &'a str,
    pub flow: Option<&'a str>,
    pub ticket_id: &'a str,
    pub ticket_title: &'a str,
    pub ticket_url: &'a str,
    pub branch: Option<&'a str>,
    pub run_id: Option<&'a str>,
}

pub struct InitOutcome {
    pub run_id: String,
    pub flow: String,
    pub run_dir: PathBuf,
}

pub fn init_run(env: &LedgerEnv, args: &InitArgs) -> Result<InitOutcome, StoreError> {
    let flow = core::normalize_flow(args.flow, Some(args.skill));
    let explicit = match args.run_id {
        Some(rid) => {
            if !core::identifier_valid(rid) {
                return Err(domain(
                    Code::RunIdInvalid,
                    rid,
                    "invalid run id: use a single path-safe segment [A-Za-z0-9_.-]",
                ));
            }
            Some(rid.to_owned())
        }
        None => None,
    };
    let root = env.run_root(&flow);
    fs::create_dir_all(&root)?;

    let mut rid = explicit
        .clone()
        .unwrap_or_else(|| core::new_run_id(&now_stamp(), &token_hex(3)));
    let mut suffix = 1u32;
    let run_dir = loop {
        let candidate = root.join(&rid);
        match fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {
                if explicit.is_some() {
                    return Err(domain(
                        Code::RunIdCollision,
                        candidate.display().to_string(),
                        format!("run id already exists: {}", candidate.display()),
                    ));
                }
                suffix += 1;
                rid = format!("{}-{suffix}", core::new_run_id(&now_stamp(), &token_hex(3)));
            }
            Err(err) => return Err(err.into()),
        }
    };
    for sub in [
        "artifacts",
        "artifacts/gates",
        "artifacts/reviews",
        "logs",
        "checkpoints",
    ] {
        fs::create_dir_all(run_dir.join(sub))?;
    }

    let started = now_iso();
    let branch = match args.branch.filter(|b| !b.is_empty()) {
        Some(branch) => branch.to_owned(),
        None => current_branch(&env.repo),
    };
    let run_dir_text = run_dir.display().to_string();
    let repo_text = env.repo.display().to_string();
    let ctx = core::InitContext {
        run_id: &rid,
        flow: &flow,
        repo: &repo_text,
        repo_hash: &env.repo_hash,
        branch: &branch,
        skill: args.skill,
        ticket_id: if args.ticket_id.is_empty() {
            "none"
        } else {
            args.ticket_id
        },
        ticket_title: if args.ticket_title.is_empty() {
            "none"
        } else {
            args.ticket_title
        },
        ticket_url: args.ticket_url,
        started_at: &started,
        run_dir: &run_dir_text,
    };
    write_json_durable(&run_dir.join("run.json"), &core::new_run_entry(&ctx))?;
    fs::write(run_dir.join("events.jsonl"), "")?;
    fs::write(run_dir.join("transcript.md"), core::transcript_header(&ctx))?;
    append_event(
        &run_dir,
        "run_initialized",
        &ledger_json::json!({
            "skill": args.skill,
            "flow": flow,
            "ticket": {"id": ctx.ticket_id, "title": ctx.ticket_title, "url": ctx.ticket_url},
            "branch": branch,
        }),
        None,
    )?;
    Ok(InitOutcome {
        run_id: rid,
        flow,
        run_dir,
    })
}

/// Append one event: jsonl line, transcript section, and the run.json count
/// bump, all under a single lock hold.
pub fn append_event(
    run_dir: &Path,
    event_type: &str,
    payload: &Value,
    transcript_summary: Option<&str>,
) -> Result<Value, StoreError> {
    let _lock = RunLock::acquire(run_dir)?;
    let safe_payload = core::redact_json(payload);
    let event = core::event_value(event_type, safe_payload.clone(), &now_iso());
    let mut events_file = OpenOptions::new()
        .append(true)
        .create(true)
        .open(run_dir.join("events.jsonl"))?;
    events_file.write_all(core::json_line(&event).as_bytes())?;
    events_file.write_all(b"\n")?;
    let summary = match transcript_summary {
        Some(text) => core::redact_text(text, core::TEXT_LIMIT),
        None => core::default_transcript_summary(&safe_payload),
    };
    let mut transcript = OpenOptions::new()
        .append(true)
        .create(true)
        .open(run_dir.join("transcript.md"))?;
    transcript.write_all(core::transcript_section(event_type, &summary).as_bytes())?;
    let run_path = run_dir.join("run.json");
    let mut run = read_json(&run_path)?;
    if let Some(map) = run.as_object_mut() {
        let count = map
            .get("events")
            .and_then(|e| e.get("count"))
            .and_then(Value::as_i64)
            .unwrap_or(0);
        // Python `setdefault("events", {})["count"] = ...` preserves any
        // sibling keys inside `events`; only replace when it is not an object.
        match map.get_mut("events") {
            Some(Value::Object(events)) => {
                events.insert("count".to_owned(), Value::from(count + 1));
            }
            _ => {
                map.insert(
                    "events".to_owned(),
                    ledger_json::json!({"count": count + 1}),
                );
            }
        }
        map.insert(
            "updated_at".to_owned(),
            event.get("timestamp").cloned().unwrap_or(Value::Null),
        );
    }
    write_json_durable(&run_path, &run)?;
    Ok(event)
}

/// Write a checkpoint document and fold it into run.json under one lock.
pub fn record_checkpoint(
    run_dir: &Path,
    name: &str,
    payload: &Value,
    resume_hint: Option<&str>,
) -> Result<PathBuf, StoreError> {
    let _lock = RunLock::acquire(run_dir)?;
    let now = now_iso();
    let checkpoint_path = run_dir
        .join("checkpoints")
        .join(core::checkpoint_file_name(name));
    write_json_durable(
        &checkpoint_path,
        &core::checkpoint_value(name, payload, resume_hint, &now),
    )?;
    let run_path = run_dir.join("run.json");
    let mut run = read_json(&run_path)?;
    if let Some(map) = run.as_object_mut() {
        let path_text = checkpoint_path.display().to_string();
        let mut entry = ledger_json::json!({
            "name": name,
            "path": path_text,
            "timestamp": now,
        });
        if let Some(hint) = resume_hint.filter(|h| !h.is_empty()) {
            let redacted = core::redact_text(hint, core::HINT_LIMIT);
            if let Some(entry_map) = entry.as_object_mut() {
                entry_map.insert("resume_hint".to_owned(), Value::String(redacted.clone()));
            }
            map.insert("resume_hint".to_owned(), Value::String(redacted));
        }
        map.insert("latest_checkpoint".to_owned(), entry);
        map.insert(
            "last_checkpoint".to_owned(),
            Value::String(path_text.clone()),
        );
        map.insert("current_step".to_owned(), Value::String(name.to_owned()));
        match map.get_mut("checkpoints") {
            Some(Value::Array(list)) => list.push(Value::String(path_text)),
            _ => {
                map.insert("checkpoints".to_owned(), ledger_json::json!([path_text]));
            }
        }
    }
    write_json_durable(&run_path, &run)?;
    Ok(checkpoint_path)
}

/// Allocate the next numeric attempt dir under `gate_root` (own lock hold).
pub fn next_attempt_dir(run_dir: &Path, gate_root: &Path) -> Result<PathBuf, StoreError> {
    let _lock = RunLock::acquire(run_dir)?;
    fs::create_dir_all(gate_root)?;
    let mut attempt = 1u32;
    loop {
        let candidate = gate_root.join(attempt.to_string());
        match fs::create_dir(&candidate) {
            Ok(()) => return Ok(candidate),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => attempt += 1,
            Err(err) => return Err(err.into()),
        }
    }
}

pub struct GateOutcome {
    pub envelope_path: PathBuf,
    pub checkpoint_path: PathBuf,
}

/// Record one gate attempt: envelope artifact, run.json artifact/log entries,
/// a gate checkpoint, and a `gate_result` event - four sequenced lock holds,
/// exactly like the Python tool.
pub fn record_gate(
    run_dir: &Path,
    name: &str,
    scope: Option<&str>,
    envelope: &Value,
    resume_hint: Option<&str>,
) -> Result<GateOutcome, StoreError> {
    let scope_source = scope
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or_else(|| {
            envelope
                .get("gate")
                .and_then(|g| g.get("scope"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "repo".to_owned());
    let scope = core::slug(&scope_source, "repo");
    let safe_name = core::slug(name, "gate");
    let gate_root = run_dir
        .join("artifacts")
        .join("gates")
        .join(&scope)
        .join(&safe_name);
    let attempt_dir = next_attempt_dir(run_dir, &gate_root)?;
    let envelope_path = attempt_dir.join("envelope.json");
    {
        let _lock = RunLock::acquire(run_dir)?;
        write_json_durable(&envelope_path, &core::redact_json(envelope))?;
        let run_path = run_dir.join("run.json");
        let mut run = read_json(&run_path)?;
        if let Some(map) = run.as_object_mut() {
            let artifact = ledger_json::json!({
                "name": name,
                "path": envelope_path.display().to_string(),
                "kind": "gate",
                "scope": scope,
            });
            for key in ["artifacts", "logs"] {
                match map.get_mut(key) {
                    Some(Value::Array(list)) => list.push(artifact.clone()),
                    _ => {
                        map.insert(key.to_owned(), ledger_json::json!([artifact.clone()]));
                    }
                }
            }
        }
        write_json_durable(&run_path, &run)?;
    }
    let checkpoint_path = record_checkpoint(
        run_dir,
        &format!("gate-{scope}-{safe_name}"),
        &ledger_json::json!({
            "name": name,
            "scope": scope,
            "path": envelope_path.display().to_string(),
            "status": envelope.get("status").cloned().unwrap_or(Value::Null),
            "envelope": envelope,
        }),
        // Python `args.resume_hint or <default>`: empty hints fall back too.
        Some(
            resume_hint
                .filter(|h| !h.is_empty())
                .unwrap_or("continue after reviewing gate result"),
        ),
    )?;
    append_event(
        run_dir,
        "gate_result",
        &ledger_json::json!({
            "name": name,
            "scope": scope,
            "path": envelope_path.display().to_string(),
            "checkpoint": checkpoint_path.display().to_string(),
            "envelope": envelope,
        }),
        None,
    )?;
    Ok(GateOutcome {
        envelope_path,
        checkpoint_path,
    })
}

pub fn record_interrupt(
    run_dir: &Path,
    reason: &str,
    resume_hint: Option<&str>,
) -> Result<PathBuf, StoreError> {
    let checkpoint_path = record_checkpoint(
        run_dir,
        "interrupted",
        &ledger_json::json!({"reason": reason}),
        resume_hint,
    )?;
    {
        let _lock = RunLock::acquire(run_dir)?;
        let run_path = run_dir.join("run.json");
        let mut run = read_json(&run_path)?;
        if let Some(map) = run.as_object_mut() {
            map.insert(
                "status".to_owned(),
                Value::String(Status::Interrupted.as_str().to_owned()),
            );
            map.insert(
                "interruption".to_owned(),
                ledger_json::json!({
                    "timestamp": now_iso(),
                    "reason": core::redact_text(reason, core::TEXT_LIMIT),
                    "checkpoint": checkpoint_path.display().to_string(),
                }),
            );
            if let Some(hint) = resume_hint.filter(|h| !h.is_empty()) {
                map.insert(
                    "resume_hint".to_owned(),
                    Value::String(core::redact_text(hint, core::HINT_LIMIT)),
                );
            }
        }
        write_json_durable(&run_path, &run)?;
    }
    append_event(
        run_dir,
        "interrupted",
        &ledger_json::json!({
            "reason": reason,
            "resume_hint": resume_hint,
            "checkpoint": checkpoint_path.display().to_string(),
        }),
        None,
    )?;
    Ok(checkpoint_path)
}

pub struct FinalizeOutcome {
    pub final_report: Option<PathBuf>,
}

pub fn record_finalize(
    run_dir: &Path,
    status: &str,
    report_file: Option<&Path>,
) -> Result<FinalizeOutcome, StoreError> {
    let status = match Status::parse(status) {
        Some(status) if Status::FINAL.contains(&status) => status,
        _ => {
            return Err(domain(
                Code::LedgerStatusInvalid,
                run_dir.display().to_string(),
                format!(
                    "invalid final status: {status}; expected one of {}",
                    Status::FINAL
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            ));
        }
    };
    let report_path = match report_file {
        Some(source) => {
            let target = run_dir.join("final-report.md");
            fs::copy(source, &target)?;
            Some(target)
        }
        None => None,
    };
    let report_value = report_path
        .as_ref()
        .map(|p| Value::String(p.display().to_string()))
        .unwrap_or(Value::Null);
    let checkpoint_path = record_checkpoint(
        run_dir,
        "final-report",
        &ledger_json::json!({"status": status.as_str(), "final_report": report_value}),
        Some(if status == Status::Completed {
            "run complete"
        } else {
            "inspect final status before resuming"
        }),
    )?;
    {
        let _lock = RunLock::acquire(run_dir)?;
        let run_path = run_dir.join("run.json");
        let mut run = read_json(&run_path)?;
        if let Some(map) = run.as_object_mut() {
            map.insert(
                "status".to_owned(),
                Value::String(status.as_str().to_owned()),
            );
            map.insert("finalized_at".to_owned(), Value::String(now_iso()));
            if let Some(target) = &report_path
                && let Some(Value::Object(paths)) = map.get_mut("paths")
            {
                paths.insert(
                    "final_report".to_owned(),
                    Value::String(target.display().to_string()),
                );
            }
        }
        write_json_durable(&run_path, &run)?;
    }
    append_event(
        run_dir,
        "finalized",
        &ledger_json::json!({
            "status": status.as_str(),
            "final_report": report_path
                .as_ref()
                .map(|p| Value::String(p.display().to_string()))
                .unwrap_or(Value::Null),
            "checkpoint": checkpoint_path.display().to_string(),
        }),
        None,
    )?;
    Ok(FinalizeOutcome {
        final_report: report_path,
    })
}

// ---------------------------------------------------------------------------
// Read surfaces: scanning for resume and dashboard
// ---------------------------------------------------------------------------

pub struct ScannedRun {
    pub entry: Value,
    pub run_dir: PathBuf,
}

pub struct Scan {
    pub runs: Vec<ScannedRun>,
    /// Unreadable or status-invalid runs surface here instead of vanishing.
    pub warnings: Vec<Diagnostic>,
}

/// Read every run for this repo (optionally one flow), keeping only runs
/// whose status parses and passes `allow`.
pub fn scan_runs(
    env: &LedgerEnv,
    flow: Option<&str>,
    allow: impl Fn(Status) -> bool,
) -> Result<Scan, StoreError> {
    let mut runs = Vec::new();
    let mut warnings = Vec::new();
    for root in candidate_roots(env, flow)? {
        if !root.is_dir() {
            continue;
        }
        let mut run_dirs: Vec<PathBuf> = fs::read_dir(&root)?
            .filter_map(|entry| entry.ok().map(|e| e.path()))
            .collect();
        run_dirs.sort();
        for run_dir in run_dirs {
            let run_file = run_dir.join("run.json");
            if !run_file.is_file() {
                continue;
            }
            let entry = match read_json(&run_file) {
                Ok(entry) => entry,
                Err(StoreError::Domain(diag)) => {
                    warnings.push(Diagnostic::warning(
                        Code::LedgerEntryInvalid,
                        run_file.display().to_string(),
                        format!("skipping unreadable run file: {}", diag.message),
                    ));
                    continue;
                }
                Err(StoreError::Io(err)) => {
                    warnings.push(Diagnostic::warning(
                        Code::LedgerEntryInvalid,
                        run_file.display().to_string(),
                        format!("skipping unreadable run file: {err}"),
                    ));
                    continue;
                }
            };
            let status_text = entry.get("status").and_then(Value::as_str).unwrap_or("");
            let Some(status) = Status::parse(status_text) else {
                warnings.push(Diagnostic::warning(
                    Code::LedgerStatusInvalid,
                    run_file.display().to_string(),
                    format!(
                        "skipping run with unknown status {status_text:?} (run-ledger-v1 statuses: running, interrupted, failed, completed)"
                    ),
                ));
                continue;
            };
            if allow(status) {
                runs.push(ScannedRun { entry, run_dir });
            }
        }
    }
    Ok(Scan { runs, warnings })
}

/// Latest attempt per gate for one run dir, newest first, capped at `limit`.
/// Mirrors Python `_collect_gate_history`.
pub fn collect_gate_history(run_dir: &Path, limit: usize) -> Vec<Value> {
    let gates_root = run_dir.join("artifacts").join("gates");
    if !gates_root.is_dir() {
        return Vec::new();
    }
    let mut keyed: Vec<(String, Value)> = Vec::new();
    for scope_dir in sorted_dirs(&gates_root) {
        let scope_name = dir_name(&scope_dir);
        for name_dir in sorted_dirs(&scope_dir) {
            let mut attempts = sorted_dirs(&name_dir);
            attempts.sort_by_key(|p| std::cmp::Reverse(dir_name(p).parse::<i64>().unwrap_or(0)));
            for attempt_dir in attempts {
                let envelope_path = attempt_dir.join("envelope.json");
                if !envelope_path.is_file() {
                    continue;
                }
                let Ok(envelope) = read_json(&envelope_path) else {
                    continue;
                };
                let gate_status = envelope
                    .get("status")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown")
                    .to_owned();
                let attempt_name = dir_name(&attempt_dir);
                let attempt_value = attempt_name
                    .parse::<i64>()
                    .map(Value::from)
                    .unwrap_or_else(|_| Value::String(attempt_name));
                let mut entry = ledger_json::json!({
                    "name": envelope
                        .get("gate")
                        .and_then(|g| g.get("name"))
                        .and_then(Value::as_str)
                        .unwrap_or(&dir_name(&name_dir)),
                    "scope": scope_name,
                    "attempt": attempt_value,
                    "status": gate_status,
                    "path": envelope_path.display().to_string(),
                });
                if let Some(classification) = core::classify_gate(&envelope)
                    && let Some(map) = entry.as_object_mut()
                {
                    map.insert("classification".to_owned(), Value::String(classification));
                }
                let timestamp = envelope
                    .get("gate")
                    .and_then(|g| g.get("timestamp"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                keyed.push((timestamp, entry));
                break;
            }
        }
    }
    keyed.sort_by(|a, b| b.0.cmp(&a.0));
    keyed.into_iter().take(limit).map(|(_, e)| e).collect()
}

fn sorted_dirs(path: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(path)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|e| e.path()))
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default();
    dirs.sort();
    dirs
}

fn dir_name(path: &Path) -> String {
    path.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_owned()
}

/// Sort key used by resume (ascending) and dashboard (descending).
/// Python `r.get("updated_at") or r.get("started_at") or ""`: an empty
/// `updated_at` falls through to `started_at`, so filter on truthiness.
pub fn recency_key(entry: &Value) -> String {
    entry
        .get("updated_at")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| {
            entry
                .get("started_at")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty())
        })
        .unwrap_or("")
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_env() -> (tempfile::TempDir, LedgerEnv) {
        let dir = tempfile::tempdir().unwrap_or_else(|e| panic!("tempdir: {e}"));
        let env = LedgerEnv {
            state_dir: dir.path().join("state"),
            repo: dir.path().join("repo"),
            repo_hash: "testhash0000".to_owned(),
        };
        (dir, env)
    }

    fn init_args<'a>() -> InitArgs<'a> {
        InitArgs {
            skill: "kickoff",
            flow: None,
            ticket_id: "TASK-19",
            ticket_title: "Port",
            ticket_url: "",
            branch: Some("feature/x"),
            run_id: None,
        }
    }

    #[test]
    fn init_creates_contract_tree_and_first_event() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        assert_eq!(out.flow, "kickoff");
        let run_dir = &out.run_dir;
        for sub in [
            "artifacts/gates",
            "artifacts/reviews",
            "logs",
            "checkpoints",
        ] {
            assert!(run_dir.join(sub).is_dir(), "missing {sub}");
        }
        let run = read_json(&run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["kind"], "run-ledger-v1");
        assert_eq!(run["status"], "running");
        assert_eq!(run["events"]["count"], 1, "run_initialized counted");
        assert_eq!(run["branch"], "feature/x");
        let events = fs::read_to_string(run_dir.join("events.jsonl")).unwrap_or_default();
        assert_eq!(events.lines().count(), 1);
        assert!(events.contains("\"type\": \"run_initialized\""));
        let transcript = fs::read_to_string(run_dir.join("transcript.md")).unwrap_or_default();
        assert!(transcript.starts_with("# Beislið run transcript\n"));
        assert_eq!(transcript.matches("\n## ").count(), 1);
    }

    #[test]
    fn explicit_run_id_collision_is_loud_and_auto_ids_retry() {
        let (_tmp, env) = temp_env();
        let mut args = init_args();
        args.run_id = Some("fixed-id");
        init_run(&env, &args).unwrap_or_else(|_| panic!("first init"));
        match init_run(&env, &args) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::RunIdCollision),
            other => panic!("expected collision, got {other:?}", other = other.is_ok()),
        }
    }

    #[test]
    fn invalid_explicit_run_id_is_rejected() {
        let (_tmp, env) = temp_env();
        let mut args = init_args();
        args.run_id = Some("../escape");
        match init_run(&env, &args) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::RunIdInvalid),
            _ => panic!("expected run_id_invalid"),
        }
    }

    #[test]
    fn append_event_keeps_count_lines_and_sections_in_step() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        for i in 0..3 {
            append_event(
                &out.run_dir,
                "step",
                &ledger_json::json!({"i": i, "token": "leak"}),
                None,
            )
            .unwrap_or_else(|_| panic!("event {i}"));
        }
        let run = read_json(&out.run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["events"]["count"], 4);
        let events = fs::read_to_string(out.run_dir.join("events.jsonl")).unwrap_or_default();
        assert_eq!(events.lines().count(), 4);
        assert!(events.contains("\"token\": \"[REDACTED]\""));
        let transcript = fs::read_to_string(out.run_dir.join("transcript.md")).unwrap_or_default();
        assert_eq!(transcript.matches("\n## ").count(), 4);
    }

    #[test]
    fn checkpoint_updates_run_entry_and_writes_document() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        let path = record_checkpoint(
            &out.run_dir,
            "ctx ready",
            &ledger_json::json!({"x": 1}),
            Some("resume here"),
        )
        .unwrap_or_else(|_| panic!("checkpoint"));
        assert!(path.ends_with("checkpoints/ctx-ready.json"));
        let body = read_json(&path).unwrap_or_else(|_| panic!("checkpoint body"));
        assert_eq!(body["kind"], "run-ledger-checkpoint-v1");
        assert_eq!(body["resume_hint"], "resume here");
        let run = read_json(&out.run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["current_step"], "ctx ready");
        assert_eq!(run["latest_checkpoint"]["name"], "ctx ready");
        assert_eq!(run["resume_hint"], "resume here");
        assert_eq!(run["checkpoints"].as_array().map(Vec::len), Some(1));
    }

    #[test]
    fn gate_records_attempts_artifacts_checkpoint_and_event() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        let envelope = ledger_json::json!({
            "status": "fail",
            "environment_failure": false,
            "gate": {"name": "fmt", "scope": "Repo Wide", "timestamp": "T1"},
        });
        let first = record_gate(&out.run_dir, "fmt", None, &envelope, None)
            .unwrap_or_else(|_| panic!("gate 1"));
        let second = record_gate(&out.run_dir, "fmt", None, &envelope, None)
            .unwrap_or_else(|_| panic!("gate 2"));
        assert!(
            first
                .envelope_path
                .to_string_lossy()
                .contains("/Repo-Wide/fmt/1/")
        );
        assert!(
            second
                .envelope_path
                .to_string_lossy()
                .contains("/Repo-Wide/fmt/2/")
        );
        let run = read_json(&out.run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["artifacts"].as_array().map(Vec::len), Some(2));
        assert_eq!(run["logs"].as_array().map(Vec::len), Some(2));
        assert_eq!(run["current_step"], "gate-Repo-Wide-fmt");
        let history = collect_gate_history(&out.run_dir, 5);
        assert_eq!(history.len(), 1, "latest attempt per gate");
        assert_eq!(history[0]["attempt"], 2);
        assert_eq!(history[0]["classification"], "code_failure");
    }

    #[test]
    fn interrupt_and_finalize_update_status() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        record_interrupt(&out.run_dir, "stop TOKEN=abc", Some("pick up at step 2"))
            .unwrap_or_else(|_| panic!("interrupt"));
        let run = read_json(&out.run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["status"], "interrupted");
        assert_eq!(run["interruption"]["reason"], "stop TOKEN=[REDACTED]");
        assert_eq!(run["resume_hint"], "pick up at step 2");

        match record_finalize(&out.run_dir, "running", None) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::LedgerStatusInvalid),
            _ => panic!("running must be rejected"),
        }
        record_finalize(&out.run_dir, "completed", None).unwrap_or_else(|_| panic!("finalize"));
        let run = read_json(&out.run_dir.join("run.json")).unwrap_or_else(|_| panic!("run.json"));
        assert_eq!(run["status"], "completed");
        assert!(run.get("finalized_at").is_some());
    }

    #[test]
    fn find_run_dir_reports_missing_and_ambiguous() {
        let (_tmp, env) = temp_env();
        match find_run_dir(&env, "nope", None) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::RunNotFound),
            _ => panic!("expected run_not_found"),
        }
        let mut args = init_args();
        args.run_id = Some("same-id");
        init_run(&env, &args).unwrap_or_else(|_| panic!("init kickoff"));
        args.flow = Some("other");
        init_run(&env, &args).unwrap_or_else(|_| panic!("init other"));
        match find_run_dir(&env, "same-id", None) {
            Err(StoreError::Domain(diag)) => assert_eq!(diag.code, Code::RunAmbiguous),
            _ => panic!("expected run_ambiguous"),
        }
        let found = find_run_dir(&env, "same-id", Some("other"))
            .unwrap_or_else(|_| panic!("flow disambiguation"));
        assert!(found.to_string_lossy().contains("/other/"));
    }

    #[test]
    fn scan_filters_status_and_warns_on_garbage() {
        let (_tmp, env) = temp_env();
        let out = init_run(&env, &init_args()).unwrap_or_else(|_| panic!("init"));
        record_finalize(&out.run_dir, "completed", None).unwrap_or_else(|_| panic!("finalize"));
        let mut args = init_args();
        args.run_id = Some("second");
        init_run(&env, &args).unwrap_or_else(|_| panic!("init 2"));
        // A ghost-active run must be skipped with a warning, not accepted.
        let ghost_dir = env.run_root("kickoff").join("ghost");
        fs::create_dir_all(&ghost_dir).unwrap_or_else(|_| panic!("ghost dir"));
        fs::write(
            ghost_dir.join("run.json"),
            "{\"status\": \"active\", \"run_id\": \"ghost\"}\n",
        )
        .unwrap_or_else(|_| panic!("ghost run.json"));

        let scan = scan_runs(&env, None, Status::is_incomplete).unwrap_or_else(|_| panic!("scan"));
        let ids: Vec<&str> = scan
            .runs
            .iter()
            .filter_map(|r| r.entry.get("run_id").and_then(Value::as_str))
            .collect();
        assert_eq!(ids, vec!["second"]);
        assert_eq!(scan.warnings.len(), 1);
        assert_eq!(scan.warnings[0].code, Code::LedgerStatusInvalid);

        let all = scan_runs(&env, None, |_| true).unwrap_or_else(|_| panic!("scan all"));
        assert_eq!(all.runs.len(), 2);
    }
}
