//! Feed adapters: the composed field query, ask resolution, the
//! rondo.core/v1 event tail, and the process-tree agent-presence poller.
//!
//! Every adapter is a [`Feed`] polled on its own thread; results arrive in
//! the app loop as [`FeedEvent`]s. A source that is absent (subcommand not
//! merged yet, rondo checkout missing, mix not installed) degrades to
//! [`SourceStatus::Unavailable`] and never crashes the field.

pub mod agents;
pub mod asks;
pub mod field;
pub mod rondo;

use std::process::Command;
use std::sync::mpsc::Sender;
use std::time::Duration;

use crate::AppEvent;
use crate::state::{FeedEvent, SourceStatus};

/// One pollable feed source.
pub trait Feed: Send {
    fn name(&self) -> &'static str;
    fn interval(&self) -> Duration;
    fn poll(&mut self) -> Result<Vec<FeedEvent>, String>;
}

/// Poll `feed` forever on a background thread, reporting availability
/// transitions; exits when the app loop hangs up.
pub fn spawn(mut feed: Box<dyn Feed>, tx: Sender<AppEvent>) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut last_ok: Option<bool> = None;
        loop {
            let (events, status) = match feed.poll() {
                Ok(events) => (events, SourceStatus::Ok),
                Err(reason) => (Vec::new(), SourceStatus::Unavailable(reason)),
            };
            let ok = status == SourceStatus::Ok;
            if last_ok != Some(ok) {
                last_ok = Some(ok);
                let event = FeedEvent::Source {
                    name: feed.name().to_owned(),
                    status,
                };
                if tx.send(AppEvent::Feed(event)).is_err() {
                    return;
                }
            }
            for event in events {
                if tx.send(AppEvent::Feed(event)).is_err() {
                    return;
                }
            }
            std::thread::sleep(feed.interval());
        }
    })
}

/// Run a subprocess expected to print JSON on stdout. Exit status is not
/// consulted beyond reporting: nopal report commands exit 1 with a valid
/// payload when a domain problem exists, so stdout wins when parseable.
pub fn run_json_command(
    argv: &[String],
    cwd: Option<&std::path::Path>,
) -> Result<serde_json::Value, String> {
    let (program, args) = argv
        .split_first()
        .ok_or_else(|| "empty feed command".to_owned())?;
    let mut command = Command::new(program);
    command.args(args);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    let output = command
        .output()
        .map_err(|err| format!("failed to spawn {program}: {err}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    if let Ok(value) = serde_json::from_str(stdout.trim()) {
        return Ok(value);
    }
    // Some transports print compile/startup noise before the payload
    // (mix prints "Generated rondo app"); take the last JSON-shaped line.
    if let Some(value) = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .and_then(|line| serde_json::from_str(line.trim()).ok())
    {
        return Ok(value);
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let detail = if stderr.trim().is_empty() {
        stdout.trim().to_owned()
    } else {
        stderr.trim().to_owned()
    };
    let mut summary = detail.lines().next().unwrap_or("no output").to_owned();
    summary.truncate(120);
    Err(format!("{program} returned no JSON: {summary}"))
}

/// Extract a string field from a JSON object, defaulting to empty.
pub(crate) fn str_field(value: &serde_json::Value, field: &str) -> String {
    value
        .get(field)
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_owned()
}
