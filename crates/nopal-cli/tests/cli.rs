// Integration tests may panic freely; clippy's in-tests allowance only covers
// #[test] fns, not shared helpers in the tests/ tree.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn example(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .join(name)
        .to_string_lossy()
        .into_owned()
}

fn fixture(name: &str) -> String {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
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
    serde_json::from_str(&stdout(out)).expect("stdout is not valid JSON")
}

fn write_minimal_project(root: &Path, extra_manifest_fields: &str) {
    fs::create_dir_all(root.join(".nopal")).unwrap();
    fs::write(
        root.join(".nopal/nopal.jsonc"),
        format!(
            r#"{{
  "version": "nopal.project/v1",
  "project": {{ "name": "export-fixture" }},
  "profile": "minimal"{extra_manifest_fields}
}}
"#
        ),
    )
    .unwrap();
}

#[test]
fn export_process_stdout_json_emits_artifact_and_redacts_secrets() {
    let temp = tempfile::tempdir().unwrap();
    write_minimal_project(
        temp.path(),
        r#",
  "api_token": "plain-secret",
  "bearer": "plain bearer credential",
  "bearer_literal": "Bearer abc123""#,
    );

    let out = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "export",
        "process",
        "--stdout",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.process_artifact/v1");
    assert_eq!(doc["project"], "export-fixture");
    assert_eq!(doc["modules"]["manifest"]["api_token"], "<redacted>");
    assert_eq!(doc["modules"]["manifest"]["bearer"], "<redacted>");
    assert_eq!(doc["modules"]["manifest"]["bearer_literal"], "<redacted>");
    assert!(
        doc["sources"][0]["hash"]
            .as_str()
            .unwrap()
            .starts_with("sha256:")
    );
    assert_eq!(doc["sources"][0]["hash"].as_str().unwrap().len(), 71);
    let diagnostic_codes: Vec<&str> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str())
        .collect();
    assert!(diagnostic_codes.contains(&"process_artifact_redacted"));
    assert!(!stdout(&out).contains("plain-secret"));
    assert!(!stdout(&out).contains("plain bearer credential"));
    assert!(!stdout(&out).contains("Bearer abc123"));
}

#[test]
fn export_process_output_then_fresh_check_passes() {
    let temp = tempfile::tempdir().unwrap();
    write_minimal_project(temp.path(), "");
    let artifact_path = temp.path().join("artifact.json");

    let export = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "export",
        "process",
        "--output",
        artifact_path.to_str().unwrap(),
    ]);
    assert_eq!(export.status.code(), Some(0));
    assert_eq!(
        json_from_file(&artifact_path)["kind"],
        "nopal.process_artifact/v1"
    );
    assert!(stdout(&export).contains("kind: nopal.process_artifact.export/v1"));

    let check = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "export",
        "process",
        "--output",
        artifact_path.to_str().unwrap(),
        "--check",
    ]);
    assert_eq!(check.status.code(), Some(0));
    let doc = json(&check);
    assert_eq!(doc["kind"], "nopal.process_artifact.check/v1");
    assert_eq!(doc["ok"], true);
}

fn json_from_file(path: &Path) -> serde_json::Value {
    let text = fs::read_to_string(path).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn copy_beislid_workflow(root: &Path, name: &str) {
    fs::create_dir_all(root.join(".beislid")).unwrap();
    fs::copy(
        fixture(&format!("beislid-workflows/{name}")),
        root.join(".beislid/workflow.md"),
    )
    .unwrap();
}

fn copy_basic_beislid_workflow(root: &Path) {
    copy_beislid_workflow(root, "basic.md");
}

#[test]
fn import_beislid_workflow_preview_matches_golden_toon() {
    let temp = tempfile::tempdir().unwrap();
    copy_basic_beislid_workflow(temp.path());

    let out = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "import",
        "beislid-workflow",
    ]);

    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        include_str!("golden/import-beislid-preview.toon")
    );
}

#[test]
fn import_beislid_workflow_json_preview_contains_module_drafts_and_diagnostics() {
    let temp = tempfile::tempdir().unwrap();
    copy_basic_beislid_workflow(temp.path());

    let out = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.beislid_import/v1");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["outputs"].as_array().unwrap().len(), 3);
    let gates = doc["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|output| output["module"] == "gates")
        .unwrap();
    assert!(
        gates["content"]
            .as_str()
            .unwrap()
            .contains("cargo test --workspace")
    );
    let codes: Vec<&str> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str())
        .collect();
    assert_eq!(
        codes,
        vec![
            "beislid_import_unsupported",
            "beislid_import_unsupported",
            "beislid_import_unsupported"
        ]
    );
}

