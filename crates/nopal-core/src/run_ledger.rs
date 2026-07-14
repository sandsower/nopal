//! Durable run ledger core (`run-ledger-v1`) - the pure half.
//!
//! Ported from beislid `scripts/run_ledger.py`. Everything here is
//! values in / values out: the clock, randomness, filesystem, and git live in
//! `run_ledger_store`. Serialization is byte-faithful to the Python tool
//! (sorted keys, two-space indent, ensure-ASCII escapes, Python separators)
//! so both tools write the same trees.
//!
//! Conformance deltas from the Python tool:
//! the ghost `active` status (read but never written by any command) is not
//! modeled, and the legacy flat `runs/<repo_hash>` layout is not discovered.

use std::sync::OnceLock;

use ledger_json::Value;
use nopal_ledger_json as ledger_json;
use regex::Regex;
use serde::Serialize;

pub type JsonValue = Value;

pub const SCHEMA_VERSION: u64 = 1;
pub const LEDGER_KIND: &str = "run-ledger-v1";
pub const CHECKPOINT_KIND: &str = "run-ledger-checkpoint-v1";

/// Redacted strings are capped at this many characters.
pub const TEXT_LIMIT: usize = 2000;
/// Resume hints get a tighter cap.
pub const HINT_LIMIT: usize = 500;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Running,
    Interrupted,
    Failed,
    Completed,
}

impl Status {
    pub const ALL: [Status; 4] = [
        Status::Running,
        Status::Interrupted,
        Status::Failed,
        Status::Completed,
    ];

    /// Statuses `finalize` accepts.
    pub const FINAL: [Status; 3] = [Status::Interrupted, Status::Failed, Status::Completed];

    pub fn parse(s: &str) -> Option<Status> {
        match s {
            "running" => Some(Status::Running),
            "interrupted" => Some(Status::Interrupted),
            "failed" => Some(Status::Failed),
            "completed" => Some(Status::Completed),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Status::Running => "running",
            Status::Interrupted => "interrupted",
            Status::Failed => "failed",
            Status::Completed => "completed",
        }
    }

    pub fn is_incomplete(self) -> bool {
        self != Status::Completed
    }
}

// ---------------------------------------------------------------------------
// Time: UTC formatting from epoch seconds, no date crate
// ---------------------------------------------------------------------------

/// Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn civil_time(epoch_secs: i64) -> (i64, i64, i64, i64, i64, i64) {
    let days = epoch_secs.div_euclid(86_400);
    let secs = epoch_secs.rem_euclid(86_400);
    let (y, mo, d) = civil_from_days(days);
    (y, mo, d, secs / 3600, (secs / 60) % 60, secs % 60)
}

/// Python `datetime.now(timezone.utc).isoformat(timespec="seconds")`:
/// `2026-07-04T22:43:13+00:00`.
pub fn iso_utc(epoch_secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_time(epoch_secs);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{mi:02}:{s:02}+00:00")
}

/// Inverse of `civil_from_days`: days since 1970-01-01 for a civil date
/// (Howard Hinnant's algorithm, the standard reverse of the forward form
/// already used by `civil_time`).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400; // [0, 399]
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1; // [0, 365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0, 146096]
    era * 146_097 + doe - 719_468
}

