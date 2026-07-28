#![allow(clippy::unwrap_used)]

use std::fs;

use nopal_core::gate_scaffold::{self, Readiness};

#[test]
fn rust_project_selects_stable_cargo_template_and_complete_gates() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("Cargo.toml"),
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();

    let plan = gate_scaffold::inspect(temp.path()).unwrap();

    assert!(plan.ok, "{:?}", plan.diagnostics);
    assert_eq!(plan.readiness, Readiness::Ready);
    assert_eq!(
        plan.selected_template_ids(),
        vec!["baseline.git/v1", "rust.cargo/v1"]
    );
    assert_eq!(
        plan.gates
            .iter()
            .map(|gate| gate.id.as_str())
            .collect::<Vec<_>>(),
        vec![
            "detected.root.baseline.diff-check",
            "detected.root.rust.cargo-fmt",
            "detected.root.rust.cargo-clippy",
            "detected.root.rust.cargo-test",
        ]
    );
    let rendered = plan.gates_json().unwrap();
    assert!(rendered.contains(r#""version": "nopal.gates/v2""#));
    assert!(rendered.contains(r#""version": "nopal.gate-scaffold/v1""#));
}

#[test]
fn unknown_project_gets_baseline_but_needs_explicit_configuration() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(temp.path().join("README.md"), "unknown\n").unwrap();

    let plan = gate_scaffold::inspect(temp.path()).unwrap();

    assert!(plan.ok);
    assert_eq!(plan.readiness, Readiness::NeedsConfiguration);
    assert_eq!(plan.selected_template_ids(), vec!["baseline.git/v1"]);
    assert_eq!(plan.gates.len(), 1);
    assert!(plan.decisions.iter().any(|decision| {
        decision.reason_code == "no_validation_evidence" && decision.outcome.as_str() == "blocked"
    }));
}

#[test]
fn generated_v2_document_is_accepted_and_malformed_provenance_fails_closed() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .unwrap();
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    let rendered = plan.gates_json().unwrap();

    let (config, diagnostics) = nopal_core::gates::parse_gates(&rendered, ".nopal/gates.jsonc");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    assert!(config.unwrap().scaffold.is_some());

    let malformed = rendered.replace(
        r#""version": "nopal.gate-scaffold/v1""#,
        r#""version": "nopal.gate-scaffold/v99""#,
    );
    let (_, diagnostics) = nopal_core::gates::parse_gates(&malformed, ".nopal/gates.jsonc");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::VersionUnsupported
    }));
}

#[test]
fn v1_remains_explicit_authority_but_cannot_smuggle_scaffold_metadata() {
    let v1 = r#"{
      "version": "nopal.gates/v1",
      "gates": [{"id":"explicit", "stage":"pre_pr", "argv":["true"]}]
    }"#;
    let (config, diagnostics) = nopal_core::gates::parse_gates(v1, ".nopal/gates.jsonc");
    assert!(diagnostics.is_empty());
    assert!(config.unwrap().scaffold.is_none());

    let smuggled = v1.replace(
        r#""gates":"#,
        r#""scaffold":{"version":"nopal.gate-scaffold/v1"},"gates":"#,
    );
    let (_, diagnostics) = nopal_core::gates::parse_gates(&smuggled, ".nopal/gates.jsonc");
    assert!(
        diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.code == nopal_core::diagnostics::Code::FieldInvalid })
    );
}

#[test]
fn repeated_detection_is_byte_deterministic() {
    let temp = tempfile::tempdir().unwrap();
    fs::write(
        temp.path().join("Cargo.toml"),
        "[workspace]\nmembers = []\n",
    )
    .unwrap();

    let first = gate_scaffold::inspect(temp.path()).unwrap();
    let second = gate_scaffold::inspect(temp.path()).unwrap();

    assert_eq!(first.gates_json().unwrap(), second.gates_json().unwrap());
    assert_eq!(
        serde_json::to_vec(&first).unwrap(),
        serde_json::to_vec(&second).unwrap()
    );
}
