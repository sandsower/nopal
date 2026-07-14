// Integration tests may panic freely; clippy's in-tests allowance only covers
// #[test] fns, not shared helpers in the tests/ tree.
#![allow(clippy::expect_used, clippy::unwrap_used)]

//! `nopal workflow show` integration tests (WS-CORE): present config with
//! handoff/babysit overrides, absent module (defaults), and invalid config
//! (nonzero exit, diagnostics rendered).

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn example(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn nopal(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_nopal"))
        .args(args)
        .output()
        .expect("failed to spawn nopal binary")
}

fn stdout(out: &Output) -> String {
    String::from_utf8(out.stdout.clone()).expect("stdout is not utf-8")
}

fn json(out: &Output) -> serde_json::Value {
    serde_json::from_str(&stdout(out)).unwrap_or_else(|err| {
        panic!(
            "stdout is not valid JSON ({err}):\n{}\nstderr: {}",
            stdout(out),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}

#[test]
fn workflow_show_present_config_reports_overrides() {
    let out = nopal(&["--dir", &example("nopal"), "--json", "workflow", "show"]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.workflow.show/v1");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["handoff"]["auto"], true);
    assert_eq!(
        doc["handoff"]["events"],
        serde_json::json!(["kickoff_context_ready"])
    );
    assert_eq!(
        doc["handoff"]["exclude"],
        serde_json::json!(["break_spec_approved", "spec_approved", "blueprint_approved"])
    );
    assert_eq!(doc["babysit"]["token_budget"], 400000);
    assert_eq!(doc["diagnostics"], serde_json::json!([]));
}

#[test]
fn workflow_show_present_config_toon_matches_json() {
    let out = nopal(&["--dir", &example("nopal"), "workflow", "show"]);
    assert_eq!(out.status.code(), Some(0));
    let text = stdout(&out);
    assert!(text.contains("kind: nopal.workflow.show/v1"));
    assert!(text.contains("auto: true"));
    assert!(text.contains("token_budget: 400000"));
}

#[test]
fn workflow_show_missing_module_emits_defaults_and_exits_zero() {
    let out = nopal(&["--dir", &example("minimal"), "--json", "workflow", "show"]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["handoff"]["auto"], false);
    assert_eq!(doc["handoff"]["events"], serde_json::json!([]));
    assert_eq!(
        doc["handoff"]["exclude"],
        serde_json::json!(["break_spec_approved", "spec_approved", "blueprint_approved"])
    );
    assert_eq!(doc["babysit"]["token_budget"], serde_json::Value::Null);
    assert_eq!(doc["diagnostics"], serde_json::json!([]));
}

#[test]
fn workflow_show_invalid_config_exits_nonzero_with_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".nopal")).unwrap();
    fs::write(
        temp.path().join(".nopal/workflow.jsonc"),
        r#"{
  "version": "nopal.workflow/v1",
  "handoff": { "auto": "not-a-bool" },
  "babysit": { "token_budget": -5 }
}"#,
    )
    .unwrap();

    let out = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "workflow",
        "show",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false);
    let codes: Vec<&str> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|d| d["code"].as_str())
        .collect();
    assert!(codes.contains(&"field_invalid"), "{codes:?}");
    // Defaults still render alongside the diagnostics: nopal does not report
    // half-understood config.
    assert_eq!(doc["handoff"]["auto"], false);
    assert_eq!(doc["babysit"]["token_budget"], serde_json::Value::Null);
}