/// Inverse of `iso_utc`: parse a `YYYY-MM-DDTHH:MM:SS` prefix back to epoch
/// seconds. Any trailing offset is ignored - every writer in this codebase
/// emits `+00:00`, so treating the prefix as UTC is exact for our own data.
/// `None` for anything that is not a plausible calendar date/time (garbage
/// `updated_at`/`now_iso` reads as "unknown age", never as a crash).
pub fn epoch_from_iso(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 19
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b'T'
        || bytes[13] != b':'
        || bytes[16] != b':'
    {
        return None;
    }
    let year: i64 = s.get(0..4)?.parse().ok()?;
    let month: i64 = s.get(5..7)?.parse().ok()?;
    let day: i64 = s.get(8..10)?.parse().ok()?;
    let hour: i64 = s.get(11..13)?.parse().ok()?;
    let minute: i64 = s.get(14..16)?.parse().ok()?;
    let second: i64 = s.get(17..19)?.parse().ok()?;
    if !(1..=12).contains(&month)
        || !(1..=31).contains(&day)
        || !(0..=23).contains(&hour)
        || !(0..=59).contains(&minute)
        || !(0..=60).contains(&second)
    {
        return None;
    }
    let days = days_from_civil(year, month, day);
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Python `strftime("%Y%m%dT%H%M%SZ")`: `20260704T224313Z`.
pub fn stamp_utc(epoch_secs: i64) -> String {
    let (y, mo, d, h, mi, s) = civil_time(epoch_secs);
    format!("{y:04}{mo:02}{d:02}T{h:02}{mi:02}{s:02}Z")
}

// ---------------------------------------------------------------------------
// Names and ids
// ---------------------------------------------------------------------------

/// Path-safe slug: runs of characters outside `[A-Za-z0-9_.-]` collapse to
/// one `-`, then edge `-`/`.`/`_` are stripped; empty falls back.
pub fn slug(value: &str, fallback: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut in_run = false;
    for c in value.chars() {
        if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
            out.push(c);
            in_run = false;
        } else if !in_run {
            out.push('-');
            in_run = true;
        }
    }
    let trimmed = out.trim_matches(|c| matches!(c, '-' | '.' | '_'));
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// Flow defaults to skill, then `"run"`; empty strings count as absent
/// (Python `or` semantics).
pub fn normalize_flow(flow: Option<&str>, skill: Option<&str>) -> String {
    let base = flow
        .filter(|s| !s.is_empty())
        .or_else(|| skill.filter(|s| !s.is_empty()))
        .unwrap_or("run");
    slug(base, "run")
}

/// A persisted identifier is one path-safe segment starting alphanumeric.
/// The Python tool's extra `.`/`..` check is subsumed: neither starts
/// alphanumeric.
pub fn identifier_valid(value: &str) -> bool {
    let mut chars = value.chars();
    match chars.next() {
        Some(c) if c.is_ascii_alphanumeric() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

pub fn new_run_id(stamp: &str, token_hex: &str) -> String {
    format!("{stamp}-{token_hex}")
}

// ---------------------------------------------------------------------------
// Redaction parity with the action-policy secret patterns.
// ---------------------------------------------------------------------------

// The patterns are fixed program constants ported verbatim from the Python
// tool; compilation cannot fail for them, so the panic path is unreachable.
#[allow(clippy::expect_used)]
fn fixed(pattern: &str) -> Regex {
    Regex::new(pattern).expect("fixed redaction pattern compiles")
}

fn assignment_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        fixed(
            r#"(?i)\b((?:[a-z0-9]+[_-])*(?:api[_-]?key|token|secret|password|private[_-]?key|auth[_-]?header)(?:[_-][a-z0-9]+)*)\b\s*[:=]\s*("[^"\r\n]*"|'[^'\r\n]*'|[^\s,;)}\]]+)"#,
        )
    })
}

fn bearer_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        fixed(r#"(?i)(authorization\s*:\s*bearer\s+)("[^"\r\n]*"|'[^'\r\n]*'|[^\s,;)}\]]+)"#)
    })
}

fn env_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| fixed(r"(?i)\$\{?(TOKEN|SECRET|PASSWORD|API[_-]?KEY|AUTH|GITHUB_TOKEN)\}?"))
}

fn json_key_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        fixed(
            r"(?i)\b(?:[a-z0-9]+[_-])*(?:api[_-]?key|token|secret|password|authorization|private[_-]?key|auth[_-]?header)(?:[_-][a-z0-9]+)*\b",
        )
    })
}

