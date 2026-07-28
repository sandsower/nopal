//! Evidence-backed first-run gate planning.
//!
//! One immutable plan owns detection, generated gate bytes, provenance, and
//! explanations. Callers may render or persist the plan, but Core never runs
//! detected tools and never searches outside the repository and its declared
//! workspaces.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::diagnostics::{Code, Diagnostic, Severity};

pub const PLAN_KIND: &str = "nopal.gate-scaffold/v1";
pub const GENERATED_GATES_KIND: &str = "nopal.gates/v2";

pub const TEMPLATE_IDS: [&str; 23] = [
    "baseline.git/v1",
    "task.make/v1",
    "task.just/v1",
    "task.taskfile/v1",
    "task.mise/v1",
    "javascript.npm/v1",
    "javascript.pnpm/v1",
    "javascript.yarn/v1",
    "javascript.bun/v1",
    "rust.cargo/v1",
    "python.pytest/v1",
    "python.ruff/v1",
    "python.mypy/v1",
    "go.test/v1",
    "elixir.mix/v1",
    "ruby.rspec/v1",
    "java.maven/v1",
    "java.gradle/v1",
    "dotnet.test/v1",
    "swift.spm/v1",
    "php.composer/v1",
    // C and C++ share these build-system templates.
    "cpp.cmake/v1",
    "cpp.meson/v1",
];

pub fn known_template_id(id: &str) -> bool {
    TEMPLATE_IDS.contains(&id)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Readiness {
    Ready,
    NeedsConfiguration,
    Blocked,
}

impl Readiness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::NeedsConfiguration => "needs_configuration",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Generated,
    ExplicitNopal,
    ExplicitBeislid,
    ExplicitNopalAndBeislid,
}

