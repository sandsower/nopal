#![allow(clippy::unwrap_used)]

use std::fs;

use nopal_core::gate_scaffold::{self, Readiness};

type FixtureFile<'a> = (&'a str, &'a str);
type MatrixCase<'a> = (&'a str, &'a [FixtureFile<'a>], &'a [&'a str]);

fn write_files(root: &std::path::Path, files: &[FixtureFile<'_>]) {
    for (path, contents) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, contents).unwrap();
    }
}

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
fn doctor_trace_records_skipped_templates_with_stable_reasons() {
    let temp = tempfile::tempdir().unwrap();
    write_files(temp.path(), &[("Cargo.toml", "[workspace]\nmembers=[]\n")]);
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    assert!(plan.decisions.iter().any(|decision| {
        decision.template_id.as_deref() == Some("javascript.npm/v1")
            && decision.outcome.as_str() == "skipped"
            && decision.reason_code == "template_evidence_absent"
    }));
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

    let unknown = rendered.replace("rust.cargo/v1", "rust.unknown/v1");
    let (_, diagnostics) = nopal_core::gates::parse_gates(&unknown, ".nopal/gates.jsonc");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::ScaffoldTemplateInvalid
    }));

    let mut incomplete: serde_json::Value = serde_json::from_str(&rendered).unwrap();
    incomplete["scaffold"]["generated_gate_ids"] = serde_json::json!([]);
    let (_, diagnostics) = nopal_core::gates::parse_gates(
        &serde_json::to_string(&incomplete).unwrap(),
        ".nopal/gates.jsonc",
    );
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::FieldInvalid
            && diagnostic.message.contains("generated gate")
    }));
}

#[test]
fn explicit_gate_selection_suppresses_generated_template_gates() {
    let temp = tempfile::tempdir().unwrap();
    write_files(temp.path(), &[("Cargo.toml", "[workspace]\nmembers=[]\n")]);
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&plan.gates_json().unwrap()).unwrap();
    value["gates"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "explicit",
            "stage": "pre_pr",
            "argv": ["true"]
        }));
    let (config, diagnostics) = nopal_core::gates::parse_gates(
        &serde_json::to_string(&value).unwrap(),
        ".nopal/gates.jsonc",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let selected =
        nopal_core::selection::select(&config.unwrap(), nopal_core::gates::GateStage::PrePr, &[]);
    assert_eq!(
        selected
            .selected
            .iter()
            .map(|gate| gate.id.as_str())
            .collect::<Vec<_>>(),
        vec!["explicit"]
    );
    assert!(
        selected
            .skipped
            .iter()
            .any(|gate| { gate.reason.as_str() == "superseded_by_explicit_authority" })
    );
}

#[test]
fn partially_matched_explicit_selectors_do_not_suppress_uncovered_generated_proof() {
    let temp = tempfile::tempdir().unwrap();
    write_files(temp.path(), &[("Cargo.toml", "[workspace]\nmembers=[]\n")]);
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&plan.gates_json().unwrap()).unwrap();
    value["gates"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "explicit-rust",
            "stage": "pre_pr",
            "argv": ["cargo", "test"]
        }));
    value["gate_sets"] = serde_json::json!({
        "rust": {"gates": ["explicit-rust"]}
    });
    value["selectors"] = serde_json::json!([{
        "name": "rust",
        "paths": ["**/*.rs"],
        "gate_sets": ["rust"]
    }]);
    let (config, diagnostics) = nopal_core::gates::parse_gates(
        &serde_json::to_string(&value).unwrap(),
        ".nopal/gates.jsonc",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let config = config.unwrap();

    let fully_covered = nopal_core::selection::select(
        &config,
        nopal_core::gates::GateStage::PrePr,
        &["src/lib.rs".to_owned()],
    );
    assert_eq!(
        fully_covered
            .selected
            .iter()
            .map(|gate| gate.id.as_str())
            .collect::<Vec<_>>(),
        vec!["explicit-rust"]
    );

    let partially_covered = nopal_core::selection::select(
        &config,
        nopal_core::gates::GateStage::PrePr,
        &["README.md".to_owned(), "src/lib.rs".to_owned()],
    );
    let selected = partially_covered
        .selected
        .iter()
        .map(|gate| gate.id.as_str())
        .collect::<Vec<_>>();
    assert!(selected.contains(&"explicit-rust"));
    assert!(selected.contains(&"detected.root.baseline.diff-check"));
}