fn truncate_chars(s: &str, limit: usize) -> String {
    s.chars().take(limit).collect()
}

/// Strip NULs, redact secret-looking spans, truncate to `limit` characters.
/// Assignment redaction always normalizes the separator to `=`.
pub fn redact_text(text: &str, limit: usize) -> String {
    let cleaned: String = text.chars().filter(|&c| c != '\u{0}').collect();
    let pass1 = bearer_re().replace_all(&cleaned, |c: &regex::Captures| {
        format!("{}[REDACTED]", &c[1])
    });
    let pass2 = assignment_re().replace_all(&pass1, |c: &regex::Captures| {
        format!("{}=[REDACTED]", &c[1])
    });
    let pass3 = env_re().replace_all(&pass2, "[REDACTED]");
    truncate_chars(&pass3, limit)
}

/// Recursive redaction: secret-looking object keys lose their whole value,
/// strings pass through `redact_text`, everything else is untouched.
pub fn redact_json(value: &Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| {
                    if json_key_re().is_match(k) {
                        (k.clone(), Value::String("[REDACTED]".to_owned()))
                    } else {
                        (k.clone(), redact_json(v))
                    }
                })
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.iter().map(redact_json).collect()),
        Value::String(s) => Value::String(redact_text(s, TEXT_LIMIT)),
        other => other.clone(),
    }
}

// ---------------------------------------------------------------------------
// Python-compatible JSON serialization
// ---------------------------------------------------------------------------

/// `json.dumps(value, sort_keys=True)`: single line, `", "` / `": "`
/// separators, ASCII-escaped strings. Keys are already sorted because
/// `serde_json::Map` is a BTreeMap.
pub fn json_line(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, None, 0);
    out
}

/// `json.dumps(value, indent=2, sort_keys=True)`: no trailing newline
/// (writers add one).
pub fn json_pretty(value: &Value) -> String {
    let mut out = String::new();
    write_value(&mut out, value, Some(2), 0);
    out
}

fn write_value(out: &mut String, value: &Value, indent: Option<usize>, level: usize) {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => out.push_str(&n.to_string()),
        Value::String(s) => write_string(out, s),
        Value::Array(items) => write_seq(out, indent, level, items.len(), '[', ']', |out, i| {
            write_value(out, &items[i], indent, level + 1);
        }),
        Value::Object(map) => {
            // Sort explicitly: byte-level sortedness must not silently depend
            // on serde_json's default BTreeMap map (a `preserve_order`
            // feature unification anywhere in the tree would break it).
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by_key(|(k, _)| k.as_str());
            write_seq(out, indent, level, entries.len(), '{', '}', |out, i| {
                let (k, v) = entries[i];
                write_string(out, k);
                out.push_str(": ");
                write_value(out, v, indent, level + 1);
            });
        }
    }
}

fn write_seq(
    out: &mut String,
    indent: Option<usize>,
    level: usize,
    len: usize,
    open: char,
    close: char,
    mut item: impl FnMut(&mut String, usize),
) {
    out.push(open);
    if len == 0 {
        out.push(close);
        return;
    }
    for i in 0..len {
        if let Some(width) = indent {
            out.push('\n');
            out.push_str(&" ".repeat(width * (level + 1)));
        } else if i > 0 {
            out.push(' ');
        }
        item(out, i);
        if i + 1 < len {
            out.push(',');
        }
    }
    if let Some(width) = indent {
        out.push('\n');
        out.push_str(&" ".repeat(width * level));
    }
    out.push(close);
}