impl Authority {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Generated => "generated",
            Self::ExplicitNopal => "explicit_nopal",
            Self::ExplicitBeislid => "explicit_beislid",
            Self::ExplicitNopalAndBeislid => "explicit_nopal_and_beislid",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionOutcome {
    Selected,
    Skipped,
    Superseded,
    Ambiguous,
    Blocked,
}

impl DecisionOutcome {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::Skipped => "skipped",
            Self::Superseded => "superseded",
            Self::Ambiguous => "ambiguous",
            Self::Blocked => "blocked",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectionDecision {
    pub scope: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub template_id: Option<String>,
    pub outcome: DecisionOutcome,
    pub reason_code: String,
    pub evidence: Vec<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TemplateSelection {
    pub id: String,
    pub scope: String,
    pub evidence: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedGate {
    pub id: String,
    pub stage: String,
    pub argv: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    pub parallel_safe: bool,
    pub mutates: bool,
    pub template_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScaffoldProvenance {
    pub version: String,
    pub readiness: Readiness,
    pub authority: Authority,
    pub templates: Vec<TemplateSelection>,
    pub generated_gate_ids: Vec<String>,
    pub decisions: Vec<DetectionDecision>,
}

#[derive(Debug, Clone, Serialize)]
pub struct GateScaffoldPlan {
    pub kind: &'static str,
    pub ok: bool,
    pub readiness: Readiness,
    pub authority: Authority,
    pub templates: Vec<TemplateSelection>,
    pub gates: Vec<GeneratedGate>,
    pub decisions: Vec<DetectionDecision>,
    pub diagnostics: Vec<Diagnostic>,
}

impl GateScaffoldPlan {
    pub fn selected_template_ids(&self) -> Vec<&str> {
        self.templates
            .iter()
            .map(|template| template.id.as_str())
            .collect()
    }

    /// Render the exact checked-in gate document without another repository
    /// read. This prevents preview and publication from observing different
    /// ecosystem evidence.
    pub fn gates_json(&self) -> Result<String, serde_json::Error> {
        let gates = self
            .gates
            .iter()
            .map(|gate| {
                let mut value = serde_json::json!({
                    "id": gate.id,
                    "stage": gate.stage,
                    "argv": gate.argv,
                    "parallel_safe": gate.parallel_safe,
                    "mutates": gate.mutates,
                });
                if let Some(cwd) = &gate.cwd {
                    value["cwd"] = serde_json::Value::String(cwd.clone());
                }
                value
            })
            .collect::<Vec<_>>();
        let value = serde_json::json!({
            "version": GENERATED_GATES_KIND,
            "scaffold": {
                "version": PLAN_KIND,
                "readiness": self.readiness.as_str(),
                "authority": self.authority.as_str(),
                "templates": self.templates,
                "generated_gate_ids": self.gates.iter().map(|gate| gate.id.as_str()).collect::<Vec<_>>(),
                "decisions": self.decisions,
            },
            "preflights": [],
            "gates": gates,
            "gate_sets": {},
            "selectors": [],
        });
        serde_json::to_string_pretty(&value).map(|mut text| {
            text.push('\n');
            text
        })
    }
}

/// Inspect root evidence and only those nested scopes named by root manifests.
/// Pattern expansion follows the declaration itself; there is no repository-wide
/// ecosystem search.
pub fn inspect(root: &Path) -> io::Result<GateScaffoldPlan> {
    let root = std::path::absolute(root)?;
    let inherited_manager = root_package_manager_hint(&root);
    let mut plan = inspect_scope(&root, None)?;
    let workspaces = discover_workspaces(&root, &mut plan)?;
    let mut unknown_workspace = false;
    for workspace in workspaces {
        let child = inspect_scope(&root.join(&workspace.path), inherited_manager.as_ref())?;
        unknown_workspace |= child.readiness == Readiness::NeedsConfiguration;
        merge_workspace_plan(
            &mut plan,
            child,
            &workspace.path,
            &workspace.root_covering_templates,
        );
    }
    if plan.readiness != Readiness::Blocked {
        plan.decisions.retain(|decision| {
            !(decision.scope == "."
                && decision.reason_code == "no_validation_evidence"
                && plan.templates.len() > 1)
        });
        plan.readiness = if unknown_workspace || plan.templates.len() == 1 {
            Readiness::NeedsConfiguration
        } else {
            Readiness::Ready
        };
        plan.ok = true;
    }
    crate::diagnostics::sort(&mut plan.diagnostics);
    Ok(plan)
}

/// Inspect detection together with existing explicit gate authority. This is
/// used by doctor only; it never rewrites the checked-in document.
pub fn inspect_with_checked_in_authority(root: &Path) -> io::Result<GateScaffoldPlan> {
    let mut plan = inspect(root)?;
    let mut nopal_explicit = Vec::new();
    let mut beislid_explicit = Vec::new();
    let mut generated_checked_text = None;
    let mut authority_diagnostics = Vec::new();

    let gates_path = root.join(".nopal/gates.jsonc");
    if gates_path.exists() {
        match fs::read_to_string(&gates_path) {
            Ok(text) => {
                let (config, diagnostics) = crate::gates::parse_gates(&text, ".nopal/gates.jsonc");
                authority_diagnostics.extend(diagnostics);
                if let Some(config) = config {
                    let generated = config
                        .scaffold
                        .as_ref()
                        .map(|provenance| {
                            provenance
                                .generated_gate_ids
                                .iter()
                                .map(String::as_str)
                                .collect::<std::collections::BTreeSet<_>>()
                        })
                        .unwrap_or_default();
                    nopal_explicit.extend(
                        config
                            .gates
                            .iter()
                            .filter(|gate| {
                                config.scaffold.is_none() || !generated.contains(gate.id.as_str())
                            })
                            .map(|gate| gate.id.clone()),
                    );
                    nopal_explicit.extend(config.preflights.iter().map(|gate| gate.id.clone()));
                    if config.scaffold.is_some() && nopal_explicit.is_empty() {
                        generated_checked_text = Some(text.clone());
                    }
                }
            }
            Err(error) => authority_diagnostics.push(Diagnostic::error(
                Code::GateScaffoldEvidenceInvalid,
                ".nopal/gates.jsonc",
                format!("could not read explicit Nopal gates: {error}"),
            )),
        }
    }

    let workflow_path = root.join(".beislid/workflow.md");
    if workflow_path.exists() {
        match fs::read_to_string(&workflow_path) {
            Ok(text) => {
                let compiled = crate::beislid_import::compile_text(&text, ".beislid/workflow.md");
                authority_diagnostics.extend(compiled.diagnostics);
                if let Some(value) = compiled.modules.get("gates") {
                    for field in ["gates", "preflights"] {
                        beislid_explicit.extend(
                            value
                                .get(field)
                                .and_then(serde_json::Value::as_array)
                                .into_iter()
                                .flatten()
                                .filter_map(|entry| entry.get("id").or_else(|| entry.get("name")))
                                .filter_map(serde_json::Value::as_str)
                                .map(ToOwned::to_owned),
                        );
                    }
                }
            }
            Err(error) => authority_diagnostics.push(Diagnostic::error(
                Code::GateScaffoldEvidenceInvalid,
                ".beislid/workflow.md",
                format!("could not read typed Beislið gate authority: {error}"),
            )),
        }
    }

    if !nopal_explicit.is_empty() || !beislid_explicit.is_empty() {
        plan.authority = match (nopal_explicit.is_empty(), beislid_explicit.is_empty()) {
            (false, false) => Authority::ExplicitNopalAndBeislid,
            (false, true) => Authority::ExplicitNopal,
            (true, false) => Authority::ExplicitBeislid,
            (true, true) => Authority::Generated,
        };
        for decision in &mut plan.decisions {
            if decision.template_id.as_deref() != Some("baseline.git/v1") {
                decision.outcome = DecisionOutcome::Superseded;
                decision.reason_code = "explicit_gate_precedence".to_owned();
                decision.message =
                    "checked-in explicit gates supersede generated templates".to_owned();
            }
        }
        plan.templates
            .retain(|template| template.id == "baseline.git/v1");
        plan.gates
            .retain(|gate| gate.template_id == "baseline.git/v1");
        plan.diagnostics.retain(|diagnostic| {
            !matches!(
                diagnostic.code,
                Code::GateScaffoldEvidenceInvalid
                    | Code::GateScaffoldAmbiguous
                    | Code::GateConfigurationRequired
                    | Code::GateWorkspaceInvalid
            )
        });
        let mut evidence = nopal_explicit.clone();
        evidence.extend(beislid_explicit.clone());
        evidence.sort();
        plan.decisions.push(DetectionDecision {
            scope: ".".to_owned(),
            template_id: None,
            outcome: DecisionOutcome::Selected,
            reason_code: "explicit_gate_authority".to_owned(),
            evidence,
            message: "checked-in explicit gates are authoritative over generated defaults"
                .to_owned(),
        });
        plan.readiness = Readiness::Ready;
        plan.ok = true;
    }
    if nopal_explicit.is_empty()
        && beislid_explicit.is_empty()
        && let Some(checked_text) = generated_checked_text
    {
        let expected = plan
            .gates_json()
            .ok()
            .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok());
        let actual =
            crate::config::parse_jsonc(&checked_text, ".nopal/gates.jsonc", Code::ModuleParseError)
                .ok();
        if expected != actual {
            authority_diagnostics.push(Diagnostic::error(
                Code::GateScaffoldDrift,
                ".nopal/gates.jsonc",
                "checked-in generated gates do not match current detection evidence",
            ));
        }
    }
    plan.diagnostics.extend(authority_diagnostics);
    crate::diagnostics::sort(&mut plan.diagnostics);
    if plan
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        plan.ok = false;
        plan.readiness = Readiness::Blocked;
    }
    Ok(plan)
}

fn inspect_scope(
    root: &Path,
    inherited_manager: Option<&PackageManagerChoice>,
) -> io::Result<GateScaffoldPlan> {
    let mut plan = GateScaffoldPlan {
        kind: PLAN_KIND,
        ok: true,
        readiness: Readiness::NeedsConfiguration,
        authority: Authority::Generated,
        templates: Vec::new(),
        gates: Vec::new(),
        decisions: Vec::new(),
        diagnostics: Vec::new(),
    };
    select_baseline(&mut plan);

    let suppression = detect_explicit_tasks(root, &mut plan, inherited_manager)?;
    if plan.readiness != Readiness::Blocked {
        detect_ecosystem_defaults(root, &mut plan, suppression)?;
    }

    if plan.readiness != Readiness::Blocked && plan.templates.len() > 1 {
        plan.readiness = Readiness::Ready;
    } else if plan.readiness != Readiness::Blocked {
        plan.decisions.push(DetectionDecision {
            scope: ".".to_owned(),
            template_id: None,
            outcome: DecisionOutcome::Blocked,
            reason_code: "no_validation_evidence".to_owned(),
            evidence: Vec::new(),
            message: "no evidence-backed validation template was selected; add explicit gates before launch".to_owned(),
        });
    }
    plan.ok = plan.readiness != Readiness::Blocked
        && plan
            .diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error);
    crate::diagnostics::sort(&mut plan.diagnostics);
    Ok(plan)
}

fn select_baseline(plan: &mut GateScaffoldPlan) {
    plan.templates.push(TemplateSelection {
        id: "baseline.git/v1".to_owned(),
        scope: ".".to_owned(),
        evidence: vec![".git".to_owned()],
    });
    plan.gates.push(GeneratedGate {
        id: "detected.root.baseline.diff-check".to_owned(),
        stage: "pre_pr".to_owned(),
        argv: vec!["git".to_owned(), "diff".to_owned(), "--check".to_owned()],
        cwd: None,
        parallel_safe: true,
        mutates: false,
        template_id: "baseline.git/v1".to_owned(),
    });
    plan.decisions.push(DetectionDecision {
        scope: ".".to_owned(),
        template_id: Some("baseline.git/v1".to_owned()),
        outcome: DecisionOutcome::Selected,
        reason_code: "git_baseline".to_owned(),
        evidence: vec![".git".to_owned()],
        message: "selected the repository diff integrity baseline".to_owned(),
    });
}

const ROOT_WORKSPACE_COVERING_TEMPLATES: [&str; 7] = [
    "rust.cargo/v1",
    "elixir.mix/v1",
    "java.maven/v1",
    "java.gradle/v1",
    "dotnet.test/v1",
    "cpp.cmake/v1",
    "cpp.meson/v1",
];

fn merge_workspace_plan(
    plan: &mut GateScaffoldPlan,
    mut child: GateScaffoldPlan,
    scope: &str,
    declared_root_coverage: &std::collections::BTreeSet<&'static str>,
) {
    let scope_id = scope_gate_id(scope);
    let root_templates = plan
        .templates
        .iter()
        .filter(|template| template.scope == ".")
        .map(|template| template.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    let covered_by_root = ROOT_WORKSPACE_COVERING_TEMPLATES
        .iter()
        .filter(|template| {
            root_templates.contains(**template) && declared_root_coverage.contains(**template)
        })
        .copied()
        .collect::<std::collections::BTreeSet<_>>();
    for decision in &mut child.decisions {
        if decision
            .template_id
            .as_deref()
            .is_some_and(|template| covered_by_root.contains(template))
        {
            decision.outcome = DecisionOutcome::Superseded;
            decision.reason_code = "root_workspace_coverage".to_owned();
            decision.message =
                "the selected root template already validates its declared workspaces".to_owned();
        }
    }
    child.templates.retain(|template| {
        template.id != "baseline.git/v1" && !covered_by_root.contains(template.id.as_str())
    });
    child.gates.retain(|gate| {
        gate.template_id != "baseline.git/v1"
            && !covered_by_root.contains(gate.template_id.as_str())
    });
    let generated_ids = child
        .gates
        .iter()
        .map(|gate| gate.id.clone())
        .collect::<Vec<_>>();
    let covered_workspace = child
        .decisions
        .iter()
        .any(|decision| decision.reason_code == "root_workspace_coverage");
    child
        .decisions
        .retain(|decision| decision.template_id.as_deref() != Some("baseline.git/v1"));
    for template in &mut child.templates {
        template.scope = scope.to_owned();
        prefix_evidence(&mut template.evidence, scope);
    }
    for gate in &mut child.gates {
        gate.id = gate.id.replacen(
            "detected.root",
            &format!("detected.workspace.{scope_id}"),
            1,
        );
        gate.cwd = Some(scope.to_owned());
    }
    for decision in &mut child.decisions {
        decision.scope = scope.to_owned();
        prefix_evidence(&mut decision.evidence, scope);
    }
    for diagnostic in &mut child.diagnostics {
        if diagnostic.path != "." {
            diagnostic.path = format!("{scope}/{}", diagnostic.path);
        } else {
            diagnostic.path = scope.to_owned();
        }
    }
    if child.readiness == Readiness::Blocked {
        plan.readiness = Readiness::Blocked;
        plan.ok = false;
    }
    plan.templates.extend(child.templates);
    plan.gates.extend(child.gates);
    plan.decisions.extend(child.decisions);
    plan.diagnostics.extend(child.diagnostics);

    if generated_ids.is_empty() && !covered_workspace {
        plan.decisions.push(DetectionDecision {
            scope: scope.to_owned(),
            template_id: None,
            outcome: DecisionOutcome::Blocked,
            reason_code: "workspace_validation_unproven".to_owned(),
            evidence: vec![scope.to_owned()],
            message: "declared workspace has no evidence-backed validation template".to_owned(),
        });
    }
}

fn prefix_evidence(evidence: &mut [String], scope: &str) {
    for path in evidence {
        if let Some(root_path) = path.strip_prefix("@root/") {
            *path = root_path.to_owned();
        } else if path != ".git" {
            *path = format!("{scope}/{path}");
        }
    }
}

fn scope_gate_id(scope: &str) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(scope.as_bytes());
    format!("{}-{}", slug(scope), hex_prefix(&digest, 8))
}

fn hex_prefix(bytes: &[u8], count: usize) -> String {
    bytes
        .iter()
        .take(count)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn root_package_manager_hint(root: &Path) -> Option<PackageManagerChoice> {
    let text = fs::read_to_string(root.join("package.json")).ok()?;
    let package: serde_json::Value = serde_json::from_str(&text).ok()?;
    let candidates = [
        ("javascript.npm/v1", "npm", "package-lock.json"),
        ("javascript.pnpm/v1", "pnpm", "pnpm-lock.yaml"),
        ("javascript.yarn/v1", "yarn", "yarn.lock"),
        ("javascript.bun/v1", "bun", "bun.lock"),
        ("javascript.bun/v1", "bun", "bun.lockb"),
    ];
    let mut found = candidates
        .iter()
        .filter(|candidate| root.join(candidate.2).is_file())
        .collect::<Vec<_>>();
    found.dedup_by_key(|candidate| candidate.0);
    if found.len() == 1 {
        let choice = found[0];
        return Some(PackageManagerChoice {
            template_id: choice.0,
            program: choice.1,
            evidence: format!("@root/{}", choice.2),
        });
    }
    package
        .get("packageManager")
        .and_then(serde_json::Value::as_str)
        .and_then(|manager| {
            candidates
                .iter()
                .find(|candidate| manager.starts_with(candidate.1))
                .map(|candidate| PackageManagerChoice {
                    template_id: candidate.0,
                    program: candidate.1,
                    evidence: "@root/package.json".to_owned(),
                })
        })
}

struct DeclaredWorkspace {
    path: String,
    root_covering_templates: std::collections::BTreeSet<&'static str>,
}

fn discover_workspaces(
    root: &Path,
    plan: &mut GateScaffoldPlan,
) -> io::Result<Vec<DeclaredWorkspace>> {
    let mut patterns = std::collections::BTreeSet::new();
    let mut coverage_patterns = std::collections::BTreeMap::new();
    let mut excludes = std::collections::BTreeSet::new();
    collect_toml_workspace_patterns(
        root,
        plan,
        &mut patterns,
        &mut coverage_patterns,
        &mut excludes,
    )?;
    collect_json_workspace_patterns(root, plan, &mut patterns)?;
    collect_line_workspace_patterns(root, plan, &mut patterns, &mut coverage_patterns)?;

    let mut excluded_workspaces = std::collections::BTreeSet::new();
    for pattern in excludes {
        if invalid_workspace_pattern(&pattern) {
            block_workspace(
                plan,
                &pattern,
                "workspace exclusions must be relative and cannot contain parent traversal",
            );
            continue;
        }
        excluded_workspaces.extend(expand_workspace_pattern(root, &pattern, plan)?);
    }

    let mut workspaces: std::collections::BTreeMap<
        String,
        std::collections::BTreeSet<&'static str>,
    > = std::collections::BTreeMap::new();
    for pattern in patterns {
        if invalid_workspace_pattern(&pattern) {
            block_workspace(
                plan,
                &pattern,
                "workspace declarations must be relative and cannot contain parent traversal",
            );
            continue;
        }
        let expanded = expand_workspace_pattern(root, &pattern, plan)?;
        if expanded.is_empty() && !pattern.contains(['*', '?', '[']) {
            block_workspace(
                plan,
                &pattern,
                "declared workspace does not resolve to a confined directory",
            );
        }
        for workspace in expanded {
            if !excluded_workspaces.contains(&workspace) {
                workspaces.entry(workspace).or_default().extend(
                    coverage_patterns
                        .get(&pattern)
                        .into_iter()
                        .flatten()
                        .copied(),
                );
            }
        }
    }
    Ok(workspaces
        .into_iter()
        .map(|(path, root_covering_templates)| DeclaredWorkspace {
            path,
            root_covering_templates,
        })
        .collect())
}

fn collect_toml_workspace_patterns(
    root: &Path,
    plan: &mut GateScaffoldPlan,
    patterns: &mut std::collections::BTreeSet<String>,
    coverage_patterns: &mut std::collections::BTreeMap<
        String,
        std::collections::BTreeSet<&'static str>,
    >,
    excludes: &mut std::collections::BTreeSet<String>,
) -> io::Result<()> {
    for path in ["Cargo.toml", "pyproject.toml"] {
        let Some(text) = read_optional_evidence(root, path, plan)? else {
            continue;
        };
        let value: toml::Value = match toml::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                block_invalid_evidence(plan, path, format!("invalid TOML evidence: {error}"));
                continue;
            }
        };
        if path == "Cargo.toml" {
            for member in toml_array_strings(&value, &["workspace", "members"]) {
                patterns.insert(member.clone());
                coverage_patterns
                    .entry(member)
                    .or_default()
                    .insert("rust.cargo/v1");
            }
            excludes.extend(toml_array_strings(&value, &["workspace", "exclude"]));
        } else {
            patterns.extend(toml_array_strings(
                &value,
                &["tool", "uv", "workspace", "members"],
            ));
            excludes.extend(toml_array_strings(
                &value,
                &["tool", "uv", "workspace", "exclude"],
            ));
        }
    }
    Ok(())
}

fn toml_array_strings(value: &toml::Value, keys: &[&str]) -> Vec<String> {
    let mut current = value;
    for key in keys {
        let Some(next) = current.get(*key) else {
            return Vec::new();
        };
        current = next;
    }
    current
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(toml::Value::as_str)
        .map(ToOwned::to_owned)
        .collect()
}

fn collect_json_workspace_patterns(
    root: &Path,
    plan: &mut GateScaffoldPlan,
    patterns: &mut std::collections::BTreeSet<String>,
) -> io::Result<()> {
    if let Some(text) = read_optional_evidence(root, "package.json", plan)? {
        let package: serde_json::Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(error) => {
                block_invalid_evidence(
                    plan,
                    "package.json",
                    format!("invalid package.json: {error}"),
                );
                serde_json::Value::Null
            }
        };
        let workspaces = package.get("workspaces").and_then(|value| {
            value
                .as_array()
                .or_else(|| value.get("packages").and_then(serde_json::Value::as_array))
        });
        if let Some(workspaces) = workspaces {
            patterns.extend(
                workspaces
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(ToOwned::to_owned),
            );
            let mut lockfiles = Vec::new();
            for path in [
                "package-lock.json",
                "pnpm-lock.yaml",
                "yarn.lock",
                "bun.lock",
                "bun.lockb",
            ] {
                if has_evidence(root, path, plan)? {
                    lockfiles.push(path);
                }
            }
            let manager_kinds = lockfiles
                .iter()
                .map(|path| {
                    if path.starts_with("bun.") {
                        "bun"
                    } else {
                        *path
                    }
                })
                .collect::<std::collections::BTreeSet<_>>();
            if manager_kinds.len() > 1 {
                block_ambiguity(
                    plan,
                    "package_manager_conflict",
                    lockfiles.iter().map(|path| (*path).to_owned()).collect(),
                    "multiple package manager choices are present for declared workspaces",
                );
            } else if let (Some(lockfile), Some(declared)) = (
                lockfiles.first(),
                package
                    .get("packageManager")
                    .and_then(serde_json::Value::as_str),
            ) {
                let lock_manager = match *lockfile {
                    "package-lock.json" => "npm",
                    "pnpm-lock.yaml" => "pnpm",
                    "yarn.lock" => "yarn",
                    "bun.lock" | "bun.lockb" => "bun",
                    _ => "",
                };
                if !declared.starts_with(lock_manager) {
                    block_ambiguity(
                        plan,
                        "package_manager_conflict",
                        vec!["package.json".to_owned(), (*lockfile).to_owned()],
                        "packageManager and workspace lockfile select different package managers",
                    );
                }
            }
        }
    }