#[test]
fn preflight_only_authority_does_not_suppress_generated_pre_pr_gates() {
    let temp = tempfile::tempdir().unwrap();
    write_files(temp.path(), &[("Cargo.toml", "[workspace]\nmembers=[]\n")]);
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&plan.gates_json().unwrap()).unwrap();
    value["preflights"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "readiness",
            "stage": "run_start",
            "argv": ["true"]
        }));
    let (config, diagnostics) = nopal_core::gates::parse_gates(
        &serde_json::to_string(&value).unwrap(),
        ".nopal/gates.jsonc",
    );
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let config = config.unwrap();
    assert!(!config.has_explicit_gates());
    let selected = nopal_core::selection::select(&config, nopal_core::gates::GateStage::PrePr, &[]);
    assert!(
        selected
            .selected
            .iter()
            .any(|gate| gate.id.contains("cargo-test"))
    );
    assert!(
        !selected
            .skipped
            .iter()
            .any(|gate| { gate.reason.as_str() == "superseded_by_explicit_authority" })
    );
}

#[test]
fn explicit_gate_precedence_is_limited_to_the_same_stage() {
    let temp = tempfile::tempdir().unwrap();
    write_files(temp.path(), &[("Cargo.toml", "[workspace]\nmembers=[]\n")]);
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&plan.gates_json().unwrap()).unwrap();
    value["gates"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "post-release",
            "stage": "post_pr",
            "argv": ["true"]
        }));
    let checked = serde_json::to_string(&value).unwrap();
    assert!(plan.matches_checked_generated(&checked));
    let (config, diagnostics) = nopal_core::gates::parse_gates(&checked, ".nopal/gates.jsonc");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
    let selected =
        nopal_core::selection::select(&config.unwrap(), nopal_core::gates::GateStage::PrePr, &[]);
    assert!(
        selected
            .selected
            .iter()
            .any(|gate| gate.id.contains("cargo-test"))
    );
    assert!(
        !selected
            .skipped
            .iter()
            .any(|gate| { gate.reason.as_str() == "superseded_by_explicit_authority" })
    );
}