#[test]
fn import_beislid_workflow_maps_required_sections() {
    let temp = tempfile::tempdir().unwrap();
    write_minimal_project(temp.path(), "");
    fs::create_dir_all(temp.path().join(".beislid")).unwrap();
    fs::write(
        temp.path().join(".beislid/workflow.md"),
        r#"```beislid:gates
- name: fmt
  command: 'cargo fmt --all --check'
```

```beislid:gate_sets
fast:
  gates: [fmt]
```

```beislid:model_routing
defaults:
  model: gpt-5
  mode: prefer
overrides:
  - skills: [blueprint]
    model: gpt-5-thinking
tiers:
  light: [gpt-5-mini]
tier_mode: prefer
```

```beislid:visual_surfaces
provider: lavish-axi
mode: suggest
artifact_retention: local
workflows:
  ready-for-review: prompt
```

```beislid:workflow_signals
mode: auto
sinks:
  - type: tmux-glance
skills:
  babysit: auto
```

```beislid:guidance
skills: [blueprint, implement]
context:
  - read docs/surface/config-and-envelopes.md
```
"#,
    )
    .unwrap();

    let import = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
        "--write",
    ]);

    assert_eq!(import.status.code(), Some(0), "{}", stdout(&import));
    let import_doc = json(&import);
    assert_eq!(import_doc["ok"], true);
    assert_eq!(
        json_from_file(&temp.path().join(".nopal/gates.jsonc"))["gate_sets"]["fast"]["gates"][0],
        "fmt"
    );
    assert_eq!(
        json_from_file(&temp.path().join(".nopal/integrations.jsonc"))["model_routing"]["defaults"]
            ["model"],
        "gpt-5"
    );
    assert_eq!(
        json_from_file(&temp.path().join(".nopal/integrations.jsonc"))["model_routing"]["overrides"]
            [0]["skills"][0],
        "blueprint"
    );
    assert_eq!(
        json_from_file(&temp.path().join(".nopal/integrations.jsonc"))["visual_surfaces"]["mode"],
        "suggest"
    );
    assert_eq!(
        json_from_file(&temp.path().join(".nopal/integrations.jsonc"))["workflow_signals"]["sinks"]
            [0]["type"],
        "tmux-glance"
    );
    assert_eq!(
        json_from_file(&temp.path().join(".nopal/guidance.jsonc"))["hints"]["skills"][0],
        "blueprint"
    );

    let validate = nopal(&["--dir", temp.path().to_str().unwrap(), "--json", "validate"]);
    assert_eq!(validate.status.code(), Some(0), "{}", stdout(&validate));
}

#[test]
fn import_beislid_workflow_duplicate_fences_keep_first_and_warn() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".beislid")).unwrap();
    fs::write(
        temp.path().join(".beislid/workflow.md"),
        r#"```beislid:gates
- name: first
  command: 'cargo test --workspace'
```

```beislid:gates
- name: second
  command: 'cargo clippy --workspace'
```
"#,
    )
    .unwrap();

    let out = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
    ]);

    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["ok"], true);
    let gates = doc["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|output| output["module"] == "gates")
        .unwrap();
    let content = gates["content"].as_str().unwrap();
    assert!(content.contains("cargo test --workspace"));
    assert!(!content.contains("cargo clippy --workspace"));
    let diagnostics: Vec<&str> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|diagnostic| diagnostic["message"].as_str())
        .collect();
    assert!(
        diagnostics
            .iter()
            .any(|message| message.contains("duplicate beislid block"))
    );
}

#[test]
fn import_beislid_workflow_write_creates_modules_without_overwriting() {
    let temp = tempfile::tempdir().unwrap();
    copy_basic_beislid_workflow(temp.path());

    let write = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
        "--write",
    ]);
    assert_eq!(write.status.code(), Some(0));
    assert_eq!(
        json_from_file(&temp.path().join(".nopal/gates.jsonc"))["version"],
        "nopal.gates/v1"
    );
    assert_eq!(
        json_from_file(&temp.path().join(".nopal/policy.jsonc"))["modes"]["supervised_auto"]["rules"]
            [0]["decision"],
        "allow"
    );
    let policy = json_from_file(&temp.path().join(".nopal/policy.jsonc"));
    assert!(policy["modes"].get("supervised-auto").is_none());
    assert_eq!(
        policy["modes"]["supervised_auto"]["rules"][0]["classes"][0],
        "network_read"
    );
    assert_eq!(
        json_from_file(&temp.path().join(".nopal/integrations.jsonc"))["probe_cache"]["ttl_hours"],
        24
    );

    let blocked = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
        "--write",
    ]);
    assert_eq!(blocked.status.code(), Some(1));
    let doc = json(&blocked);
    let codes: Vec<&str> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|diagnostic| diagnostic["code"].as_str())
        .collect();
    assert!(codes.contains(&"beislid_import_overwrite_blocked"));
}

#[test]
fn import_beislid_workflow_check_accepts_semantically_equal_jsonc_and_preserves_warnings() {
    let temp = tempfile::tempdir().unwrap();
    copy_basic_beislid_workflow(temp.path());

    let write = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
        "--write",
    ]);
    assert_eq!(write.status.code(), Some(0), "{}", stdout(&write));

    let gates_path = temp.path().join(".nopal/gates.jsonc");
    let gates = fs::read_to_string(&gates_path).unwrap();
    fs::write(
        &gates_path,
        gates.replacen(
            '{',
            "{\n  // Local formatting and comments are not drift.\n",
            1,
        ),
    )
    .unwrap();

    let check = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
        "--check",
    ]);

    assert_eq!(check.status.code(), Some(0), "{}", stdout(&check));
    let doc = json(&check);
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["mode"], "check");
    assert!(
        doc["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|output| output["action"] == "checked" && output.get("content").is_none())
    );
    let unsupported = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|diagnostic| diagnostic["code"] == "beislid_import_unsupported")
        .count();
    assert_eq!(unsupported, 3);
}

#[test]
fn import_beislid_workflow_check_matches_golden_toon() {
    let temp = tempfile::tempdir().unwrap();
    copy_basic_beislid_workflow(temp.path());
    let write = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "import",
        "beislid-workflow",
        "--write",
    ]);
    assert_eq!(write.status.code(), Some(0), "{}", stdout(&write));

    let check = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "import",
        "beislid-workflow",
        "--check",
    ]);

    assert_eq!(check.status.code(), Some(0), "{}", stdout(&check));
    assert_eq!(
        stdout(&check),
        include_str!("golden/import-beislid-check.toon")
    );
}

#[test]
fn import_beislid_workflow_check_fails_for_missing_generated_modules() {
    let temp = tempfile::tempdir().unwrap();
    copy_basic_beislid_workflow(temp.path());

    let check = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
        "--check",
    ]);

    assert_eq!(check.status.code(), Some(1), "{}", stdout(&check));
    let doc = json(&check);
    assert_eq!(doc["ok"], false);
    assert!(
        doc["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .all(|output| output["action"] == "missing")
    );
    assert_eq!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|diagnostic| diagnostic["code"] == "beislid_import_missing")
            .count(),
        3
    );
}

#[test]
fn import_beislid_workflow_check_fails_for_semantic_drift_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    copy_basic_beislid_workflow(temp.path());
    let write = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "import",
        "beislid-workflow",
        "--write",
    ]);
    assert_eq!(write.status.code(), Some(0), "{}", stdout(&write));

    let gates_path = temp.path().join(".nopal/gates.jsonc");
    let gates = fs::read_to_string(&gates_path).unwrap();
    let stale = gates.replace("cargo test --workspace", "cargo test -p stale");
    assert_ne!(gates, stale);
    fs::write(&gates_path, &stale).unwrap();

    let check = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
        "--check",
    ]);

    assert_eq!(check.status.code(), Some(1), "{}", stdout(&check));
    assert_eq!(fs::read_to_string(&gates_path).unwrap(), stale);
    let doc = json(&check);
    let gates_output = doc["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|output| output["module"] == "gates")
        .unwrap();
    assert_eq!(gates_output["action"], "drift");
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "beislid_import_drift")
    );
}

#[test]
fn import_beislid_workflow_check_detects_deleted_managed_source_without_writing() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".beislid")).unwrap();
    fs::write(
        temp.path().join(".beislid/workflow.md"),
        r#"```beislid:gates
- name: test
  command: 'cargo test --workspace'
```
"#,
    )
    .unwrap();
    let write = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "import",
        "beislid-workflow",
        "--write",
    ]);
    assert_eq!(write.status.code(), Some(0), "{}", stdout(&write));

    let gates_path = temp.path().join(".nopal/gates.jsonc");
    let stale_gates = fs::read_to_string(&gates_path).unwrap();
    let unrelated_path = temp.path().join(".nopal/manual-notes.jsonc");
    fs::write(&unrelated_path, "{ \"owner\": \"manual\" }\n").unwrap();
    fs::write(
        temp.path().join(".beislid/workflow.md"),
        "# Workflow with the managed gates block removed\n",
    )
    .unwrap();

    let check = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
        "--check",
    ]);

    assert_eq!(check.status.code(), Some(1), "{}", stdout(&check));
    assert_eq!(fs::read_to_string(&gates_path).unwrap(), stale_gates);
    assert_eq!(
        fs::read_to_string(&unrelated_path).unwrap(),
        "{ \"owner\": \"manual\" }\n"
    );
    let doc = json(&check);
    assert_eq!(doc["outputs"].as_array().unwrap().len(), 1);
    assert_eq!(doc["outputs"][0]["module"], "gates");
    assert_eq!(doc["outputs"][0]["action"], "drift");
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "beislid_import_drift"
                && diagnostic["path"] == ".nopal/gates.jsonc")
    );
}

#[test]
fn import_beislid_workflow_check_fails_for_invalid_checked_jsonc() {
    let temp = tempfile::tempdir().unwrap();
    copy_basic_beislid_workflow(temp.path());
    let write = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "import",
        "beislid-workflow",
        "--write",
    ]);
    assert_eq!(write.status.code(), Some(0), "{}", stdout(&write));
    fs::write(temp.path().join(".nopal/gates.jsonc"), "{ invalid").unwrap();

    let check = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
        "--check",
    ]);

    assert_eq!(check.status.code(), Some(1), "{}", stdout(&check));
    let doc = json(&check);
    let gates_output = doc["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .find(|output| output["module"] == "gates")
        .unwrap();
    assert_eq!(gates_output["action"], "invalid");
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "beislid_import_check_parse_error")
    );
}

#[test]
fn import_beislid_workflow_check_is_mutually_exclusive_with_write_flags() {
    let temp = tempfile::tempdir().unwrap();
    copy_basic_beislid_workflow(temp.path());

    for flag in ["--write", "--overwrite"] {
        let out = nopal(&[
            "--dir",
            temp.path().to_str().unwrap(),
            "import",
            "beislid-workflow",
            "--check",
            flag,
        ]);
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("cannot be used with"));
    }
}

#[test]
fn import_beislid_workflow_maps_review_policy_and_split_policy_fences() {
    let temp = tempfile::tempdir().unwrap();
    copy_beislid_workflow(temp.path(), "review-policy.md");

    let import = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "import",
        "beislid-workflow",
        "--write",
    ]);
    assert_eq!(import.status.code(), Some(0), "{}", stdout(&import));
    let import_doc = json(&import);
    assert_eq!(import_doc["ok"], true);

    // The agentic_reviewer host-integration skip is the only diagnostic;
    // it is info-severity, not a warning, and the fence is not dropped.
    let diagnostics = import_doc["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{diagnostics:?}");
    assert_eq!(diagnostics[0]["severity"], "info");
    assert_eq!(diagnostics[0]["code"], "beislid_import_unsupported");
    let message = diagnostics[0]["message"].as_str().unwrap();
    assert!(message.contains("agentic_reviewer.mode"), "{message}");
    assert!(message.contains("agentic_reviewer.provider"), "{message}");
    assert!(message.contains("stay beislid-side"), "{message}");

    let review_policy = json_from_file(&temp.path().join(".nopal/review_policy.jsonc"));
    assert_eq!(review_policy["version"], "nopal.review_policy/v1");
    assert_eq!(review_policy["risk"]["max_auto_closeout_risk"], "low");
    assert_eq!(
        review_policy["risk"]["high_risk_paths"][0],
        "**/.github/workflows/**"
    );
    assert_eq!(review_policy["risk"]["high_risk_paths"][1], "bin/**");
    assert_eq!(review_policy["risk"]["low_risk_paths"][0], "docs/**");
    assert_eq!(review_policy["risk"]["high_risk_file_count"], 12);
    assert_eq!(review_policy["risk"]["high_risk_total_changes"], 500);
    assert_eq!(review_policy["risk"]["low_risk_file_count"], 3);
    assert_eq!(review_policy["risk"]["low_risk_total_changes"], 120);
    assert_eq!(review_policy["split_policy"], "exclusive");
    // agentic_reviewer never round-trips into the nopal module.
    assert!(review_policy.get("agentic_reviewer").is_none());

    let gates = json_from_file(&temp.path().join(".nopal/gates.jsonc"));
    let fmt = gates["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == "fmt")
        .unwrap();
    assert_eq!(fmt["parallel_safe"], true);
    assert_eq!(fmt["mutates"], false);
    let docs_lint = gates["gates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["id"] == "docs-lint")
        .unwrap();
    assert!(docs_lint.get("parallel_safe").is_none());
    assert!(docs_lint.get("mutates").is_none());

    write_minimal_project(temp.path(), "");
    let validate = nopal(&["--dir", temp.path().to_str().unwrap(), "--json", "validate"]);
    assert_eq!(validate.status.code(), Some(0), "{}", stdout(&validate));
}

#[test]
fn migration_bridge_proof_imports_current_workflows_exports_artifacts_redacts_and_detects_drift() {
    let cases: &[(&str, &[&str])] = &[
        (
            "nopal-current.md",
            &["integrations", "gates", "policy", "workflow"],
        ),
        (
            "rondo-runner.md",
            &["integrations", "gates", "policy", "workflow"],
        ),
        (
            "memento-memory-provider.md",
            &["integrations", "gates", "policy", "workflow"],
        ),
        ("review-policy.md", &["gates", "review_policy"]),
    ];

    for (fixture_name, expected_modules) in cases {
        let temp = tempfile::tempdir().unwrap();
        copy_beislid_workflow(temp.path(), fixture_name);

        let import = nopal(&[
            "--dir",
            temp.path().to_str().unwrap(),
            "--json",
            "import",
            "beislid-workflow",
            "--write",
        ]);
        let import_text = stdout(&import);
        assert_eq!(
            import.status.code(),
            Some(0),
            "fixture {fixture_name} import failed\nstdout: {import_text}\nstderr: {}",
            String::from_utf8_lossy(&import.stderr)
        );
        let import_doc: serde_json::Value = serde_json::from_str(&import_text).unwrap();
        assert_eq!(import_doc["kind"], "nopal.beislid_import/v1");
        assert_eq!(import_doc["ok"], true);
        let output_modules: Vec<&str> = import_doc["outputs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|output| output["module"].as_str().unwrap())
            .collect();
        assert_eq!(
            output_modules, *expected_modules,
            "fixture {fixture_name} generated unexpected modules"
        );
        assert!(
            import_doc["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .all(|diagnostic| diagnostic["severity"] != "error"),
            "fixture {fixture_name} import diagnostics: {:?}",
            import_doc["diagnostics"]
        );

        write_minimal_project(
            temp.path(),
            r#",
  "api_token": "migration-proof-token",
  "bearer_literal": "Bearer migration-proof-fake""#,
        );

        let validate = nopal(&["--dir", temp.path().to_str().unwrap(), "--json", "validate"]);
        assert_eq!(
            validate.status.code(),
            Some(0),
            "fixture {fixture_name} generated invalid .nopal drafts\nstdout: {}\nstderr: {}",
            stdout(&validate),
            String::from_utf8_lossy(&validate.stderr)
        );

        let export = nopal(&[
            "--dir",
            temp.path().to_str().unwrap(),
            "--json",
            "export",
            "process",
            "--stdout",
        ]);
        let artifact_text = stdout(&export);
        assert_eq!(
            export.status.code(),
            Some(0),
            "fixture {fixture_name} export failed\nstdout: {artifact_text}\nstderr: {}",
            String::from_utf8_lossy(&export.stderr)
        );
        assert!(
            !artifact_text.contains("migration-proof-token")
                && !artifact_text.contains("Bearer migration-proof-fake"),
            "fixture {fixture_name} leaked fake secret material: {artifact_text}"
        );
        let artifact: serde_json::Value = serde_json::from_str(&artifact_text).unwrap();
        assert_eq!(artifact["kind"], "nopal.process_artifact/v1");
        assert!(
            artifact["sources"][0]["hash"]
                .as_str()
                .unwrap()
                .starts_with("sha256:")
        );
        assert_eq!(artifact["modules"]["manifest"]["api_token"], "<redacted>");
        assert_eq!(
            artifact["modules"]["manifest"]["bearer_literal"],
            "<redacted>"
        );
        for expected_module in *expected_modules {
            assert!(
                artifact["modules"].get(*expected_module).is_some(),
                "fixture {fixture_name} artifact missing module {expected_module}"
            );
        }

        if *fixture_name == "nopal-current.md" {
            let artifact_path = temp.path().join("process-artifact.json");
            let export_file = nopal(&[
                "--dir",
                temp.path().to_str().unwrap(),
                "export",
                "process",
                "--output",
                artifact_path.to_str().unwrap(),
            ]);
            assert_eq!(export_file.status.code(), Some(0));

            let fresh_check = nopal(&[
                "--dir",
                temp.path().to_str().unwrap(),
                "--json",
                "export",
                "process",
                "--output",
                artifact_path.to_str().unwrap(),
                "--check",
            ]);
            assert_eq!(fresh_check.status.code(), Some(0));
            assert_eq!(json(&fresh_check)["ok"], true);

            let manifest_path = temp.path().join(".nopal/nopal.jsonc");
            let manifest = fs::read_to_string(&manifest_path).unwrap();
            fs::write(&manifest_path, format!("{manifest}\n")).unwrap();
            let stale_check = nopal(&[
                "--dir",
                temp.path().to_str().unwrap(),
                "--json",
                "export",
                "process",
                "--output",
                artifact_path.to_str().unwrap(),
                "--check",
            ]);
            assert_eq!(stale_check.status.code(), Some(1));
            let stale_doc = json(&stale_check);
            assert!(
                stale_doc["diagnostics"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|diagnostic| diagnostic["code"] == "process_artifact_drift")
            );
        }
    }
}

#[test]
fn export_process_rejects_conflicting_stdout_flags() {
    let temp = tempfile::tempdir().unwrap();
    write_minimal_project(temp.path(), "");
    let artifact_path = temp.path().join("artifact.json");

    for args in [
        vec![
            "--dir",
            temp.path().to_str().unwrap(),
            "export",
            "process",
            "--stdout",
            "--output",
            artifact_path.to_str().unwrap(),
        ],
        vec![
            "--dir",
            temp.path().to_str().unwrap(),
            "export",
            "process",
            "--stdout",
            "--check",
        ],
    ] {
        let out = nopal(&args);
        assert_eq!(out.status.code(), Some(2), "args: {args:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr).contains("cannot be used with"),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn export_process_check_reports_missing_stale_and_unparseable_artifacts() {
    let temp = tempfile::tempdir().unwrap();
    write_minimal_project(temp.path(), "");
    let artifact_path = temp.path().join("artifact.json");

    let missing = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "export",
        "process",
        "--output",
        artifact_path.to_str().unwrap(),
        "--check",
    ]);
    assert_eq!(missing.status.code(), Some(1));
    assert_eq!(
        json(&missing)["diagnostics"][0]["code"],
        "process_artifact_missing"
    );

    fs::write(&artifact_path, "{}\n").unwrap();
    let stale = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "export",
        "process",
        "--output",
        artifact_path.to_str().unwrap(),
        "--check",
    ]);
    assert_eq!(stale.status.code(), Some(1));
    assert_eq!(
        json(&stale)["diagnostics"][0]["code"],
        "process_artifact_drift"
    );

    fs::write(&artifact_path, "{").unwrap();
    let unparseable = nopal(&[
        "--dir",
        temp.path().to_str().unwrap(),
        "--json",
        "export",
        "process",
        "--output",
        artifact_path.to_str().unwrap(),
        "--check",
    ]);
    assert_eq!(unparseable.status.code(), Some(1));
    assert_eq!(
        json(&unparseable)["diagnostics"][0]["code"],
        "process_artifact_parse_error"
    );
}

#[test]
fn validate_minimal_example_is_ok() {
    let out = nopal(&["validate", "--dir", &example("minimal")]);
    let text = stdout(&out);
    assert_eq!(
        out.status.code(),
        Some(0),
        "expected exit 0, got {:?}\nstdout: {text}\nstderr: {}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(text.contains("kind: nopal.validation/v1"), "stdout: {text}");
    assert!(text.contains("ok: true"), "stdout: {text}");
}

#[test]
fn validate_minimal_matches_golden_toon() {
    let out = nopal(&["validate", "--dir", &example("minimal")]);
    assert_eq!(stdout(&out), include_str!("golden/validate-minimal.toon"));
}

#[test]
fn status_minimal_matches_golden_toon_and_exits_zero() {
    // Explicit `status` subcommand: bare invocation opens the field
    //; the launcher lives at `nopal cli`.
    let out = nopal(&["--dir", &example("minimal"), "status"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(stdout(&out), include_str!("golden/status-minimal.toon"));
}

#[test]
fn status_with_errors_matches_golden_and_still_exits_zero() {
    let out = nopal(&["--dir", &example("portable-missing-policy"), "status"]);
    assert_eq!(out.status.code(), Some(0), "status is informational");
    assert_eq!(
        stdout(&out),
        include_str!("golden/status-portable-missing-policy.toon")
    );
}

#[test]
fn status_uninitialized_project_matches_golden_with_setup_help() {
    // `examples/` itself has no `.nopal/` of its own, but it sits inside
    // this repo's own worktree, which does have one at its root (nopal
    // dogfoods itself) - git-rooted discovery would walk up from
    // `examples/` and find that real config instead of reporting
    // uninitialized. An isolated, non-git tempdir has no repo to walk in
    // the first place, so discovery anchors at the dir itself and this
    // stays a true "nothing configured anywhere" case.
    let temp = tempfile::tempdir().unwrap();
    let out = nopal(&["--dir", temp.path().to_str().unwrap(), "status"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        include_str!("golden/status-uninitialized.toon")
    );
}

#[test]
fn validate_missing_required_module_exits_one_with_module_missing() {
    let out = nopal(&["validate", "--dir", &example("portable-missing-policy")]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("ok: false"), "stdout: {text}");
    assert!(text.contains("module_missing"), "stdout: {text}");
}

#[test]
fn validate_broken_manifest_exits_one_with_position() {
    let out = nopal(&["validate", "--dir", &example("bad-jsonc")]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("manifest_parse_error"), "stdout: {text}");
    assert!(text.contains("3:"), "position should be on line 3: {text}");
}

#[test]
fn validate_valid_profiles_all_exit_zero() {
    for name in ["minimal", "portable", "nopal"] {
        let out = nopal(&["validate", "--dir", &example(name)]);
        assert_eq!(out.status.code(), Some(0), "example {name} should be ok");
    }
}

#[test]
fn manifest_defined_profile_requires_modules_without_core_variant() {
    let temp = tempfile::tempdir().unwrap();
    fs::create_dir_all(temp.path().join(".nopal")).unwrap();
    fs::write(
        temp.path().join(".nopal/nopal.jsonc"),
        r#"{
  "version": "nopal.project/v1",
  "project": { "name": "custom-profile" },
  "profile": "trail",
  "profiles": {
    "trail": { "required_modules": ["gates"] }
  }
}
"#,
    )
    .unwrap();
    fs::write(
        temp.path().join(".nopal/gates.jsonc"),
        r#"{
  "version": "nopal.gates/v1",
  "gates": []
}
"#,
    )
    .unwrap();

    let out = nopal(&["validate", "--dir", temp.path().to_str().unwrap(), "--json"]);
    assert_eq!(out.status.code(), Some(0), "{}", stdout(&out));
    let doc = json(&out);
    assert_eq!(doc["profile"], "trail");
    assert_eq!(doc["ok"], true);
}

#[test]
fn validate_invalid_workflow_integrations_and_guidance_examples_fail() {
    let cases = [
        ("workflow-invalid", "workflow_event_unknown"),
        ("workflow-invalid", "workflow_action_type_unknown"),
        ("integrations-invalid", "integration_provider_invalid"),
        ("integrations-invalid", "workflow_event_unknown"),
        ("guidance-invalid", "guidance_authority_invalid"),
    ];
    for (name, code) in cases {
        let out = nopal(&["validate", "--dir", &example(name), "--json"]);
        assert_eq!(out.status.code(), Some(1), "example {name} should fail");
        let doc = json(&out);
        let diagnostics = doc["diagnostics"].as_array().unwrap();
        assert!(
            diagnostics.iter().any(|d| d["code"] == code),
            "missing code {code} in {diagnostics:?}"
        );
    }
}

#[test]
fn json_status_emits_versioned_envelope() {
    let out = nopal(&["--dir", &example("minimal"), "--json", "status"]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.status/v1");
    assert_eq!(doc["ready"], true);
    assert_eq!(doc["project"], "minimal-example");
    assert_eq!(doc["profile"], "minimal");
    assert_eq!(doc["modules"].as_array().unwrap().len(), 7);
    assert!(!doc["help"].as_array().unwrap().is_empty());
}

#[test]
fn bundle_valid_example_would_exec_with_four_resolved_resources() {
    let out = nopal(&[
        "cli",
        "--dir",
        &example("bundle-valid"),
        "--json",
        "--dry-run",
    ]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.launch/v1");
    assert_eq!(doc["ok"], true, "{doc}");
    assert_eq!(doc["would_exec"], true, "{doc}");
    assert_eq!(doc["bundle"]["resources"].as_array().unwrap().len(), 4);
    assert!(!doc["pi_argv"].as_array().unwrap().is_empty());
}

#[test]
fn bundle_invalid_example_fails_closed_with_bundle_resource_missing() {
    let out = nopal(&[
        "cli",
        "--dir",
        &example("bundle-invalid"),
        "--json",
        "--dry-run",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["ok"], false, "{doc}");
    assert_eq!(doc["would_exec"], false, "{doc}");
    assert!(
        doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["code"] == "bundle_resource_missing"),
        "{doc}"
    );
}

#[test]
fn json_validate_reports_stable_codes() {
    let out = nopal(&["validate", "--dir", &example("bad-version"), "--json"]);
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.validation/v1");
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["diagnostics"][0]["code"], "version_unsupported");
    assert_eq!(doc["diagnostics"][0]["severity"], "error");
}

#[test]
fn gates_list_portable_matches_golden_toon() {
    let out = nopal(&["gates", "list", "--dir", &example("portable")]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        include_str!("golden/gates-list-portable.toon")
    );
}

#[test]
fn preflights_list_portable_matches_golden_toon() {
    let out = nopal(&["preflights", "list", "--dir", &example("portable")]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        include_str!("golden/preflights-list-portable.toon")
    );
}

#[test]
fn gates_select_portable_matches_golden_toon() {
    let out = nopal(&[
        "gates",
        "select",
        "--stage",
        "pre_pr",
        "--changed-files",
        "crates/nopal-core/src/lib.rs,README.md",
        "--dir",
        &example("portable"),
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        include_str!("golden/gates-select-portable.toon")
    );
}

#[test]
fn gates_select_is_insensitive_to_changed_file_order_and_flag_shape() {
    let combined = nopal(&[
        "gates",
        "select",
        "--stage",
        "pre_pr",
        "--changed-files",
        "crates/nopal-core/src/lib.rs,README.md",
        "--dir",
        &example("portable"),
    ]);
    let repeated = nopal(&[
        "gates",
        "select",
        "--stage",
        "pre_pr",
        "--changed-files",
        "README.md",
        "--changed-files",
        "crates/nopal-core/src/lib.rs",
        "--dir",
        &example("portable"),
    ]);
    assert_eq!(stdout(&combined), stdout(&repeated));
}

#[test]
fn gates_select_without_selectors_default_selects_matching_stage() {
    let out = nopal(&[
        "gates",
        "select",
        "--stage",
        "pre_pr",
        "--dir",
        &example("nopal"),
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        include_str!("golden/gates-select-nopal-default.toon")
    );
}

#[test]
fn gates_list_invalid_reports_every_structured_diagnostic() {
    let out = nopal(&["gates", "list", "--dir", &example("gates-invalid")]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(stdout(&out), include_str!("golden/gates-list-invalid.toon"));
}

#[test]
fn gates_select_on_missing_gates_file_exits_one_with_module_missing() {
    let out = nopal(&[
        "gates",
        "select",
        "--stage",
        "pre_pr",
        "--dir",
        &example("minimal"),
    ]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("module_missing"), "stdout: {text}");
    assert!(text.contains("selected[0]"), "stdout: {text}");
}

#[test]
fn json_gates_select_emits_versioned_envelope_with_explanations() {
    let out = nopal(&[
        "gates",
        "select",
        "--stage",
        "pre_pr",
        "--changed-files",
        "a.rs",
        "--dir",
        &example("portable"),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.gates.select/v1");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["stage"], "pre_pr");
    assert_eq!(doc["selected"][0]["id"], "fmt");
    assert_eq!(
        doc["selected"][0]["run"]["command"],
        "cargo fmt --all --check"
    );
    assert_eq!(
        doc["selected"][0]["via"]["selector"]["selector"],
        "rust-files"
    );
    assert_eq!(doc["selectors"][1]["matched"], false);
}

#[test]
fn json_gates_list_reports_stable_codes_on_invalid_config() {
    let out = nopal(&[
        "gates",
        "list",
        "--dir",
        &example("gates-invalid"),
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.gates.list/v1");
    assert_eq!(doc["ok"], false);
    let codes: Vec<&str> = doc["diagnostics"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["code"].as_str().unwrap())
        .collect();
    assert_eq!(
        codes,
        vec![
            "version_unsupported",
            "stage_unknown",
            "placeholder_unknown",
            "duplicate_id",
            "gate_ref_unknown",
            "gate_set_unknown",
        ]
    );
}

#[test]
fn gates_select_unknown_stage_is_warned_and_kept_usable() {
    let out = nopal(&[
        "gates",
        "select",
        "--stage",
        "someday",
        "--dir",
        &example("portable"),
    ]);
    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(stdout.contains("warning"), "stdout: {stdout}");
}

#[test]
fn unknown_flag_is_a_usage_error_exit_two() {
    let out = nopal(&["--no-such-flag"]);
    assert_eq!(out.status.code(), Some(2));
}

#[test]
fn policy_decide_matches_golden_toon_and_exits_zero() {
    let out = nopal(&[
        "policy",
        "decide",
        "--dir",
        &example("nopal"),
        "--mode",
        "rondo_afk",
        "--action",
        "git.push",
        "--class",
        "git_remote",
        "--env",
        "LINEAR_API_KEY",
        "--env",
        "NO_SUCH_VAR",
    ]);
    assert_eq!(out.status.code(), Some(0), "verdicts never gate the exit");
    assert_eq!(
        stdout(&out),
        include_str!("golden/policy-decide-nopal-afk.toon")
    );
}

#[test]
fn policy_evaluate_matches_golden_toon() {
    let out = nopal(&[
        "policy",
        "evaluate",
        "--dir",
        &example("nopal"),
        "--mode",
        "nopal_tui",
        "--action",
        "fs.rm",
        "--class",
        "destructive",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        include_str!("golden/policy-evaluate-tui-destructive.toon")
    );
}

#[test]
fn policy_placement_builtin_default_matches_golden_toon() {
    let out = nopal(&[
        "policy",
        "placement",
        "--dir",
        &example("nopal"),
        "--mode",
        "ci",
        "--action",
        "git.push",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        include_str!("golden/policy-placement-ci-builtin.toon")
    );
}

#[test]
fn json_policy_decide_emits_versioned_envelope_with_explanations() {
    let out = nopal(&[
        "policy",
        "decide",
        "--dir",
        &example("nopal"),
        "--mode",
        "rondo_afk",
        "--action",
        "git.push",
        "--class",
        "git_remote",
        "--env",
        "LINEAR_API_KEY",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.policy_decision/v1");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["mode"], "rondo_afk");
    assert_eq!(doc["decision"], "allow");
    assert_eq!(doc["decision_source"], "rule");
    assert_eq!(doc["placement"], "dedicated_run_runtime");
    assert_eq!(doc["placement_source"], "rule");
    assert_eq!(
        doc["classes"],
        serde_json::json!(["git_remote", "secret_bearing"]),
        "env ref classification joins the effective classes"
    );
    assert_eq!(doc["matched_rules"].as_array().unwrap().len(), 2);
    assert!(!doc["explanation"].as_array().unwrap().is_empty());
}

#[test]
fn json_policy_evaluate_mode_default_applies_when_no_rule_matches() {
    let out = nopal(&[
        "policy",
        "evaluate",
        "--dir",
        &example("portable"),
        "--mode",
        "supervised_auto",
        "--action",
        "gh.pr.create",
        "--class",
        "git_remote",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["decision"], "ask");
    assert_eq!(doc["decision_source"], "mode_default");
    assert_eq!(doc["matched_rules"], serde_json::json!([]));
    assert!(
        doc.get("placement").is_none(),
        "evaluate reports the decision verdict only"
    );
}

#[test]
fn policy_without_policy_module_exits_one_with_module_missing() {
    let out = nopal(&[
        "policy",
        "decide",
        "--dir",
        &example("minimal"),
        "--mode",
        "manual",
        "--action",
        "git.push",
        "--json",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.policy_decision/v1");
    assert_eq!(doc["ok"], false);
    assert_eq!(doc["diagnostics"][0]["code"], "module_missing");
    assert!(doc.get("decision").is_none(), "no verdict without config");
}

#[test]
fn policy_with_invalid_policy_module_exits_one_with_policy_codes() {
    let out = nopal(&[
        "policy",
        "evaluate",
        "--dir",
        &example("portable-bad-policy"),
        "--mode",
        "rondo_afk",
        "--action",
        "git.push",
        "--json",
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
    for expected in [
        "policy_rule_invalid",
        "policy_rule_duplicate_id",
        "policy_decision_invalid",
        "policy_env_invalid",
        "policy_placement_invalid",
    ] {
        assert!(codes.contains(&expected), "missing {expected}: {codes:?}");
    }
}

#[test]
fn validate_deep_checks_policy_schema() {
    let out = nopal(&["validate", "--dir", &example("portable-bad-policy")]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("policy_class_unknown"), "stdout: {text}");
    assert!(text.contains("policy_env_invalid"), "stdout: {text}");
}

#[test]
fn validate_deep_checks_gates_schema() {
    let out = nopal(&["validate", "--dir", &example("gates-invalid")]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("version_unsupported"), "stdout: {text}");
    assert!(text.contains("stage_unknown"), "stdout: {text}");
    assert!(text.contains("duplicate_id"), "stdout: {text}");
    assert!(text.contains("gate_set_unknown"), "stdout: {text}");
}

#[test]
fn policy_unknown_mode_or_class_degrades_conservatively() {
    let base = example("nopal");
    let unknown_mode = nopal(&[
        "policy", "evaluate", "--dir", &base, "--mode", "yolo", "--action", "x.y", "--json",
    ]);
    assert_eq!(unknown_mode.status.code(), Some(0));
    let mode_doc = json(&unknown_mode);
    assert_eq!(mode_doc["ok"], true);
    assert_eq!(mode_doc["mode"], "yolo");
    assert_eq!(mode_doc["decision"], "ask");
    assert!(
        mode_doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "policy_mode_unknown")
    );

    let unknown_class = nopal(&[
        "policy",
        "decide",
        "--dir",
        &base,
        "--mode",
        "ci",
        "--action",
        "x.y",
        "--class",
        "warp_core",
        "--json",
    ]);
    assert_eq!(unknown_class.status.code(), Some(0));
    let class_doc = json(&unknown_class);
    assert_eq!(class_doc["classes"], serde_json::json!(["warp_core"]));
    assert_eq!(class_doc["decision"], "deny");
    assert_eq!(class_doc["placement"], "dedicated_run_runtime");
    assert!(
        class_doc["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|diagnostic| diagnostic["code"] == "policy_class_unknown")
    );
}

#[test]
fn policy_output_is_deterministic_across_runs() {
    let nopal_example = example("nopal");
    let cases: &[&[&str]] = &[
        &[
            "policy",
            "evaluate",
            "--dir",
            &nopal_example,
            "--mode",
            "nopal_tui",
            "--action",
            "shell.rm",
            "--class",
            "destructive",
        ],
        &[
            "policy",
            "placement",
            "--dir",
            &nopal_example,
            "--mode",
            "ci",
            "--action",
            "cargo.test",
        ],
        &[
            "policy",
            "decide",
            "--dir",
            &nopal_example,
            "--mode",
            "rondo_afk",
            "--action",
            "git.push",
            "--class",
            "git_remote",
            "--env",
            "LINEAR_API_KEY",
        ],
    ];
    for args in cases {
        assert_eq!(stdout(&nopal(args)), stdout(&nopal(args)), "args: {args:?}");
    }
}

#[test]
fn output_is_deterministic_across_runs() {
    let first = stdout(&nopal(&["--dir", &example("portable-missing-policy")]));
    let second = stdout(&nopal(&["--dir", &example("portable-missing-policy")]));
    assert_eq!(first, second);
}

#[test]
fn info_json_reports_version_and_capabilities() {
    let out = nopal(&["info", "--json"]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.info/v1");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(doc["commit"], serde_json::Value::Null);

    // The full sorted capability list is pinned deliberately: adding or
    // removing a top-level subcommand must consciously update this test,
    // the same idiom golden TOON files use for renderer drift.
    let capabilities: Vec<&str> = doc["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        capabilities,
        vec![
            "ask",
            "export",
            "gates",
            "import",
            "info",
            "ledger",
            "placement",
            "policy",
            "preflights",
            "review-risk",
            "status",
            "validate",
            "workflow",
        ]
    );
}

#[test]
fn info_toon_and_json_come_from_the_same_report() {
    let toon_out = nopal(&["info"]);
    let toon = stdout(&toon_out);
    assert!(toon.contains("kind: nopal.info/v1"), "stdout: {toon}");
    assert!(toon.contains("capabilities"), "stdout: {toon}");

    let json_out = nopal(&["info", "--json"]);
    let doc = json(&json_out);
    assert_eq!(doc["kind"], "nopal.info/v1");
    // Consumer-style feature detection exposes public deterministic seams,
    // never hidden adapter or legacy coordination commands.
    let capabilities = doc["capabilities"].as_array().unwrap();
    assert!(capabilities.iter().any(|value| value == "policy"));
    assert!(!capabilities.iter().any(|value| value == "enforcement"));
}

#[test]
fn info_exits_zero_without_a_nopal_directory() {
    // `info` must work outside any project: no `.nopal/` module, no
    // read/write side effects on the target directory at all.
    let temp = tempfile::tempdir().unwrap();
    let out = nopal(&["info", "--json", "--dir", &temp.path().to_string_lossy()]);
    assert_eq!(out.status.code(), Some(0));
    assert!(!temp.path().join(".nopal").exists());
}

#[test]
fn herdr_bridge_help_documents_the_conservative_default_interval() {
    let out = nopal(&["bridge", "herdr", "--help"]);
    assert_eq!(out.status.code(), Some(0));
    let help = stdout(&out);
    assert!(help.contains("--interval <INTERVAL>"), "stdout: {help}");
    assert!(help.contains("[default: 5]"), "stdout: {help}");
}

#[test]
fn review_risk_multi_verdict_matches_golden_toon() {
    let out = nopal(&[
        "--dir",
        &example("review-policy"),
        "review-risk",
        "--changed-files",
        "crates/nopal-core/src/review_policy.rs,README.md",
        "--total-changes",
        "50",
        "--base-fresh",
        "--stage",
        "pre_pr",
    ]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        stdout(&out),
        include_str!("golden/review-risk-multi-verdict.toon")
    );
}

#[test]
fn review_risk_json_emits_versioned_envelope() {
    let out = nopal(&[
        "--dir",
        &example("review-policy"),
        "--json",
        "review-risk",
        "--changed-files",
        "crates/nopal-core/src/review_policy.rs,README.md",
        "--total-changes",
        "50",
        "--base-fresh",
        "--stage",
        "pre_pr",
    ]);
    assert_eq!(out.status.code(), Some(0));
    let doc = json(&out);
    assert_eq!(doc["kind"], "nopal.review_risk/v1");
    assert_eq!(doc["ok"], true);
    assert_eq!(doc["risk"]["class"], "medium");
    assert_eq!(doc["risk"]["agentic_reviewer_required"], true);
    assert_eq!(doc["fast_path"]["eligible"], false);
    assert_eq!(
        doc["fast_path"]["reason"],
        "multi_scope_gates_not_parallel_safe"
    );
    assert_eq!(doc["scopes"]["violation"], true);
    assert_eq!(doc["scopes"]["touched"][0], "rust-files");
    assert_eq!(doc["scopes"]["touched"][1], "doc-files");
}

#[test]
fn review_risk_on_missing_module_exits_one_with_module_missing() {
    let out = nopal(&[
        "--dir",
        &example("minimal"),
        "review-risk",
        "--changed-files",
        "a.rs",
    ]);
    assert_eq!(out.status.code(), Some(1));
    let text = stdout(&out);
    assert!(text.contains("module_missing"), "stdout: {text}");
    assert!(text.contains("ok: false"), "stdout: {text}");
}

#[test]
fn review_risk_json_and_toon_render_the_same_verdicts() {
    let args = [
        "--dir",
        &example("review-policy"),
        "review-risk",
        "--changed-files",
        "crates/nopal-core/src/review_policy.rs,README.md",
        "--total-changes",
        "50",
        "--base-fresh",
        "--stage",
        "pre_pr",
    ];
    let toon_out = nopal(&args);
    let mut json_args = args.to_vec();
    json_args.insert(0, "--json");
    let json_out = nopal(&json_args);
    assert_eq!(toon_out.status.code(), Some(0));
    assert_eq!(json_out.status.code(), Some(0));

    let toon_text = stdout(&toon_out);
    let doc = json(&json_out);
    assert!(toon_text.contains(&format!(
        "class: {}",
        doc["risk"]["class"].as_str().unwrap()
    )));
    assert!(toon_text.contains(&format!(
        "reason: {}",
        doc["fast_path"]["reason"].as_str().unwrap()
    )));
    assert!(toon_text.contains(&format!(
        "violation: {}",
        doc["scopes"]["violation"].as_bool().unwrap()
    )));
}