    for path in ["composer.json"] {
        let Some(text) = read_optional_evidence(root, path, plan)? else {
            continue;
        };
        let value: serde_json::Value = match serde_json::from_str(&text) {
            Ok(value) => value,
            Err(_) => continue,
        };
        if let Some(items) = value
            .get("extra")
            .and_then(|value| value.get("merge-plugin"))
            .and_then(|value| value.get("include"))
            .and_then(serde_json::Value::as_array)
        {
            patterns.extend(
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .filter_map(|path| Path::new(path).parent())
                    .map(|path| path.to_string_lossy().into_owned()),
            );
        }
    }
    Ok(())
}

fn collect_line_workspace_patterns(
    root: &Path,
    plan: &mut GateScaffoldPlan,
    patterns: &mut std::collections::BTreeSet<String>,
    coverage_patterns: &mut std::collections::BTreeMap<
        String,
        std::collections::BTreeSet<&'static str>,
    >,
) -> io::Result<()> {
    if let Some(text) = read_optional_evidence(root, "pnpm-workspace.yaml", plan)? {
        let mut in_packages = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed == "packages:" {
                in_packages = true;
                continue;
            }
            if in_packages && !line.starts_with(char::is_whitespace) {
                break;
            }
            if in_packages && let Some(pattern) = trimmed.strip_prefix('-') {
                patterns.insert(pattern.trim().trim_matches(['\'', '"']).to_owned());
            }
        }
    }
    if let Some(text) = read_optional_evidence(root, "go.work", plan)? {
        let mut in_block = false;
        for line in text.lines() {
            let trimmed = line.trim();
            if trimmed == "use (" {
                in_block = true;
            } else if in_block && trimmed == ")" {
                in_block = false;
            } else if in_block && !trimmed.is_empty() {
                patterns.insert(trimmed.trim_start_matches("./").to_owned());
            } else if let Some(path) = trimmed.strip_prefix("use ") {
                patterns.insert(path.trim().trim_start_matches("./").to_owned());
            }
        }
    }
    if let Some(text) = read_optional_evidence(root, "pom.xml", plan)? {
        for module in xml_values(&text, "module") {
            patterns.insert(module.clone());
            record_root_coverage(coverage_patterns, module, "java.maven/v1");
        }
    }
    for settings in ["settings.gradle", "settings.gradle.kts"] {
        if let Some(text) = read_optional_evidence(root, settings, plan)? {
            for line in text
                .lines()
                .filter(|line| line.trim_start().starts_with("include"))
            {
                for quoted in quoted_values(line) {
                    let module = quoted.trim_start_matches(':').replace(':', "/");
                    patterns.insert(module.clone());
                    record_root_coverage(coverage_patterns, module, "java.gradle/v1");
                }
            }
        }
    }
    for solution in root_files_with_extensions(root, &["sln"])? {
        let text = fs::read_to_string(root.join(&solution))?;
        for quoted in quoted_values(&text) {
            let normalized = quoted.replace('\\', "/");
            if ["csproj", "fsproj", "vbproj"]
                .iter()
                .any(|extension| normalized.ends_with(extension))
                && let Some(parent) = Path::new(&normalized).parent()
            {
                let project = parent.to_string_lossy().into_owned();
                patterns.insert(project.clone());
                record_root_coverage(coverage_patterns, project, "dotnet.test/v1");
            }
        }
    }
    for (path, marker) in [
        ("CMakeLists.txt", "add_subdirectory"),
        ("meson.build", "subdir"),
    ] {
        if let Some(text) = read_optional_evidence(root, path, plan)? {
            for line in text.lines().filter(|line| line.contains(marker)) {
                if let Some(arguments) = line
                    .split_once('(')
                    .and_then(|(_, rest)| rest.split_once(')'))
                    && let Some(value) = arguments.0.split_whitespace().next()
                {
                    let workspace = value.trim_matches(['\'', '"']).to_owned();
                    patterns.insert(workspace.clone());
                    let template = if path == "CMakeLists.txt" {
                        "cpp.cmake/v1"
                    } else {
                        "cpp.meson/v1"
                    };
                    record_root_coverage(coverage_patterns, workspace, template);
                }
            }
        }
    }
    if let Some(text) = read_optional_evidence(root, "mix.exs", plan)? {
        for line in text.lines() {
            if let Some((_, value)) = line.split_once("apps_path:") {
                let value = value.trim().trim_matches([',', '\'', '"']);
                if !value.is_empty() {
                    let workspace = format!("{value}/*");
                    patterns.insert(workspace.clone());
                    record_root_coverage(coverage_patterns, workspace, "elixir.mix/v1");
                }
            }
        }
    }
    Ok(())
}