#[test]
fn generated_unknown_baseline_cannot_claim_ready_without_an_explicit_gate() {
    let temp = tempfile::tempdir().unwrap();
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    let text = plan
        .gates_json()
        .unwrap()
        .replace("needs_configuration", "ready");
    let (_, diagnostics) = nopal_core::gates::parse_gates(&text, ".nopal/gates.jsonc");
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::GateConfigurationRequired
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

#[derive(serde::Deserialize)]
struct RootFixture {
    name: String,
    files: std::collections::BTreeMap<String, String>,
    templates: Vec<String>,
}

fn root_fixtures() -> Vec<RootFixture> {
    serde_json::from_str(include_str!("fixtures/gate-scaffold/root-matrix.json")).unwrap()
}

#[test]
fn checked_in_root_fixture_matrix_covers_every_selectable_template() {
    let fixtures = root_fixtures();
    let mut covered = std::collections::BTreeSet::from(["baseline.git/v1".to_owned()]);
    for fixture in fixtures {
        let temp = tempfile::tempdir().unwrap();
        for (path, contents) in &fixture.files {
            write_files(temp.path(), &[(path, contents)]);
        }
        let plan = gate_scaffold::inspect(temp.path()).unwrap();
        let selected = plan
            .selected_template_ids()
            .into_iter()
            .filter(|id| *id != "baseline.git/v1")
            .collect::<Vec<_>>();
        assert_eq!(selected, fixture.templates, "fixture {}", fixture.name);
        covered.extend(fixture.templates);
    }
    assert_eq!(
        covered,
        nopal_core::gate_scaffold::TEMPLATE_IDS
            .into_iter()
            .map(ToOwned::to_owned)
            .collect::<std::collections::BTreeSet<_>>()
    );
}

#[test]
fn every_root_fixture_has_negative_ambiguity_and_composition_proof() {
    let empty = tempfile::tempdir().unwrap();
    let empty_plan = gate_scaffold::inspect(empty.path()).unwrap();
    for template_id in nopal_core::gate_scaffold::TEMPLATE_IDS {
        if template_id != "baseline.git/v1" {
            assert!(!empty_plan.selected_template_ids().contains(&template_id));
        }
    }

    for fixture in root_fixtures() {
        let ambiguous = tempfile::tempdir().unwrap();
        for (path, contents) in &fixture.files {
            write_files(ambiguous.path(), &[(path, contents)]);
        }
        write_files(
            ambiguous.path(),
            &[
                ("GNUmakefile", "check:\n\t@true\n"),
                ("Justfile", "verify:\n    true\n"),
            ],
        );
        let ambiguous_plan = gate_scaffold::inspect(ambiguous.path()).unwrap();
        assert_eq!(
            ambiguous_plan.readiness,
            Readiness::Blocked,
            "ambiguity fixture {}",
            fixture.name
        );

        let composed = tempfile::tempdir().unwrap();
        for (path, contents) in &fixture.files {
            write_files(composed.path(), &[(path, contents)]);
        }
        write_files(
            composed.path(),
            &[
                ("go.mod", "module example.test/composed\n"),
                ("Package.swift", "// swift-tools-version: 6.0\n"),
            ],
        );
        let composed_plan = gate_scaffold::inspect(composed.path()).unwrap();
        for template in &fixture.templates {
            assert!(
                composed_plan
                    .selected_template_ids()
                    .contains(&template.as_str()),
                "composition fixture {} omitted {template}",
                fixture.name
            );
        }
        assert_eq!(
            composed_plan.readiness,
            Readiness::Ready,
            "{}",
            fixture.name
        );
        if fixture
            .templates
            .iter()
            .any(|template| template.starts_with("task."))
        {
            assert!(composed_plan.decisions.iter().any(|decision| {
                decision.outcome.as_str() == "superseded"
                    && matches!(
                        decision.template_id.as_deref(),
                        Some("go.test/v1" | "swift.spm/v1")
                    )
            }));
        } else {
            assert!(composed_plan.templates.len() >= 3, "{}", fixture.name);
        }
    }
}

#[test]
fn root_ecosystem_matrix_selects_only_evidence_backed_templates() {
    let cases: &[MatrixCase<'_>] = &[
        (
            "rust",
            &[("Cargo.toml", "[package]\nname='x'\nversion='0.1.0'\n")],
            &["rust.cargo/v1"],
        ),
        (
            "npm",
            &[
                ("package.json", r#"{"scripts":{"test":"node test.js"}}"#),
                ("package-lock.json", "{}"),
            ],
            &["javascript.npm/v1"],
        ),
        (
            "pnpm",
            &[
                ("package.json", r#"{"scripts":{"lint":"eslint ."}}"#),
                ("pnpm-lock.yaml", "lockfileVersion: '9.0'"),
            ],
            &["javascript.pnpm/v1"],
        ),
        (
            "yarn",
            &[
                (
                    "package.json",
                    r#"{"scripts":{"typecheck":"tsc --noEmit"}}"#,
                ),
                ("yarn.lock", "# yarn"),
            ],
            &["javascript.yarn/v1"],
        ),
        (
            "bun",
            &[
                ("package.json", r#"{"scripts":{"check":"bun test"}}"#),
                ("bun.lock", "{}"),
            ],
            &["javascript.bun/v1"],
        ),
        (
            "python",
            &[(
                "pyproject.toml",
                "[tool.pytest.ini_options]\n[tool.ruff]\n[tool.mypy]\n",
            )],
            &["python.pytest/v1", "python.ruff/v1", "python.mypy/v1"],
        ),
        (
            "go",
            &[("go.mod", "module example.test/demo\n")],
            &["go.test/v1"],
        ),
        (
            "elixir",
            &[("mix.exs", "defmodule Demo.MixProject do\nend\n")],
            &["elixir.mix/v1"],
        ),
        (
            "ruby",
            &[
                ("Gemfile", "gem 'rspec'\n"),
                (".rspec", "--format progress\n"),
            ],
            &["ruby.rspec/v1"],
        ),
        (
            "maven",
            &[("pom.xml", "<project></project>\n")],
            &["java.maven/v1"],
        ),
        (
            "gradle",
            &[
                ("build.gradle.kts", "plugins { java }\n"),
                ("gradlew", "#!/bin/sh\n"),
            ],
            &["java.gradle/v1"],
        ),
        (
            "dotnet",
            &[("Demo.sln", "Microsoft Visual Studio Solution File\n")],
            &["dotnet.test/v1"],
        ),
        (
            "swift",
            &[("Package.swift", "// swift-tools-version: 6.0\n")],
            &["swift.spm/v1"],
        ),
        (
            "php",
            &[("composer.json", r#"{"scripts":{"test":"phpunit"}}"#)],
            &["php.composer/v1"],
        ),
        (
            "cmake",
            &[("CMakeLists.txt", "enable_testing()\n")],
            &["cpp.cmake/v1"],
        ),
        (
            "meson",
            &[("meson.build", "project('demo', 'c')\n")],
            &["cpp.meson/v1"],
        ),
        (
            "make",
            &[("Makefile", "check:\n\t@true\n")],
            &["task.make/v1"],
        ),
        (
            "just",
            &[("justfile", "test:\n    true\n")],
            &["task.just/v1"],
        ),
        (
            "taskfile",
            &[(
                "Taskfile.yml",
                "version: '3'\ntasks:\n  lint:\n    cmds: ['true']\n",
            )],
            &["task.taskfile/v1"],
        ),
        (
            "mise",
            &[("mise.toml", "[tasks.verify]\nrun = 'true'\n")],
            &["task.mise/v1"],
        ),
    ];

    for (name, files, expected) in cases {
        let temp = tempfile::tempdir().unwrap();
        write_files(temp.path(), files);
        let plan = gate_scaffold::inspect(temp.path()).unwrap();
        let selected = plan
            .selected_template_ids()
            .into_iter()
            .filter(|id| *id != "baseline.git/v1")
            .collect::<Vec<_>>();
        assert_eq!(selected, *expected, "case {name}: {:#?}", plan.decisions);
        assert_eq!(plan.readiness, Readiness::Ready, "case {name}");
    }
}

#[test]
fn explicit_repository_tasks_supersede_ecosystem_defaults_in_the_same_scope() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            ("Cargo.toml", "[workspace]\nmembers=[]\n"),
            ("go.mod", "module example.test/demo\n"),
            ("Makefile", "check:\n\t@true\n"),
        ],
    );

    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    assert_eq!(
        plan.selected_template_ids(),
        vec!["baseline.git/v1", "task.make/v1"]
    );
    assert!(plan.decisions.iter().any(|decision| {
        decision.template_id.as_deref() == Some("rust.cargo/v1")
            && decision.outcome.as_str() == "superseded"
    }));
    assert!(plan.decisions.iter().any(|decision| {
        decision.template_id.as_deref() == Some("go.test/v1")
            && decision.outcome.as_str() == "superseded"
    }));
}

#[test]
fn incompatible_manager_and_build_evidence_is_actionable_and_blocks() {
    let cases = [
        vec![
            ("package.json", r#"{"scripts":{"test":"true"}}"#),
            ("package-lock.json", "{}"),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'"),
        ],
        vec![
            (
                "package.json",
                r#"{"packageManager":"pnpm@9","scripts":{"test":"true"}}"#,
            ),
            ("package-lock.json", "{}"),
        ],
        vec![
            ("pom.xml", "<project/>"),
            ("build.gradle", "plugins { id 'java' }"),
        ],
        vec![
            ("CMakeLists.txt", "project(x)"),
            ("meson.build", "project('x', 'c')"),
        ],
        vec![
            ("Makefile", "test:\n\t@true\n"),
            ("justfile", "test:\n    true\n"),
        ],
    ];
    for files in cases {
        let temp = tempfile::tempdir().unwrap();
        write_files(temp.path(), &files);
        let plan = gate_scaffold::inspect(temp.path()).unwrap();
        assert!(!plan.ok, "{:#?}", plan.decisions);
        assert_eq!(plan.readiness, Readiness::Blocked);
        assert!(plan.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == nopal_core::diagnostics::Code::GateScaffoldAmbiguous
        }));
        assert!(plan.decisions.iter().any(|decision| {
            decision.outcome.as_str() == "ambiguous" && decision.evidence.len() >= 2
        }));
    }
}

#[test]
fn python_and_ruby_require_structured_tool_evidence() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            (
                "pyproject.toml",
                "[project]\ndescription='mentions [tool.pytest but configures nothing'\n",
            ),
            (".rspec", "--format progress\n"),
        ],
    );
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    assert!(!plan.selected_template_ids().contains(&"python.pytest/v1"));
    assert!(!plan.selected_template_ids().contains(&"ruby.rspec/v1"));
}

