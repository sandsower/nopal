//! Output envelope for `nopal info`.
//!
//! Same contract as every other nopal command: one envelope, one builder per
//! output flavor, kind `nopal.info/v1`. A stale installed binary and a fresh
//! build once both reported `nopal 0.1.0` on 2026-07-07 while differing in
//! whole subcommand families - `nopal info` gives consumers (beislid doctor
//! included) a deterministic report instead of text-parsing `--version` or
//! `--help`.

use serde::Serialize;

use nopal_core::toon::{self, Value};

pub const INFO_KIND: &str = "nopal.info/v1";

#[derive(Debug, Clone, Serialize)]
pub struct InfoReport {
    pub kind: &'static str,
    pub ok: bool,
    pub version: &'static str,
    pub commit: Option<&'static str>,
    pub capabilities: Vec<String>,
}

/// Build the `nopal.info/v1` report from the top-level clap `Command`.
/// `cmd` is expected to be `Cli::command()` from main.rs; capabilities are
/// derived from its subcommands rather than hand-maintained so the list
/// cannot drift from the real CLI surface.
pub fn info_report(cmd: &clap::Command) -> InfoReport {
    let mut capabilities: Vec<String> = cmd
        .get_subcommands()
        // clap auto-generates a `help` subcommand; it is not a capability of
        // nopal itself, so it is excluded explicitly.
        .filter(|sub| sub.get_name() != "help" && !sub.is_hide_set())
        .map(|sub| sub.get_name().to_owned())
        .collect();
    capabilities.sort();

    InfoReport {
        kind: INFO_KIND,
        ok: true,
        version: env!("CARGO_PKG_VERSION"),
        commit: option_env!("NOPAL_BUILD_COMMIT"),
        capabilities,
    }
}

pub fn info_toon(report: &InfoReport) -> String {
    let doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
        ("version".into(), Value::str(report.version)),
        ("commit".into(), commit_cell(report.commit)),
        (
            "capabilities".into(),
            Value::Arr(report.capabilities.iter().map(Value::str).collect()),
        ),
    ];
    toon::encode(&doc)
}

fn commit_cell(commit: Option<&'static str>) -> Value {
    match commit {
        // Matches the `token_budget` idiom in workflow_report.rs: an absent
        // optional renders as the literal string "-", not a bare null.
        Some(commit) => Value::str(commit),
        None => Value::str("-"),
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    fn cli_command() -> clap::Command {
        crate::Cli::command()
    }

    #[test]
    fn capabilities_are_sorted() {
        let report = info_report(&cli_command());
        let mut sorted = report.capabilities.clone();
        sorted.sort();
        assert_eq!(report.capabilities, sorted);
    }

    #[test]
    fn capabilities_exclude_help() {
        let report = info_report(&cli_command());
        assert!(!report.capabilities.iter().any(|name| name == "help"));
        assert!(
            !report
                .capabilities
                .iter()
                .any(|name| name == "__rondo-host")
        );
    }

    #[test]
    fn capabilities_include_info_and_field() {
        let report = info_report(&cli_command());
        assert!(report.capabilities.iter().any(|name| name == "info"));
        assert!(report.capabilities.iter().any(|name| name == "field"));
    }

    #[test]
    fn toon_contains_kind() {
        let report = info_report(&cli_command());
        assert!(info_toon(&report).contains("kind: nopal.info/v1"));
    }
}