/// Python `ensure_ascii=True` string escaping: short escapes, `\u00XX` for
/// other control characters, and `\uXXXX` UTF-16 units for everything
/// non-ASCII (surrogate pairs beyond the BMP).
fn write_string(out: &mut String, s: &str) {
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{8}' => out.push_str("\\b"),
            '\u{c}' => out.push_str("\\f"),
            // Python escapes everything outside 0x20-0x7E, including DEL.
            c if (c as u32) < 0x20 || (c as u32) == 0x7f => {
                let _ = std::fmt::Write::write_fmt(out, format_args!("\\u{:04x}", c as u32));
            }
            c if c.is_ascii() => out.push(c),
            c => {
                let mut units = [0u16; 2];
                for unit in c.encode_utf16(&mut units) {
                    let _ = std::fmt::Write::write_fmt(out, format_args!("\\u{unit:04x}"));
                }
            }
        }
    }
    out.push('"');
}

// ---------------------------------------------------------------------------
// Document builders
// ---------------------------------------------------------------------------

pub struct InitContext<'a> {
    pub run_id: &'a str,
    pub flow: &'a str,
    pub repo: &'a str,
    pub repo_hash: &'a str,
    pub branch: &'a str,
    pub skill: &'a str,
    pub ticket_id: &'a str,
    pub ticket_title: &'a str,
    pub ticket_url: &'a str,
    pub started_at: &'a str,
    pub run_dir: &'a str,
}

/// The initial `run.json` document, field-for-field the Python shape.
pub fn new_run_entry(ctx: &InitContext) -> Value {
    let sep = std::path::MAIN_SEPARATOR;
    ledger_json::json!({
        "kind": LEDGER_KIND,
        "schema_version": SCHEMA_VERSION,
        "run_id": ctx.run_id,
        "flow": ctx.flow,
        "repo": ctx.repo,
        "repo_hash": ctx.repo_hash,
        "branch": ctx.branch,
        "skill": ctx.skill,
        "ticket": {"id": ctx.ticket_id, "title": ctx.ticket_title, "url": ctx.ticket_url},
        "ticket_id": ctx.ticket_id,
        "status": Status::Running.as_str(),
        "started_at": ctx.started_at,
        "updated_at": ctx.started_at,
        "paths": {
            "run_dir": ctx.run_dir,
            "transcript": format!("{}{}transcript.md", ctx.run_dir, sep),
            "events": format!("{}{}events.jsonl", ctx.run_dir, sep),
            "final_report": format!("{}{}final-report.md", ctx.run_dir, sep),
        },
        "selected_guides": [],
        "plan": null,
        "current_step": null,
        "checkpoints": [],
        "artifacts": [],
        "logs": [],
        "accepted_risks": [],
        "side_effects": [],
        "events": {"count": 0},
    })
}

/// The transcript file header written by `init`.
pub fn transcript_header(ctx: &InitContext) -> String {
    format!(
        "# Beislið run transcript\n\n\
         kind: `{LEDGER_KIND}`\n\
         run_id: `{}`\n\
         flow: `{}`\n\
         repo: {}\n\
         branch: {}\n\
         ticket_id: `{}`\n\
         skill: {}\n\
         started: {}\n",
        ctx.run_id,
        ctx.flow,
        ctx.repo,
        redact_text(ctx.branch, TEXT_LIMIT),
        redact_text(ctx.ticket_id, TEXT_LIMIT),
        redact_text(ctx.skill, TEXT_LIMIT),
        ctx.started_at,
    )
}

/// One appended transcript section. The event type is capped at 160 chars.
pub fn transcript_section(event_type: &str, summary: &str) -> String {
    format!("\n## {}\n- {}\n", redact_text(event_type, 160), summary)
}

/// Default transcript summary: the redacted payload as a single JSON line,
/// truncated to the text limit.
pub fn default_transcript_summary(safe_payload: &Value) -> String {
    truncate_chars(&json_line(safe_payload), TEXT_LIMIT)
}

/// One `events.jsonl` record (payload must already be redacted).
pub fn event_value(event_type: &str, safe_payload: Value, now_iso: &str) -> Value {
    let mut map = std::collections::BTreeMap::new();
    map.insert("payload".to_owned(), safe_payload);
    map.insert("timestamp".to_owned(), Value::String(now_iso.to_owned()));
    map.insert("type".to_owned(), Value::String(event_type.to_owned()));
    Value::Object(map)
}