#[test]
fn independent_root_ecosystems_compose_in_registry_order() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            ("Package.swift", "// swift-tools-version: 6.0\n"),
            ("go.mod", "module example.test/demo\n"),
            ("Cargo.toml", "[workspace]\nmembers=[]\n"),
        ],
    );
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    assert_eq!(
        plan.selected_template_ids(),
        vec![
            "baseline.git/v1",
            "rust.cargo/v1",
            "go.test/v1",
            "swift.spm/v1",
        ]
    );
}

#[test]
fn only_declared_confined_workspaces_contribute_in_scope_order() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            (
                "package.json",
                r#"{"packageManager":"pnpm@9.0.0","workspaces":["packages/*"]}"#,
            ),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
            (
                "packages/web/package.json",
                r#"{"scripts":{"lint":"eslint ."}}"#,
            ),
            ("packages/api/pyproject.toml", "[tool.pytest.ini_options]\n"),
            ("undeclared/go.mod", "module example.test/ignored\n"),
            ("packages/examples/go.mod", "module example.test/fixture\n"),
        ],
    );

    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    assert_eq!(plan.readiness, Readiness::Ready, "{:#?}", plan.decisions);
    let selected = plan
        .templates
        .iter()
        .map(|template| (template.scope.as_str(), template.id.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(
        selected,
        vec![
            (".", "baseline.git/v1"),
            ("packages/api", "python.pytest/v1"),
            ("packages/web", "javascript.pnpm/v1"),
        ]
    );
    assert!(plan.gates.iter().any(|gate| {
        gate.cwd.as_deref() == Some("packages/api") && gate.argv == ["python", "-m", "pytest"]
    }));
    assert!(!plan.decisions.iter().any(|decision| {
        decision
            .evidence
            .iter()
            .any(|path| path.contains("undeclared") || path.contains("examples"))
    }));
}

#[test]
fn root_workspace_aware_templates_do_not_duplicate_member_gates() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            ("Cargo.toml", "[workspace]\nmembers=['crates/app']\n"),
            (
                "crates/app/Cargo.toml",
                "[package]\nname='app'\nversion='0.1.0'\n",
            ),
        ],
    );
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    assert_eq!(
        plan.templates
            .iter()
            .filter(|template| template.id == "rust.cargo/v1")
            .count(),
        1
    );
    assert!(plan.decisions.iter().any(|decision| {
        decision.scope == "crates/app"
            && decision.template_id.as_deref() == Some("rust.cargo/v1")
            && decision.reason_code == "root_workspace_coverage"
    }));
}