fn record_root_coverage(
    coverage_patterns: &mut std::collections::BTreeMap<
        String,
        std::collections::BTreeSet<&'static str>,
    >,
    pattern: String,
    template: &'static str,
) {
    coverage_patterns
        .entry(pattern)
        .or_default()
        .insert(template);
}

fn xml_values(text: &str, tag: &str) -> Vec<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let mut values = Vec::new();
    let mut rest = text;
    while let Some((_, after_open)) = rest.split_once(&open) {
        let Some((value, after_close)) = after_open.split_once(&close) else {
            break;
        };
        values.push(value.trim().to_owned());
        rest = after_close;
    }
    values
}

fn quoted_values(text: &str) -> Vec<String> {
    let mut values = Vec::new();
    for quote in ['\'', '"'] {
        let mut inside = false;
        let mut current = String::new();
        for character in text.chars() {
            if character == quote {
                if inside {
                    values.push(std::mem::take(&mut current));
                }
                inside = !inside;
            } else if inside {
                current.push(character);
            }
        }
    }
    values
}

fn invalid_workspace_pattern(pattern: &str) -> bool {
    let path = Path::new(pattern);
    path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
}

const WORKSPACE_EXCLUDED_COMPONENTS: [&str; 17] = [
    ".git",
    ".nopal",
    ".beislid",
    ".cache",
    "node_modules",
    "target",
    "build",
    "dist",
    "vendor",
    "generated",
    "fixtures",
    "fixture",
    "examples",
    "example",
    "deps",
    "_build",
    "coverage",
];