/// A checkpoint document body.
pub fn checkpoint_value(
    name: &str,
    payload: &Value,
    resume_hint: Option<&str>,
    now_iso: &str,
) -> Value {
    let mut map = std::collections::BTreeMap::new();
    map.insert("checkpoint".to_owned(), Value::String(name.to_owned()));
    map.insert("kind".to_owned(), Value::String(CHECKPOINT_KIND.to_owned()));
    map.insert("payload".to_owned(), redact_json(payload));
    map.insert("timestamp".to_owned(), Value::String(now_iso.to_owned()));
    if let Some(hint) = resume_hint.filter(|h| !h.is_empty()) {
        map.insert(
            "resume_hint".to_owned(),
            Value::String(redact_text(hint, HINT_LIMIT)),
        );
    }
    Value::Object(map)
}

pub fn checkpoint_file_name(name: &str) -> String {
    format!("{}.json", slug(name, "checkpoint"))
}

// ---------------------------------------------------------------------------
// Gate classification
// ---------------------------------------------------------------------------

/// Python truthiness for a JSON value.
pub(crate) fn truthy(value: Option<&Value>) -> bool {
    match value {
        None | Some(Value::Null) | Some(Value::Bool(false)) => false,
        Some(Value::Bool(true)) => true,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(a)) => !a.is_empty(),
        Some(Value::Object(o)) => !o.is_empty(),
    }
}