#[test]
fn root_templates_do_not_claim_workspaces_declared_by_another_ecosystem() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            ("Cargo.toml", "[package]\nname='root'\nversion='0.1.0'\n"),
            ("package.json", r#"{"workspaces":["packages/app"]}"#),
            (
                "packages/app/Cargo.toml",
                "[package]\nname='app'\nversion='0.1.0'\n",
            ),
        ],
    );
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    let rust_scopes = plan
        .templates
        .iter()
        .filter(|template| template.id == "rust.cargo/v1")
        .map(|template| template.scope.as_str())
        .collect::<Vec<_>>();
    assert_eq!(rust_scopes, vec![".", "packages/app"]);
}

#[test]
fn workspace_declarations_cover_cargo_pnpm_go_maven_gradle_and_dotnet_forms() {
    let cases: &[(&[(&str, &str)], &str)] = &[
        (
            &[
                ("Cargo.toml", "[workspace]\nmembers=['members/app']\n"),
                ("members/app/go.mod", "module example.test/app\n"),
            ],
            "members/app",
        ),
        (
            &[
                ("pnpm-workspace.yaml", "packages:\n  - 'members/app'\n"),
                ("members/app/go.mod", "module example.test/app\n"),
            ],
            "members/app",
        ),
        (
            &[
                ("go.work", "go 1.24\nuse ./members/app\n"),
                (
                    "members/app/Cargo.toml",
                    "[package]\nname='app'\nversion='0.1.0'\n",
                ),
            ],
            "members/app",
        ),
        (
            &[
                (
                    "pom.xml",
                    "<project><modules><module>members/app</module></modules></project>",
                ),
                ("members/app/go.mod", "module example.test/app\n"),
            ],
            "members/app",
        ),
        (
            &[
                ("settings.gradle", "include(':members:app')\n"),
                ("members/app/go.mod", "module example.test/app\n"),
            ],
            "members/app",
        ),
        (
            &[
                (
                    "Demo.sln",
                    "Project(\"x\") = \"App\", \"members/app/App.csproj\", \"x\"\n",
                ),
                ("members/app/App.csproj", "<Project/>\n"),
                ("members/app/go.mod", "module example.test/app\n"),
            ],
            "members/app",
        ),
    ];
    for (files, scope) in cases {
        let temp = tempfile::tempdir().unwrap();
        write_files(temp.path(), files);
        let plan = gate_scaffold::inspect(temp.path()).unwrap();
        assert!(
            plan.templates
                .iter()
                .any(|template| template.scope == *scope),
            "{:#?}",
            plan.decisions
        );
    }
}