fn expand_workspace_pattern(
    root: &Path,
    pattern: &str,
    plan: &mut GateScaffoldPlan,
) -> io::Result<Vec<String>> {
    let normalized = pattern
        .trim()
        .trim_matches(['\'', '"'])
        .trim_start_matches("./")
        .trim_end_matches('/');
    if normalized.is_empty() || normalized == "." {
        return Ok(Vec::new());
    }
    let components = normalized.split('/').collect::<Vec<_>>();
    let mut matches = Vec::new();
    expand_components(root, Path::new(""), &components, 0, &mut matches, plan)?;
    matches.sort();
    matches.dedup();
    Ok(matches)
}

fn expand_components(
    root: &Path,
    relative: &Path,
    components: &[&str],
    depth: usize,
    matches: &mut Vec<String>,
    plan: &mut GateScaffoldPlan,
) -> io::Result<()> {
    if components.is_empty() {
        if !relative.as_os_str().is_empty() {
            matches.push(relative.to_string_lossy().replace('\\', "/"));
        }
        return Ok(());
    }
    if depth > 12 {
        block_workspace(
            plan,
            &relative.to_string_lossy(),
            "workspace expansion exceeded the bounded declaration depth",
        );
        return Ok(());
    }
    let component = components[0];
    if component == "**" {
        expand_components(root, relative, &components[1..], depth + 1, matches, plan)?;
        for child in confined_child_directories(root, relative, plan)? {
            expand_components(root, &child, components, depth + 1, matches, plan)?;
        }
        return Ok(());
    }
    if component.contains(['*', '?', '[']) {
        let glob = match globset::Glob::new(component) {
            Ok(glob) => glob.compile_matcher(),
            Err(error) => {
                block_workspace(plan, component, &format!("invalid workspace glob: {error}"));
                return Ok(());
            }
        };
        for child in confined_child_directories(root, relative, plan)? {
            let name = child
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default();
            if glob.is_match(name) {
                expand_components(root, &child, &components[1..], depth + 1, matches, plan)?;
            }
        }
        return Ok(());
    }
    if excluded_component(component) {
        return Ok(());
    }
    let next = relative.join(component);
    let path = root.join(&next);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            block_workspace(
                plan,
                &next.to_string_lossy(),
                "declared workspace paths cannot contain symlinks",
            );
        }
        Ok(metadata) if metadata.is_dir() => {
            expand_components(root, &next, &components[1..], depth + 1, matches, plan)?;
        }
        Ok(_) | Err(_) => {}
    }
    Ok(())
}

fn confined_child_directories(
    root: &Path,
    relative: &Path,
    plan: &mut GateScaffoldPlan,
) -> io::Result<Vec<std::path::PathBuf>> {
    let mut children = Vec::new();
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let name = entry.file_name();
        let name_text = name.to_string_lossy();
        if excluded_component(&name_text) {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            block_workspace(
                plan,
                &relative.join(&name).to_string_lossy(),
                "declared workspace expansion encountered a symlink",
            );
        } else if file_type.is_dir() {
            children.push(relative.join(name));
        }
    }
    children.sort();
    Ok(children)
}

fn excluded_component(component: &str) -> bool {
    WORKSPACE_EXCLUDED_COMPONENTS
        .iter()
        .any(|excluded| component.eq_ignore_ascii_case(excluded))
}

fn block_workspace(plan: &mut GateScaffoldPlan, path: &str, message: &str) {
    plan.readiness = Readiness::Blocked;
    plan.ok = false;
    plan.diagnostics
        .push(Diagnostic::error(Code::GateWorkspaceInvalid, path, message));
    plan.decisions.push(DetectionDecision {
        scope: ".".to_owned(),
        template_id: None,
        outcome: DecisionOutcome::Blocked,
        reason_code: "workspace_boundary_invalid".to_owned(),
        evidence: vec![path.to_owned()],
        message: message.to_owned(),
    });
}

const VALIDATION_TASKS: [&str; 8] = [
    "check",
    "lint",
    "typecheck",
    "type-check",
    "test",
    "verify",
    "ci",
    "format:check",
];

#[derive(Debug)]
struct ExplicitTaskSource {
    template_id: &'static str,
    evidence: String,
    program: &'static str,
    subcommand: Option<&'static str>,
    tasks: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default)]
struct TaskSuppression {
    all_defaults: bool,
    php_defaults: bool,
}

fn detect_explicit_tasks(
    root: &Path,
    plan: &mut GateScaffoldPlan,
    inherited_manager: Option<&PackageManagerChoice>,
) -> io::Result<TaskSuppression> {
    let mut universal = Vec::new();
    collect_first_task_variant(
        root,
        plan,
        &["Makefile", "GNUmakefile", "makefile"],
        "task.make/v1",
        "make",
        None,
        LineTaskSyntax::ColonAtRoot,
        &mut universal,
    )?;
    collect_first_task_variant(
        root,
        plan,
        &["justfile", "Justfile"],
        "task.just/v1",
        "just",
        None,
        LineTaskSyntax::ColonAtRoot,
        &mut universal,
    )?;
    collect_first_task_variant(
        root,
        plan,
        &[
            "Taskfile.yml",
            "Taskfile.yaml",
            "taskfile.yml",
            "taskfile.yaml",
        ],
        "task.taskfile/v1",
        "task",
        None,
        LineTaskSyntax::YamlTasks,
        &mut universal,
    )?;
    collect_first_task_variant(
        root,
        plan,
        &["mise.toml", ".mise.toml"],
        "task.mise/v1",
        "mise",
        Some("run"),
        LineTaskSyntax::MiseSections,
        &mut universal,
    )?;
    let mut family_specific = Vec::new();
    collect_package_scripts(root, plan, inherited_manager, &mut family_specific)?;
    collect_composer_scripts(root, plan, &mut family_specific)?;

    if universal.len() > 1 {
        block_ambiguity(
            plan,
            "explicit_task_runner_conflict",
            universal
                .iter()
                .map(|source| source.evidence.clone())
                .collect(),
            "multiple explicit validation task runners apply to the repository root",
        );
        return Ok(TaskSuppression {
            all_defaults: true,
            php_defaults: true,
        });
    }
    if let Some(source) = universal.pop() {
        select_explicit_task_source(plan, &source);
        for source in family_specific {
            plan.decisions.push(DetectionDecision {
                scope: ".".to_owned(),
                template_id: Some(source.template_id.to_owned()),
                outcome: DecisionOutcome::Superseded,
                reason_code: "repository_task_precedence".to_owned(),
                evidence: vec![source.evidence],
                message: "repository task runner supersedes family-specific package scripts"
                    .to_owned(),
            });
        }
        return Ok(TaskSuppression {
            all_defaults: true,
            php_defaults: true,
        });
    }

    let mut suppression = TaskSuppression::default();
    for source in family_specific {
        suppression.php_defaults |= source.template_id == "php.composer/v1";
        select_explicit_task_source(plan, &source);
    }
    Ok(suppression)
}