/// Failure classification for a gate envelope: `environment_failure` when the
/// envelope says so, otherwise its own `classification` or `code_failure`.
/// Non-failing envelopes classify as `None`.
pub fn classify_gate(envelope: &Value) -> Option<String> {
    if envelope.get("status").and_then(Value::as_str) != Some("fail") {
        return None;
    }
    if truthy(envelope.get("environment_failure")) {
        return Some("environment_failure".to_owned());
    }
    Some(
        envelope
            .get("classification")
            .and_then(Value::as_str)
            .unwrap_or("code_failure")
            .to_owned(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- time ---------------------------------------------------------------

    #[test]
    fn iso_and_stamp_match_python_formats() {
        assert_eq!(iso_utc(0), "1970-01-01T00:00:00+00:00");
        assert_eq!(stamp_utc(0), "19700101T000000Z");
        // 2026-07-04T22:43:13Z
        assert_eq!(iso_utc(1_783_204_993), "2026-07-04T22:43:13+00:00");
        assert_eq!(stamp_utc(1_783_204_993), "20260704T224313Z");
        // leap day
        assert_eq!(iso_utc(1_709_164_800), "2024-02-29T00:00:00+00:00");
        // year boundary
        assert_eq!(iso_utc(1_767_225_599), "2025-12-31T23:59:59+00:00");
        assert_eq!(iso_utc(1_767_225_600), "2026-01-01T00:00:00+00:00");
    }

    #[test]
    fn epoch_from_iso_round_trips_through_iso_utc() {
        for epoch in [
            0,
            1_783_204_993,
            1_709_164_800, // leap day
            1_767_225_599, // year boundary, just before
            1_767_225_600, // year boundary, just after
        ] {
            assert_eq!(
                epoch_from_iso(&iso_utc(epoch)),
                Some(epoch),
                "epoch {epoch}"
            );
        }
    }

    #[test]
    fn epoch_from_iso_rejects_garbage() {
        assert_eq!(epoch_from_iso(""), None);
        assert_eq!(epoch_from_iso("not a timestamp"), None);
        assert_eq!(epoch_from_iso("2026-13-01T00:00:00+00:00"), None); // bad month
        assert_eq!(epoch_from_iso("2026-01-32T00:00:00+00:00"), None); // bad day
        assert_eq!(epoch_from_iso("2026-01-01T25:00:00+00:00"), None); // bad hour
        assert_eq!(epoch_from_iso("2026-01-01"), None); // too short
    }

    // -- names --------------------------------------------------------------

    #[test]
    fn slug_collapses_runs_and_strips_edges() {
        assert_eq!(slug("feature/redaction", "item"), "feature-redaction");
        assert_eq!(slug("a  b//c", "item"), "a-b-c");
        assert_eq!(slug("--x--", "item"), "x");
        assert_eq!(slug("...", "item"), "item");
        assert_eq!(slug("", "checkpoint"), "checkpoint");
        assert_eq!(slug("gate name!", "gate"), "gate-name");
    }

    #[test]
    fn normalize_flow_prefers_flow_then_skill_then_run() {
        assert_eq!(normalize_flow(Some("Kick Off"), Some("s")), "Kick-Off");
        assert_eq!(normalize_flow(None, Some("kickoff")), "kickoff");
        assert_eq!(normalize_flow(Some(""), Some("kickoff")), "kickoff");
        assert_eq!(normalize_flow(None, None), "run");
        assert_eq!(normalize_flow(Some(""), Some("")), "run");
    }

    #[test]
    fn identifier_validation_matches_python_segment_rule() {
        assert!(identifier_valid("20260704T224313Z-a7f3c9"));
        assert!(identifier_valid("a"));
        assert!(!identifier_valid(""));
        assert!(!identifier_valid("."));
        assert!(!identifier_valid(".."));
        assert!(!identifier_valid("-leading-dash"));
        assert!(!identifier_valid("has/slash"));
        assert!(!identifier_valid("has space"));
    }

    // -- redaction parity (ported from test_run_ledger.sh) -------------------

    #[test]
    fn policy_secret_parity_redaction() {
        let cases = [
            ("TOKEN=token_value", "TOKEN=[REDACTED]"),
            ("secret: secret_value", "secret=[REDACTED]"),
            ("PASSWORD=password_value", "PASSWORD=[REDACTED]"),
            ("API_KEY=api_value", "API_KEY=[REDACTED]"),
            ("PRIVATE_KEY=private_value", "PRIVATE_KEY=[REDACTED]"),
            ("auth_header: header_value", "auth_header=[REDACTED]"),
            (
                "Authorization: Bearer bearer_value",
                "Authorization: Bearer [REDACTED]",
            ),
            ("deploy with $TOKEN", "deploy with [REDACTED]"),
            ("deploy with ${GITHUB_TOKEN}", "deploy with [REDACTED]"),
        ];
        for (sample, expected) in cases {
            assert_eq!(redact_text(sample, TEXT_LIMIT), expected, "case: {sample}");
        }
    }

    #[test]
    fn compound_snake_case_secrets_redact() {
        let message = "GITHUB_TOKEN=compound_text_value\nSECRET_KEY=compound_key_value\ndb_password: compound_password_value\nprivate_key=compound_private_value\nauth_header: compound_auth_header_value";
        let redacted = redact_text(message, TEXT_LIMIT);
        assert_eq!(
            redacted,
            "GITHUB_TOKEN=[REDACTED]\nSECRET_KEY=[REDACTED]\ndb_password=[REDACTED]\nprivate_key=[REDACTED]\nauth_header=[REDACTED]"
        );
    }

    #[test]
    fn non_secret_words_survive_redaction() {
        let text = "tokenizer and passwordless should remain visible";
        assert_eq!(redact_text(text, TEXT_LIMIT), text);
    }

    #[test]
    fn redact_text_strips_nul_and_truncates_by_chars() {
        assert_eq!(redact_text("a\u{0}b", TEXT_LIMIT), "ab");
        let long = "x".repeat(2500);
        assert_eq!(redact_text(&long, TEXT_LIMIT).chars().count(), 2000);
        assert_eq!(redact_text("héllo wörld", 7), "héllo w");
    }

    #[test]
    fn redact_json_replaces_secret_keys_and_recurses() {
        let value = ledger_json::json!({
            "github_token": "compound_json_value",
            "private_key": "compound_private_json_value",
            "auth_header": "x",
            "notes": "tokenizer and passwordless should remain visible",
            "nested": {"api_key": "v", "plain": "TOKEN=leak"},
            "list": ["PASSWORD=p"],
            "count": 3,
        });
        let redacted = redact_json(&value);
        assert_eq!(redacted["github_token"], "[REDACTED]");
        assert_eq!(redacted["private_key"], "[REDACTED]");
        assert_eq!(redacted["auth_header"], "[REDACTED]");
        assert_eq!(
            redacted["notes"],
            "tokenizer and passwordless should remain visible"
        );
        assert_eq!(redacted["nested"]["api_key"], "[REDACTED]");
        assert_eq!(redacted["nested"]["plain"], "TOKEN=[REDACTED]");
        assert_eq!(redacted["list"][0], "PASSWORD=[REDACTED]");
        assert_eq!(redacted["count"], 3);
    }

    // -- python-compatible json ----------------------------------------------

    #[test]
    fn json_line_uses_python_separators_and_sorted_keys() {
        let value = ledger_json::json!({"b": [1, 2], "a": {"y": true, "x": null}});
        assert_eq!(
            json_line(&value),
            r#"{"a": {"x": null, "y": true}, "b": [1, 2]}"#
        );
    }

    #[test]
    fn json_pretty_matches_python_indent_two() {
        let value = ledger_json::json!({"b": [1], "a": "s", "e": {}, "l": []});
        assert_eq!(
            json_pretty(&value),
            "{\n  \"a\": \"s\",\n  \"b\": [\n    1\n  ],\n  \"e\": {},\n  \"l\": []\n}"
        );
    }

    #[test]
    fn strings_escape_non_ascii_like_python_ensure_ascii() {
        let value = ledger_json::json!({"name": "Beislið"});
        assert_eq!(json_line(&value), "{\"name\": \"Beisli\\u00f0\"}");
        let astral = ledger_json::json!("🎉");
        assert_eq!(json_line(&astral), "\"\\ud83c\\udf89\"");
        let control = ledger_json::json!("a\u{1}b\"c\\d\ne");
        assert_eq!(json_line(&control), "\"a\\u0001b\\\"c\\\\d\\ne\"");
    }

    #[test]
    fn del_char_and_numbers_serialize_like_python() {
        // DEL is outside Python's raw range and must escape; the ledger JSON
        // wrapper keeps number source text, so Python-canonical numeric
        // literals echo through byte-identically.
        let value: Value = ledger_json::from_str(
            "{\"del\": \"a\\u007fb\", \"e30\": 1e+30, \"small\": 1e-07, \"big\": 12345678901234567890123}",
        )
        .unwrap();
        assert_eq!(
            json_line(&value),
            "{\"big\": 12345678901234567890123, \"del\": \"a\\u007fb\", \"e30\": 1e+30, \"small\": 1e-07}"
        );
    }

    // -- builders -------------------------------------------------------------

    fn ctx<'a>(run_dir: &'a str) -> InitContext<'a> {
        InitContext {
            run_id: "20260704T224313Z-a7f3c9",
            flow: "kickoff",
            repo: "/tmp/repo",
            repo_hash: "9030d801a642",
            branch: "feature/redaction",
            skill: "kickoff",
            ticket_id: "TASK-19",
            ticket_title: "Port",
            ticket_url: "",
            started_at: "2026-07-04T22:43:13+00:00",
            run_dir,
        }
    }

    #[test]
    fn run_entry_has_python_shape() {
        let entry = new_run_entry(&ctx("/state/runs/kickoff/9030d801a642/x"));
        assert_eq!(entry["kind"], LEDGER_KIND);
        assert_eq!(entry["schema_version"], 1);
        assert_eq!(entry["status"], "running");
        assert_eq!(entry["ticket"]["id"], "TASK-19");
        assert_eq!(entry["ticket_id"], "TASK-19");
        assert_eq!(entry["events"]["count"], 0);
        assert_eq!(
            entry["paths"]["transcript"],
            "/state/runs/kickoff/9030d801a642/x/transcript.md"
        );
        assert_eq!(entry["plan"], Value::Null);
        assert_eq!(entry["checkpoints"], ledger_json::json!([]));
    }

    #[test]
    fn transcript_header_matches_python_layout() {
        let header = transcript_header(&ctx("/x"));
        assert!(header.starts_with("# Beislið run transcript\n\n"));
        assert!(header.contains("kind: `run-ledger-v1`\n"));
        assert!(header.contains("run_id: `20260704T224313Z-a7f3c9`\n"));
        assert!(header.contains("branch: feature/redaction\n"));
        assert!(header.ends_with("started: 2026-07-04T22:43:13+00:00\n"));
    }

    #[test]
    fn transcript_section_shape_and_type_cap() {
        assert_eq!(
            transcript_section("checkpoint", "- x"),
            "\n## checkpoint\n- - x\n"
        );
        let long_type = "t".repeat(200);
        let section = transcript_section(&long_type, "s");
        assert!(section.starts_with(&format!("\n## {}\n", "t".repeat(160))));
    }

    #[test]
    fn checkpoint_value_includes_optional_hint() {
        let payload = ledger_json::json!({"token": "leak", "ok": true});
        let body = checkpoint_value("ctx ready", &payload, Some("resume here"), "T");
        assert_eq!(body["kind"], CHECKPOINT_KIND);
        assert_eq!(body["checkpoint"], "ctx ready");
        assert_eq!(body["payload"]["token"], "[REDACTED]");
        assert_eq!(body["resume_hint"], "resume here");
        let no_hint = checkpoint_value("x", &payload, None, "T");
        assert!(no_hint.get("resume_hint").is_none());
        let empty_hint = checkpoint_value("x", &payload, Some(""), "T");
        assert!(empty_hint.get("resume_hint").is_none());
    }

    #[test]
    fn checkpoint_file_names_are_slugged() {
        assert_eq!(checkpoint_file_name("ctx ready!"), "ctx-ready.json");
        assert_eq!(checkpoint_file_name(""), "checkpoint.json");
    }

    #[test]
    fn event_value_is_sorted_when_serialized() {
        let event = event_value("gate_result", ledger_json::json!({"a": 1}), "T");
        assert_eq!(
            json_line(&event),
            r#"{"payload": {"a": 1}, "timestamp": "T", "type": "gate_result"}"#
        );
    }

    // -- classification --------------------------------------------------------

    #[test]
    fn gate_classification_matches_python() {
        assert_eq!(classify_gate(&ledger_json::json!({"status": "pass"})), None);
        assert_eq!(classify_gate(&ledger_json::json!({})), None);
        assert_eq!(
            classify_gate(&ledger_json::json!({"status": "fail"})).as_deref(),
            Some("code_failure")
        );
        assert_eq!(
            classify_gate(&ledger_json::json!({"status": "fail", "environment_failure": true}))
                .as_deref(),
            Some("environment_failure")
        );
        assert_eq!(
            classify_gate(
                &ledger_json::json!({"status": "fail", "environment_failure": 0, "classification": "flaky"})
            )
            .as_deref(),
            Some("flaky")
        );
        assert_eq!(
            classify_gate(&ledger_json::json!({"status": "fail", "environment_failure": "yes"}))
                .as_deref(),
            Some("environment_failure")
        );
    }

    #[test]
    fn statuses_parse_and_exclude_ghost_active() {
        for status in Status::ALL {
            assert_eq!(Status::parse(status.as_str()), Some(status));
        }
        assert_eq!(Status::parse("active"), None);
    }
}