#[test]
fn pnpm_workspace_lock_proves_child_package_manager_without_root_package_json() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            ("pnpm-workspace.yaml", "packages:\n  - 'packages/*'\n"),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
            (
                "packages/web/package.json",
                r#"{"scripts":{"test":"node test.js"}}"#,
            ),
        ],
    );
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    assert!(plan.ok, "{:?}", plan.diagnostics);
    assert!(plan.templates.iter().any(|template| {
        template.scope == "packages/web" && template.id == "javascript.pnpm/v1"
    }));
}

#[test]
fn workspace_expansion_has_a_total_result_bound() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[("package.json", r#"{"workspaces":["packages/*"]}"#)],
    );
    for index in 0..257 {
        fs::create_dir_all(temp.path().join(format!("packages/member-{index:03}"))).unwrap();
    }
    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    assert!(!plan.ok);
    assert!(plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::GateWorkspaceInvalid
            && diagnostic.message.contains("256-workspace")
    }));
}

#[test]
fn workspace_normalization_cannot_hide_parent_traversal() {
    for declaration in [" ../outside ", "'../outside'"] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("root");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&root).unwrap();
        write_files(&outside, &[("Makefile", "test:\n\t@true\n")]);
        write_files(
            &root,
            &[(
                "package.json",
                &format!(r#"{{"workspaces":["{declaration}"]}}"#),
            )],
        );
        let plan = gate_scaffold::inspect(&root).unwrap();
        assert!(!plan.ok, "{declaration}: {:#?}", plan.decisions);
        assert!(plan.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == nopal_core::diagnostics::Code::GateWorkspaceInvalid
        }));
        assert!(
            !plan
                .templates
                .iter()
                .any(|template| template.scope.contains(".."))
        );
    }
}

#[test]
#[cfg(unix)]
fn workspace_traversal_and_symlink_boundaries_fail_closed() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write_files(
        outside.path(),
        &[("go.mod", "module example.test/outside\n")],
    );
    write_files(
        temp.path(),
        &[("package.json", r#"{"workspaces":["../outside","linked"]}"#)],
    );
    symlink(outside.path(), temp.path().join("linked")).unwrap();

    let plan = gate_scaffold::inspect(temp.path()).unwrap();
    assert!(!plan.ok);
    assert_eq!(plan.readiness, Readiness::Blocked);
    assert!(plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::GateWorkspaceInvalid
    }));

    let evidence_root = temp.path().join("evidence-root");
    fs::create_dir_all(&evidence_root).unwrap();
    let outside_manifest = outside.path().join("package.json");
    fs::write(&outside_manifest, r#"{"scripts":{"test":"true"}}"#).unwrap();
    symlink(&outside_manifest, evidence_root.join("package.json")).unwrap();
    let evidence_plan = gate_scaffold::inspect(&evidence_root).unwrap();
    assert!(!evidence_plan.ok);
    assert!(evidence_plan.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::GateScaffoldEvidenceInvalid
    }));

    let authority_root = temp.path().join("authority-root");
    fs::create_dir_all(authority_root.join(".nopal")).unwrap();
    write_files(
        &authority_root,
        &[("Cargo.toml", "[workspace]\nmembers=[]\n")],
    );
    let outside_gates = outside.path().join("gates.jsonc");
    fs::write(
        &outside_gates,
        r#"{"version":"nopal.gates/v1","gates":[{"id":"outside","stage":"pre_pr","argv":["true"]}]}"#,
    )
    .unwrap();
    symlink(&outside_gates, authority_root.join(".nopal/gates.jsonc")).unwrap();
    let authority_plan = gate_scaffold::inspect_with_checked_in_authority(&authority_root).unwrap();
    assert!(!authority_plan.ok);
    assert_eq!(authority_plan.authority.as_str(), "generated");
}