fn select_explicit_task_source(plan: &mut GateScaffoldPlan, source: &ExplicitTaskSource) {
    let gates = source
        .tasks
        .iter()
        .map(|task| {
            let mut argv = vec![source.program.to_owned()];
            if let Some(subcommand) = source.subcommand {
                argv.push(subcommand.to_owned());
            }
            argv.push(task.clone());
            (slug(task), argv)
        })
        .collect();
    select_template(
        plan,
        source.template_id,
        source.template_id.split('/').next().unwrap_or("task"),
        vec![source.evidence.clone()],
        gates,
    );
}

#[derive(Debug, Clone, Copy)]
enum LineTaskSyntax {
    ColonAtRoot,
    YamlTasks,
    MiseSections,
}

#[allow(clippy::too_many_arguments)]
fn collect_first_task_variant(
    root: &Path,
    plan: &mut GateScaffoldPlan,
    paths: &[&str],
    template_id: &'static str,
    program: &'static str,
    subcommand: Option<&'static str>,
    syntax: LineTaskSyntax,
    sources: &mut Vec<ExplicitTaskSource>,
) -> io::Result<()> {
    let initial_len = sources.len();
    for path in paths {
        collect_line_task_source(
            root,
            plan,
            path,
            template_id,
            program,
            subcommand,
            syntax,
            sources,
        )?;
        if sources.len() > initial_len {
            break;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn collect_line_task_source(
    root: &Path,
    plan: &mut GateScaffoldPlan,
    path: &str,
    template_id: &'static str,
    program: &'static str,
    subcommand: Option<&'static str>,
    syntax: LineTaskSyntax,
    sources: &mut Vec<ExplicitTaskSource>,
) -> io::Result<()> {
    let Some(text) = read_optional_evidence(root, path, plan)? else {
        return Ok(());
    };
    let tasks = match syntax {
        LineTaskSyntax::ColonAtRoot => line_colon_tasks(&text),
        LineTaskSyntax::YamlTasks => yaml_tasks(&text),
        LineTaskSyntax::MiseSections => mise_tasks(&text),
    };
    if !tasks.is_empty() {
        sources.push(ExplicitTaskSource {
            template_id,
            evidence: path.to_owned(),
            program,
            subcommand,
            tasks,
        });
    }
    Ok(())
}

fn line_colon_tasks(text: &str) -> Vec<String> {
    let declared = text.lines().filter_map(|line| {
        if line.starts_with(char::is_whitespace) || line.starts_with('#') {
            return None;
        }
        line.split_once(':')
            .map(|(name, _)| name.trim())
            .filter(|name| !name.contains(['=', ' ']))
    });
    ordered_validation_tasks(declared)
}

fn yaml_tasks(text: &str) -> Vec<String> {
    let mut in_tasks = false;
    let mut declared = Vec::new();
    for line in text.lines() {
        if line.trim() == "tasks:" && !line.starts_with(char::is_whitespace) {
            in_tasks = true;
            continue;
        }
        if in_tasks && !line.trim().is_empty() && !line.starts_with(char::is_whitespace) {
            break;
        }
        if in_tasks {
            let spaces = line
                .chars()
                .take_while(|character| *character == ' ')
                .count();
            if spaces == 2
                && let Some(name) = line.trim().strip_suffix(':')
            {
                declared.push(name);
            }
        }
    }
    ordered_validation_tasks(declared)
}

fn mise_tasks(text: &str) -> Vec<String> {
    let declared = text.lines().filter_map(|line| {
        line.trim()
            .strip_prefix("[tasks.")
            .and_then(|value| value.strip_suffix(']'))
            .map(|value| value.trim_matches(['\'', '"']))
    });
    ordered_validation_tasks(declared)
}

fn ordered_validation_tasks<'a>(declared: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let declared = declared
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    VALIDATION_TASKS
        .iter()
        .filter(|task| declared.contains(**task))
        .map(|task| (*task).to_owned())
        .collect()
}

fn collect_package_scripts(
    root: &Path,
    plan: &mut GateScaffoldPlan,
    inherited_manager: Option<&PackageManagerChoice>,
    sources: &mut Vec<ExplicitTaskSource>,
) -> io::Result<()> {
    let Some(text) = read_optional_evidence(root, "package.json", plan)? else {
        return Ok(());
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            block_invalid_evidence(
                plan,
                "package.json",
                format!("invalid package.json: {error}"),
            );
            return Ok(());
        }
    };
    let tasks = value
        .get("scripts")
        .and_then(serde_json::Value::as_object)
        .map(|scripts| ordered_validation_tasks(scripts.keys().map(String::as_str)))
        .unwrap_or_default();
    if tasks.is_empty() {
        return Ok(());
    }
    let manager = detect_package_manager(root, plan, &value, inherited_manager)?;
    let Some(manager) = manager else {
        return Ok(());
    };
    sources.push(ExplicitTaskSource {
        template_id: manager.template_id,
        evidence: manager.evidence,
        program: manager.program,
        subcommand: Some("run"),
        tasks,
    });
    Ok(())
}

fn collect_composer_scripts(
    root: &Path,
    plan: &mut GateScaffoldPlan,
    sources: &mut Vec<ExplicitTaskSource>,
) -> io::Result<()> {
    let Some(text) = read_optional_evidence(root, "composer.json", plan)? else {
        return Ok(());
    };
    let value: serde_json::Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            block_invalid_evidence(
                plan,
                "composer.json",
                format!("invalid composer.json: {error}"),
            );
            return Ok(());
        }
    };
    let tasks = value
        .get("scripts")
        .and_then(serde_json::Value::as_object)
        .map(|scripts| ordered_validation_tasks(scripts.keys().map(String::as_str)))
        .unwrap_or_default();
    if !tasks.is_empty() {
        sources.push(ExplicitTaskSource {
            template_id: "php.composer/v1",
            evidence: "composer.json".to_owned(),
            program: "composer",
            subcommand: Some("run-script"),
            tasks,
        });
    }
    Ok(())
}

#[derive(Debug, Clone)]
struct PackageManagerChoice {
    template_id: &'static str,
    program: &'static str,
    evidence: String,
}

fn detect_package_manager(
    root: &Path,
    plan: &mut GateScaffoldPlan,
    package: &serde_json::Value,
    inherited: Option<&PackageManagerChoice>,
) -> io::Result<Option<PackageManagerChoice>> {
    let candidates = [
        ("javascript.npm/v1", "npm", "package-lock.json"),
        ("javascript.pnpm/v1", "pnpm", "pnpm-lock.yaml"),
        ("javascript.yarn/v1", "yarn", "yarn.lock"),
        ("javascript.bun/v1", "bun", "bun.lock"),
        ("javascript.bun/v1", "bun", "bun.lockb"),
    ];
    let mut found = Vec::new();
    for candidate in candidates {
        if has_evidence(root, candidate.2, plan)? {
            found.push(candidate);
        }
    }
    if found.is_empty() {
        if let Some(manager) = package
            .get("packageManager")
            .and_then(serde_json::Value::as_str)
            && let Some(candidate) = candidates
                .iter()
                .find(|candidate| manager.starts_with(candidate.1))
        {
            return Ok(Some(PackageManagerChoice {
                template_id: candidate.0,
                program: candidate.1,
                evidence: "package.json".to_owned(),
            }));
        }
        if let Some(inherited) = inherited {
            return Ok(Some(inherited.clone()));
        }
        block_ambiguity(
            plan,
            "package_manager_unproven",
            vec!["package.json".to_owned()],
            "package scripts exist but no single package manager lock or packageManager field proves how to run them",
        );
        return Ok(None);
    }
    found.dedup_by_key(|candidate| candidate.0);
    if found.len() != 1 {
        block_ambiguity(
            plan,
            "package_manager_conflict",
            found
                .iter()
                .map(|candidate| candidate.2.to_owned())
                .collect(),
            "multiple package manager choices are present",
        );
        return Ok(None);
    }
    let selected = found[0];
    if let Some(declared) = package
        .get("packageManager")
        .and_then(serde_json::Value::as_str)
        .and_then(|manager| {
            candidates
                .iter()
                .find(|candidate| manager.starts_with(candidate.1))
        })
        && declared.0 != selected.0
    {
        block_ambiguity(
            plan,
            "package_manager_conflict",
            vec!["package.json".to_owned(), selected.2.to_owned()],
            "packageManager and lockfile select different package managers",
        );
        return Ok(None);
    }
    Ok(found.first().map(|candidate| PackageManagerChoice {
        template_id: candidate.0,
        program: candidate.1,
        evidence: candidate.2.to_owned(),
    }))
}

fn detect_ecosystem_defaults(
    root: &Path,
    plan: &mut GateScaffoldPlan,
    suppression: TaskSuppression,
) -> io::Result<()> {
    let suppressed = suppression.all_defaults;
    if has_evidence(root, "Cargo.toml", plan)? {
        choose_template(
            plan,
            suppressed,
            "rust.cargo/v1",
            "rust",
            vec!["Cargo.toml".to_owned()],
            vec![
                gate("cargo-fmt", &["cargo", "fmt", "--all", "--check"]),
                gate(
                    "cargo-clippy",
                    &[
                        "cargo",
                        "clippy",
                        "--workspace",
                        "--all-targets",
                        "--",
                        "-D",
                        "warnings",
                    ],
                ),
                gate("cargo-test", &["cargo", "test", "--workspace"]),
            ],
        );
    }

    let pyproject = read_optional_evidence(root, "pyproject.toml", plan)?;
    let python_tools = [
        (
            "[tool.pytest",
            &["pytest.ini"][..],
            "python.pytest/v1",
            "pytest",
            &["python", "-m", "pytest"][..],
        ),
        (
            "[tool.ruff",
            &["ruff.toml", ".ruff.toml"][..],
            "python.ruff/v1",
            "ruff-check",
            &["python", "-m", "ruff", "check", "."][..],
        ),
        (
            "[tool.mypy",
            &["mypy.ini", ".mypy.ini"][..],
            "python.mypy/v1",
            "mypy",
            &["python", "-m", "mypy", "."][..],
        ),
    ];
    for (marker, config_paths, template, purpose, command) in python_tools {
        let mut evidence = Vec::new();
        if pyproject
            .as_deref()
            .is_some_and(|contents| contents.contains(marker))
        {
            evidence.push("pyproject.toml".to_owned());
        }
        for config_path in config_paths {
            if has_evidence(root, config_path, plan)? {
                evidence.push((*config_path).to_owned());
            }
        }
        if !evidence.is_empty() {
            choose_template(
                plan,
                suppressed,
                template,
                "python",
                evidence,
                vec![gate(purpose, command)],
            );
        }
    }

    for (path, template, namespace, purpose, command) in [
        (
            "go.mod",
            "go.test/v1",
            "go",
            "go-test",
            &["go", "test", "./..."][..],
        ),
        (
            "mix.exs",
            "elixir.mix/v1",
            "elixir",
            "mix-test",
            &["mix", "test"][..],
        ),
        (
            "Package.swift",
            "swift.spm/v1",
            "swift",
            "swift-test",
            &["swift", "test", "--skip-update"][..],
        ),
    ] {
        if has_evidence(root, path, plan)? {
            choose_template(
                plan,
                suppressed,
                template,
                namespace,
                vec![path.to_owned()],
                vec![gate(purpose, command)],
            );
        }
    }

    let gemfile = read_optional_evidence(root, "Gemfile", plan)?;
    let rspec = has_evidence(root, ".rspec", plan)?;
    if rspec
        || gemfile
            .as_deref()
            .is_some_and(|text| text.contains("rspec"))
    {
        let mut evidence = vec!["Gemfile".to_owned()];
        if rspec {
            evidence.push(".rspec".to_owned());
        }
        choose_template(
            plan,
            suppressed,
            "ruby.rspec/v1",
            "ruby",
            evidence,
            vec![gate("rspec", &["bundle", "exec", "rspec"])],
        );
    }

    detect_java(root, plan, suppressed)?;
    detect_dotnet(root, plan, suppressed)?;
    detect_php(
        root,
        plan,
        suppression.all_defaults || suppression.php_defaults,
    )?;
    detect_cpp(root, plan, suppressed)?;
    Ok(())
}

fn detect_java(root: &Path, plan: &mut GateScaffoldPlan, suppressed: bool) -> io::Result<()> {
    let maven = has_evidence(root, "pom.xml", plan)?;
    let gradle_path = if has_evidence(root, "build.gradle.kts", plan)? {
        Some("build.gradle.kts")
    } else if has_evidence(root, "build.gradle", plan)? {
        Some("build.gradle")
    } else {
        None
    };
    if maven && gradle_path.is_some() && !suppressed {
        block_ambiguity(
            plan,
            "java_build_tool_conflict",
            vec![
                "pom.xml".to_owned(),
                gradle_path.unwrap_or_default().to_owned(),
            ],
            "Maven and Gradle both claim the repository root",
        );
        return Ok(());
    }
    if maven {
        let wrapper = has_evidence(root, "mvnw", plan)?;
        choose_template(
            plan,
            suppressed,
            "java.maven/v1",
            "java",
            vec!["pom.xml".to_owned()],
            vec![gate(
                "maven-test",
                &[if wrapper { "./mvnw" } else { "mvn" }, "-o", "test"],
            )],
        );
    }
    if let Some(path) = gradle_path {
        let wrapper = has_evidence(root, "gradlew", plan)?;
        choose_template(
            plan,
            suppressed,
            "java.gradle/v1",
            "java",
            vec![path.to_owned()],
            vec![gate(
                "gradle-test",
                &[
                    if wrapper { "./gradlew" } else { "gradle" },
                    "--offline",
                    "test",
                ],
            )],
        );
    }
    Ok(())
}