#[test]
fn checked_in_nopal_and_typed_beislid_gates_supersede_generated_defaults() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            ("Cargo.toml", "[workspace]\nmembers=[]\n"),
            (
                ".nopal/gates.jsonc",
                r#"{"version":"nopal.gates/v1","gates":[{"id":"explicit-nopal","stage":"pre_pr","argv":["true"]}]}"#,
            ),
            (
                ".beislid/workflow.md",
                "```beislid:gates\n- name: explicit-beislid\n  command: 'cargo test'\n```\n",
            ),
        ],
    );

    let plan = gate_scaffold::inspect_with_checked_in_authority(temp.path()).unwrap();
    assert!(plan.ok, "{:?}", plan.diagnostics);
    assert_eq!(plan.authority.as_str(), "explicit_nopal_and_beislid");
    assert_eq!(plan.readiness, Readiness::Ready);
    assert_eq!(plan.selected_template_ids(), vec!["baseline.git/v1"]);
    assert!(plan.decisions.iter().any(|decision| {
        decision.template_id.as_deref() == Some("rust.cargo/v1")
            && decision.outcome.as_str() == "superseded"
            && decision.reason_code == "explicit_gate_precedence"
    }));
}

#[test]
fn doctor_does_not_treat_selector_scoped_gates_as_repository_wide_readiness() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[(
            ".nopal/gates.jsonc",
            r#"{
  "version": "nopal.gates/v1",
  "gates": [{"id":"explicit-rust","stage":"pre_pr","argv":["cargo","test"]}],
  "gate_sets": {"rust":{"gates":["explicit-rust"]}},
  "selectors": [{"name":"rust","paths":["**/*.rs"],"gate_sets":["rust"]}]
}"#,
        )],
    );

    let plan = gate_scaffold::inspect_with_checked_in_authority(temp.path()).unwrap();

    assert_eq!(plan.readiness, Readiness::NeedsConfiguration);
    assert_eq!(plan.authority.as_str(), "generated");
}

#[test]
fn doctor_reports_drift_between_checked_in_generation_and_current_evidence() {
    let temp = tempfile::tempdir().unwrap();
    write_files(temp.path(), &[("Cargo.toml", "[workspace]\nmembers=[]\n")]);
    let generated = gate_scaffold::inspect(temp.path())
        .unwrap()
        .gates_json()
        .unwrap();
    write_files(temp.path(), &[(".nopal/gates.jsonc", &generated)]);
    fs::remove_file(temp.path().join("Cargo.toml")).unwrap();
    write_files(temp.path(), &[("go.mod", "module example.test/demo\n")]);

    let plan = gate_scaffold::inspect_with_checked_in_authority(temp.path()).unwrap();
    assert!(!plan.ok);
    assert!(
        plan.diagnostics.iter().any(|diagnostic| {
            diagnostic.code == nopal_core::diagnostics::Code::GateScaffoldDrift
        })
    );
}

#[test]
fn malformed_explicit_authority_blocks_doctor_instead_of_falling_back() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            ("Cargo.toml", "[workspace]\nmembers=[]\n"),
            (".nopal/gates.jsonc", "not json\n"),
        ],
    );
    let plan = gate_scaffold::inspect_with_checked_in_authority(temp.path()).unwrap();
    assert!(!plan.ok);
    assert_eq!(plan.readiness, Readiness::Blocked);
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

#[test]
fn composer_description_text_does_not_prove_phpunit() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[(
            "composer.json",
            r#"{"name":"demo/demo","description":"Documentation mentions phpunit but does not configure it","scripts":{"test":"echo phpunit is not configured"}}"#,
        )],
    );

    let plan = gate_scaffold::inspect(temp.path()).unwrap();

    assert_eq!(plan.readiness, Readiness::NeedsConfiguration);
    assert!(!plan.selected_template_ids().contains(&"php.composer/v1"));
}

#[test]
fn composer_script_alias_does_not_prove_the_aliased_command_runs_phpunit() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[(
            "composer.json",
            r#"{"name":"demo/demo","scripts":{"test":"@fake/phpunit","fake/phpunit":"echo no tests"}}"#,
        )],
    );

    let plan = gate_scaffold::inspect(temp.path()).unwrap();

    assert_eq!(plan.readiness, Readiness::NeedsConfiguration);
    assert!(!plan.selected_template_ids().contains(&"php.composer/v1"));
}

#[test]
fn go_and_maven_comments_do_not_declare_workspaces() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            (
                "go.work",
                "go 1.22\nus/* disabled */e ./split-comment-go\nuse (\n  // ./commented-go\n  /* ./also-commented-go */\n  ./live-go\n)\n",
            ),
            (
                "pom.xml",
                "<project><modules><!-- <module>commented-maven</module> --><mod<!-- disabled -->ule>split-comment-maven</module><module>live-maven</module></modules></project>",
            ),
            (
                "split-comment-go/go.mod",
                "module example.test/split-comment-go\n",
            ),
            (
                "split-comment-maven/go.mod",
                "module example.test/split-comment-maven\n",
            ),
            ("live-go/go.mod", "module example.test/live-go\n"),
            ("live-maven/go.mod", "module example.test/live-maven\n"),
        ],
    );

    let plan = gate_scaffold::inspect(temp.path()).unwrap();

    assert!(plan.ok, "{:?}", plan.diagnostics);
    let scopes = plan
        .templates
        .iter()
        .map(|template| template.scope.as_str())
        .collect::<Vec<_>>();
    assert!(scopes.contains(&"live-go"));
    assert!(scopes.contains(&"live-maven"));
    assert!(!scopes.contains(&"commented-go"));
    assert!(!scopes.contains(&"also-commented-go"));
    assert!(!scopes.contains(&"commented-maven"));
    assert!(!scopes.contains(&"split-comment-go"));
    assert!(!scopes.contains(&"split-comment-maven"));
}

#[test]
fn commented_cpp_subdirectory_calls_do_not_declare_workspaces() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            (
                "CMakeLists.txt",
                "enable_testing()\n# add_subdirectory(line-comment)\n#[[\nadd_subdirectory(archive)\n]]\n",
            ),
            ("line-comment/go.mod", "module example.test/line-comment\n"),
            ("archive/go.mod", "module example.test/archive\n"),
        ],
    );

    let plan = gate_scaffold::inspect(temp.path()).unwrap();

    assert!(plan.ok, "{:?}", plan.diagnostics);
    assert!(
        !plan
            .templates
            .iter()
            .any(|template| { matches!(template.scope.as_str(), "archive" | "line-comment") })
    );
}

#[test]
fn cmake_bracket_strings_are_ignored_and_commands_are_case_insensitive() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            (
                "CMakeLists.txt",
                "enable_testing()\nset(DOCUMENTATION [=[\nadd_subdirectory(archive)\n]=])\nADD_SUBDIRECTORY(live)\n",
            ),
            ("archive/go.mod", "module example.test/archive\n"),
            ("live/go.mod", "module example.test/live\n"),
        ],
    );

    let plan = gate_scaffold::inspect(temp.path()).unwrap();

    assert!(plan.ok, "{:?}", plan.diagnostics);
    assert!(
        !plan
            .templates
            .iter()
            .any(|template| template.scope == "archive")
    );
    assert!(
        plan.templates
            .iter()
            .any(|template| template.scope == "live")
    );
}

#[test]
fn multiline_meson_strings_do_not_declare_workspaces() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            (
                "meson.build",
                "project('demo', 'c')\nmessage = '''\nsubdir('archive')\n'''\n",
            ),
            ("archive/go.mod", "module example.test/archive\n"),
        ],
    );

    let plan = gate_scaffold::inspect(temp.path()).unwrap();

    assert!(plan.ok, "{:?}", plan.diagnostics);
    assert!(
        !plan
            .templates
            .iter()
            .any(|template| template.scope == "archive")
    );
}

#[test]
fn pnpm_negations_do_not_exclude_workspaces_declared_by_cargo() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            ("Cargo.toml", "[workspace]\nmembers = ['packages/*']\n"),
            (
                "pnpm-workspace.yaml",
                "packages:\n  - 'packages/*'\n  - '!packages/backend'\n",
            ),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
            ("packages/backend/go.mod", "module example.test/backend\n"),
        ],
    );

    let plan = gate_scaffold::inspect(temp.path()).unwrap();

    assert!(plan.ok, "{:?}", plan.diagnostics);
    assert!(
        plan.templates
            .iter()
            .any(|template| template.scope == "packages/backend")
    );
}

#[test]
fn pnpm_negated_workspace_patterns_exclude_matching_members() {
    let temp = tempfile::tempdir().unwrap();
    write_files(
        temp.path(),
        &[
            (
                "pnpm-workspace.yaml",
                "packages:\n  - 'packages/*'\n  - '!packages/legacy' # intentionally excluded\n",
            ),
            ("pnpm-lock.yaml", "lockfileVersion: '9.0'\n"),
            ("packages/app/go.mod", "module example.test/app\n"),
            ("packages/legacy/go.mod", "module example.test/legacy\n"),
        ],
    );

    let plan = gate_scaffold::inspect(temp.path()).unwrap();

    assert!(plan.ok, "{:?}", plan.diagnostics);
    assert!(
        plan.templates
            .iter()
            .any(|template| template.scope == "packages/app")
    );
    assert!(
        !plan
            .templates
            .iter()
            .any(|template| template.scope == "packages/legacy")
    );
}