fn detect_dotnet(root: &Path, plan: &mut GateScaffoldPlan, suppressed: bool) -> io::Result<()> {
    let mut manifests =
        root_files_with_extensions(root, &["sln", "slnx", "csproj", "fsproj", "vbproj"])?;
    manifests.sort();
    if let Some(manifest) = manifests.first() {
        choose_template(
            plan,
            suppressed,
            "dotnet.test/v1",
            "dotnet",
            manifests.clone(),
            vec![gate(
                "dotnet-test",
                &["dotnet", "test", manifest, "--no-restore"],
            )],
        );
    }
    Ok(())
}

fn detect_php(root: &Path, plan: &mut GateScaffoldPlan, suppressed: bool) -> io::Result<()> {
    let Some(composer) = read_optional_evidence(root, "composer.json", plan)? else {
        return Ok(());
    };
    let phpunit_config =
        has_evidence(root, "phpunit.xml", plan)? || has_evidence(root, "phpunit.xml.dist", plan)?;
    if composer.contains("phpunit") || phpunit_config {
        choose_template(
            plan,
            suppressed,
            "php.composer/v1",
            "php",
            vec!["composer.json".to_owned()],
            vec![gate("phpunit", &["vendor/bin/phpunit"])],
        );
    }
    Ok(())
}

fn detect_cpp(root: &Path, plan: &mut GateScaffoldPlan, suppressed: bool) -> io::Result<()> {
    let cmake = has_evidence(root, "CMakeLists.txt", plan)?;
    let meson = has_evidence(root, "meson.build", plan)?;
    if cmake && meson && !suppressed {
        block_ambiguity(
            plan,
            "cpp_build_tool_conflict",
            vec!["CMakeLists.txt".to_owned(), "meson.build".to_owned()],
            "CMake and Meson both claim the repository root",
        );
        return Ok(());
    }
    if cmake {
        choose_template(
            plan,
            suppressed,
            "cpp.cmake/v1",
            "cpp",
            vec!["CMakeLists.txt".to_owned()],
            vec![gate(
                "cmake-test",
                &["cmake", "--build", "build", "--target", "test"],
            )],
        );
    }
    if meson {
        choose_template(
            plan,
            suppressed,
            "cpp.meson/v1",
            "cpp",
            vec!["meson.build".to_owned()],
            vec![gate("meson-test", &["meson", "test", "-C", "build"])],
        );
    }
    Ok(())
}

fn choose_template(
    plan: &mut GateScaffoldPlan,
    suppressed: bool,
    template_id: &str,
    namespace: &str,
    evidence: Vec<String>,
    gates: Vec<(String, Vec<String>)>,
) {
    if suppressed {
        plan.decisions.push(DetectionDecision {
            scope: ".".to_owned(),
            template_id: Some(template_id.to_owned()),
            outcome: DecisionOutcome::Superseded,
            reason_code: "explicit_task_precedence".to_owned(),
            evidence,
            message: "explicit repository validation tasks supersede this ecosystem default"
                .to_owned(),
        });
    } else {
        select_template(plan, template_id, namespace, evidence, gates);
    }
}

fn select_template(
    plan: &mut GateScaffoldPlan,
    template_id: &str,
    namespace: &str,
    evidence: Vec<String>,
    gates: Vec<(String, Vec<String>)>,
) {
    plan.templates.push(TemplateSelection {
        id: template_id.to_owned(),
        scope: ".".to_owned(),
        evidence: evidence.clone(),
    });
    for (purpose, argv) in gates {
        plan.gates.push(GeneratedGate {
            id: format!("detected.root.{namespace}.{purpose}"),
            stage: "pre_pr".to_owned(),
            argv,
            cwd: None,
            parallel_safe: false,
            mutates: false,
            template_id: template_id.to_owned(),
        });
    }
    plan.decisions.push(DetectionDecision {
        scope: ".".to_owned(),
        template_id: Some(template_id.to_owned()),
        outcome: DecisionOutcome::Selected,
        reason_code: "evidence_proven".to_owned(),
        evidence,
        message: "selected evidence-backed validation gates".to_owned(),
    });
}

fn gate(purpose: &str, argv: &[&str]) -> (String, Vec<String>) {
    (
        purpose.to_owned(),
        argv.iter().map(|argument| (*argument).to_owned()).collect(),
    )
}

fn slug(value: &str) -> String {
    let mut result = String::new();
    let mut previous_dash = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() {
            result.push(character.to_ascii_lowercase());
            previous_dash = false;
        } else if !previous_dash {
            result.push('-');
            previous_dash = true;
        }
    }
    result.trim_matches('-').to_owned()
}

fn block_ambiguity(
    plan: &mut GateScaffoldPlan,
    reason_code: &str,
    mut evidence: Vec<String>,
    message: &str,
) {
    evidence.sort();
    plan.readiness = Readiness::Blocked;
    plan.ok = false;
    plan.decisions.push(DetectionDecision {
        scope: ".".to_owned(),
        template_id: None,
        outcome: DecisionOutcome::Ambiguous,
        reason_code: reason_code.to_owned(),
        evidence: evidence.clone(),
        message: message.to_owned(),
    });
    plan.diagnostics.push(Diagnostic::error(
        Code::GateScaffoldAmbiguous,
        evidence.first().cloned().unwrap_or_else(|| ".".to_owned()),
        format!("{message}: [{}]", evidence.join(", ")),
    ));
}

fn block_invalid_evidence(plan: &mut GateScaffoldPlan, path: &str, message: String) {
    plan.readiness = Readiness::Blocked;
    plan.ok = false;
    plan.diagnostics.push(Diagnostic::error(
        Code::GateScaffoldEvidenceInvalid,
        path,
        message,
    ));
}

fn has_evidence(root: &Path, relative: &str, plan: &mut GateScaffoldPlan) -> io::Result<bool> {
    match evidence_file(root, relative) {
        Ok(present) => Ok(present),
        Err(diagnostic) => {
            plan.readiness = Readiness::Blocked;
            plan.ok = false;
            plan.diagnostics.push(diagnostic);
            Ok(false)
        }
    }
}

fn read_optional_evidence(
    root: &Path,
    relative: &str,
    plan: &mut GateScaffoldPlan,
) -> io::Result<Option<String>> {
    if !has_evidence(root, relative, plan)? {
        return Ok(None);
    }
    let path = root.join(relative);
    let metadata = fs::metadata(&path)?;
    if metadata.len() > 1024 * 1024 {
        block_invalid_evidence(
            plan,
            relative,
            "ecosystem manifest exceeds the 1 MiB detection limit".to_owned(),
        );
        return Ok(None);
    }
    fs::read_to_string(path).map(Some)
}

fn root_files_with_extensions(root: &Path, extensions: &[&str]) -> io::Result<Vec<String>> {
    let mut matches = Vec::new();
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let metadata = entry.file_type()?;
        if !metadata.is_file() {
            continue;
        }
        let path = entry.path();
        if path
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extensions.contains(&extension))
        {
            matches.push(entry.file_name().to_string_lossy().into_owned());
        }
    }
    matches.sort();
    Ok(matches)
}

fn evidence_file(root: &Path, relative: &str) -> Result<bool, Diagnostic> {
    let path = root.join(relative);
    let metadata = match fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => {
            return Err(Diagnostic::error(
                Code::GateScaffoldEvidenceInvalid,
                relative,
                format!("could not inspect ecosystem evidence: {error}"),
            ));
        }
    };
    if !metadata.file_type().is_file() {
        return Err(Diagnostic::error(
            Code::GateScaffoldEvidenceInvalid,
            relative,
            "ecosystem evidence must be a regular non-symlink file",
        ));
    }
    Ok(true)
}
