//! Cold launch planning for the enforced Pi distribution.
//!
//! One plan covers existing projects and the generated first-run baseline.
//! It never writes, installs, resolves versions, or starts Pi. The real
//! launch writes only when this plan reports `WouldCreate`, then re-runs this
//! same planner against the committed files before executing anything.

use std::io;
use std::path::Path;

use nopal_core::bundle::AmbientInherit;
use nopal_core::diagnostics::{self, Code, Diagnostic, Severity};
use nopal_core::distribution::{
    self, BuiltinDistribution, DistributionContext, DistributionReport, ResolvedResource,
};
use nopal_core::gate_scaffold::{GateScaffoldPlan, Readiness};
use nopal_core::process_artifact;
use nopal_core::scaffold::{self, ScaffoldSource};
use nopal_core::toon::{self, Value};
use nopal_core::validate;

pub const LAUNCH_KIND: &str = "nopal.launch/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scaffold {
    None,
    WouldCreate,
    Created,
}

impl Scaffold {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::WouldCreate => "would_create",
            Self::Created => "created",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub struct LaunchContext<'a> {
    pub store_root: &'a Path,
    pub builtin: BuiltinDistribution<'a>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LaunchPlan {
    pub kind: &'static str,
    pub ok: bool,
    pub would_exec: bool,
    pub validity_ok: bool,
    pub ready: bool,
    pub ambient: bool,
    pub ambient_kinds: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_artifact_note: Option<String>,
    pub scaffold: Scaffold,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_scaffold: Option<GateScaffoldPlan>,
    /// The baseline is runtime preparation state, not report authority. It is
    /// retained so publication uses the exact evidence snapshot just shown.
    #[serde(skip)]
    pub prepared_baseline: Option<scaffold::Baseline>,
    /// Kept under the historical `bundle` field so machine consumers receive
    /// a compatible report position while its content is now the stronger
    /// contract-plus-lock distribution report.
    pub bundle: DistributionReport,
    pub pi_argv: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

pub fn plan(dir: &Path, context: LaunchContext<'_>) -> io::Result<LaunchPlan> {
    let legacy = dir.join(nopal_core::discover::LEGACY_DIR);
    if legacy.exists() {
        return Ok(blocked_unconfigured_plan(Diagnostic::error(
            Code::ScaffoldLegacyDetected,
            legacy.display().to_string(),
            format!(
                "legacy project state {} is preserved; Nopal will not merge or launch through it",
                legacy.display()
            ),
        )));
    }
    if !dir.join(".nopal").exists() {
        return plan_without_nopal(dir, context);
    }
    let required = [
        ".nopal/nopal.jsonc",
        ".nopal/policy.jsonc",
        ".nopal/gates.jsonc",
        distribution::BUNDLE_PATH,
        distribution::LOCK_PATH,
        ".beislid/workflow.md",
    ];
    let missing = required
        .iter()
        .filter(|path| !dir.join(path).is_file())
        .copied()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Ok(blocked_unconfigured_plan(Diagnostic::error(
            Code::ScaffoldIncomplete,
            ".nopal",
            format!(
                "partial Nopal baseline is preserved; missing [{}]",
                missing.join(", ")
            ),
        )));
    }
    plan_configured(dir, context)
}

fn plan_configured(dir: &Path, context: LaunchContext<'_>) -> io::Result<LaunchPlan> {
    let validation = validate::validate(dir)?;
    // v0.3 treats every checked-in Nopal schema error as a launch error. The
    // old optional-module exception made invalid configuration depend on the
    // selected profile and contradicted fail-closed project portability.
    let validity_ok = validation.ok();
    let mut diagnostics = validation.diagnostics;
    let (gate_ready, gate_diagnostics) = checked_in_gates_ready(dir)?;
    diagnostics.extend(gate_diagnostics);
    if !gate_ready {
        diagnostics.push(Diagnostic::error(
            Code::GateConfigurationRequired,
            ".nopal/gates.jsonc",
            "detected project has only the baseline diff check; add explicit Nopal or typed Beislið gates before launch",
        ));
    }
    let ready = validity_ok && gate_ready;
    let (process_artifact_ok, process_artifact_note) =
        check_process_artifact(dir, &mut diagnostics)?;
    let distribution = distribution::inspect(DistributionContext {
        project_root: dir,
        store_root: context.store_root,
        builtin: context.builtin,
    })?;
    diagnostics.extend(distribution.diagnostics.clone());
    finish_plan(
        validity_ok,
        ready,
        process_artifact_ok,
        process_artifact_note,
        Scaffold::None,
        distribution,
        diagnostics,
    )
}

fn checked_in_gates_ready(dir: &Path) -> io::Result<(bool, Vec<Diagnostic>)> {
    let gates_text =
        nopal_core::confined_read::read_utf8(dir, Path::new(".nopal/gates.jsonc"), 1024 * 1024)?
            .ok_or_else(|| {
                io::Error::new(io::ErrorKind::NotFound, "repository gates are missing")
            })?;
    let (config, mut diagnostics) =
        nopal_core::gates::parse_gates(&gates_text, ".nopal/gates.jsonc");
    let Some(config) = config else {
        return Ok((false, diagnostics));
    };

    let workflow_text =
        nopal_core::confined_read::read_utf8(dir, Path::new(".beislid/workflow.md"), 1024 * 1024)?
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "workflow is missing"))?;
    let compiled = nopal_core::beislid_import::compile_text(&workflow_text, ".beislid/workflow.md");
    let beislid_explicit = compiled.modules.get("gates").is_some_and(|value| {
        let (gates, gate_diagnostics) =
            nopal_core::gates::validate_document(value, ".beislid/workflow.md#beislid:gates");
        diagnostics.extend(gate_diagnostics);
        has_unscoped_explicit_pre_pr(&gates)
    });
    diagnostics.extend(compiled.diagnostics);

    let ready = match &config.scaffold {
        None => has_unscoped_explicit_pre_pr(&config) || beislid_explicit,
        Some(provenance) => {
            let nopal_explicit = has_unscoped_explicit_pre_pr(&config);
            if nopal_explicit || beislid_explicit {
                true
            } else {
                let detected = nopal_core::gate_scaffold::inspect(dir)?;
                diagnostics.extend(detected.diagnostics.clone());
                let matches_current_evidence = detected.matches_checked_generated(&gates_text);
                if !matches_current_evidence {
                    diagnostics.push(Diagnostic::error(
                        Code::GateScaffoldDrift,
                        ".nopal/gates.jsonc",
                        "generated gates no longer match current root and declared-workspace evidence; add explicit gates or regenerate from `nopal doctor` evidence",
                    ));
                }
                provenance.readiness == Readiness::Ready
                    && detected.readiness == Readiness::Ready
                    && matches_current_evidence
            }
        }
    };
    let diagnostics_ok = diagnostics
        .iter()
        .all(|diagnostic| diagnostic.severity != Severity::Error);
    Ok((ready && diagnostics_ok, diagnostics))
}

/// Launch has no changed-file set to prove selector coverage.
/// Only explicit proof selected without file evidence can replace generated
/// readiness for the whole repository.
fn has_unscoped_explicit_pre_pr(config: &nopal_core::gates::GatesConfig) -> bool {
    let generated = config.generated_gate_ids();
    nopal_core::selection::select(config, nopal_core::gates::GateStage::PrePr, &[])
        .selected
        .iter()
        .any(|gate| !generated.contains(gate.id.as_str()))
}

fn plan_without_nopal(dir: &Path, context: LaunchContext<'_>) -> io::Result<LaunchPlan> {
    let beislid = dir.join(".beislid");
    if beislid.exists() {
        let path = beislid;
        return Ok(blocked_unconfigured_plan(Diagnostic::error(
            Code::ScaffoldIncomplete,
            path.display().to_string(),
            format!(
                "existing or legacy project state {} is preserved; Nopal will not merge an inferred baseline into it",
                path.display()
            ),
        )));
    }
    if !dir.join(".git").exists() {
        return Ok(blocked_unconfigured_plan(Diagnostic::error(
            Code::ScaffoldIncomplete,
            dir.display().to_string(),
            "first-run scaffolding requires a Git worktree so every generated contract can be checked in",
        )));
    }

    let baseline = scaffold::build_baseline(dir, context.builtin)?;
    let bundle_text = baseline
        .text(distribution::BUNDLE_PATH)
        .ok_or_else(|| io::Error::other("generated baseline omitted its bundle contract"))?;
    let lock_text = baseline
        .text(distribution::LOCK_PATH)
        .ok_or_else(|| io::Error::other("generated baseline omitted its distribution lock"))?;
    let distribution = distribution::inspect_texts(
        DistributionContext {
            project_root: dir,
            store_root: context.store_root,
            builtin: context.builtin,
        },
        bundle_text,
        lock_text,
    )?;
    let mut diagnostics = vec![scaffold_diagnostic(
        false,
        &baseline.source,
        &baseline.files,
    )];
    let gate_plan = baseline.gate_scaffold.clone();
    let gate_ready = gate_plan.readiness == Readiness::Ready;
    let can_scaffold = gate_plan.readiness != Readiness::Blocked;
    if !gate_ready {
        diagnostics.push(Diagnostic::error(
            Code::GateConfigurationRequired,
            ".nopal/gates.jsonc",
            "no evidence-backed validation template was detected; Nopal can create the baseline but will not launch Pi until explicit gates are added",
        ));
    }
    diagnostics.extend(gate_plan.diagnostics.clone());
    diagnostics.extend(distribution.diagnostics.clone());
    let (process_artifact_ok, process_artifact_note) =
        check_process_artifact(dir, &mut diagnostics)?;
    let mut plan = finish_plan(
        true,
        gate_ready,
        process_artifact_ok,
        process_artifact_note,
        Scaffold::None,
        distribution,
        diagnostics,
    )?;
    let can_scaffold = can_scaffold
        && plan.diagnostics.iter().all(|diagnostic| {
            diagnostic.severity != Severity::Error
                || diagnostic.code == Code::GateConfigurationRequired
        });
    plan.gate_scaffold = Some(gate_plan);
    if can_scaffold {
        plan.scaffold = Scaffold::WouldCreate;
        plan.prepared_baseline = Some(baseline);
    }
    Ok(plan)
}

fn finish_plan(
    validity_ok: bool,
    ready: bool,
    process_artifact_ok: bool,
    process_artifact_note: Option<String>,
    scaffold: Scaffold,
    distribution: DistributionReport,
    mut diagnostics: Vec<Diagnostic>,
) -> io::Result<LaunchPlan> {
    if distribution.inherit_ambient.extensions {
        diagnostics.push(Diagnostic::error(
            Code::DistributionPackageInvalid,
            distribution::BUNDLE_PATH,
            "ambient executable Pi extensions are outside the trusted Nopal distribution",
        ));
    }
    diagnostics::sort(&mut diagnostics);
    let ok = validity_ok
        && process_artifact_ok
        && distribution.ok
        && diagnostics
            .iter()
            .all(|diagnostic| diagnostic.severity != Severity::Error);
    let ambient_kinds = distribution.inherit_ambient.kind_names();
    let ambient = distribution.inherit_ambient.is_all();
    let pi_argv = if ok {
        build_pi_argv(&distribution, distribution.inherit_ambient)
    } else {
        Vec::new()
    };
    Ok(LaunchPlan {
        kind: LAUNCH_KIND,
        ok,
        would_exec: ok,
        validity_ok,
        ready,
        ambient,
        ambient_kinds,
        process_artifact_note,
        scaffold,
        gate_scaffold: None,
        prepared_baseline: None,
        bundle: distribution,
        pi_argv,
        diagnostics,
    })
}

fn blocked_unconfigured_plan(diagnostic: Diagnostic) -> LaunchPlan {
    LaunchPlan {
        kind: LAUNCH_KIND,
        ok: false,
        would_exec: false,
        validity_ok: false,
        ready: false,
        ambient: false,
        ambient_kinds: Vec::new(),
        process_artifact_note: None,
        scaffold: Scaffold::None,
        gate_scaffold: None,
        prepared_baseline: None,
        bundle: empty_distribution(vec![diagnostic.clone()]),
        pi_argv: Vec::new(),
        diagnostics: vec![diagnostic],
    }
}

fn empty_distribution(diagnostics: Vec<Diagnostic>) -> DistributionReport {
    DistributionReport {
        kind: "nopal.distribution/v1",
        ok: false,
        inherit_ambient: AmbientInherit::NONE,
        packages: Vec::new(),
        resources: Vec::new(),
        diagnostics,
    }
}

pub fn mark_scaffolded(mut plan: LaunchPlan, baseline: &scaffold::Baseline) -> LaunchPlan {
    plan.scaffold = Scaffold::Created;
    plan.diagnostics
        .push(scaffold_diagnostic(true, &baseline.source, &baseline.files));
    diagnostics::sort(&mut plan.diagnostics);
    plan
}

fn scaffold_diagnostic(
    created: bool,
    source: &ScaffoldSource,
    files: &[scaffold::BaselineFile],
) -> Diagnostic {
    let paths = files
        .iter()
        .map(|file| file.rel_path.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    let verb = if created { "created" } else { "would create" };
    Diagnostic {
        severity: Severity::Info,
        code: Code::ScaffoldDefaults,
        path: ".nopal/nopal.jsonc".to_owned(),
        position: None,
        message: format!(
            "unconfigured Git repository: {verb} complete baseline [{paths}] from {}",
            source.describe()
        ),
    }
}

pub fn summary_line(plan: &LaunchPlan) -> String {
    format!(
        "{}: would_exec={} packages={} resources={} ambient={} scaffold={}",
        plan.kind,
        plan.would_exec,
        plan.bundle.packages.len(),
        plan.bundle.resources.len(),
        plan.ambient,
        plan.scaffold.as_str()
    )
}

pub fn resource_surface_line(plan: &LaunchPlan) -> String {
    let resource_count = plan.bundle.resources.len();
    let ambient_desc = if plan.ambient_kinds.is_empty() {
        "none".to_owned()
    } else if plan.ambient {
        "full".to_owned()
    } else {
        plan.ambient_kinds.join(", ")
    };
    let resource_word = if resource_count == 1 {
        "resource"
    } else {
        "resources"
    };
    format!(
        "nopal: {} locked packages, {resource_count} {resource_word}; ambient: {ambient_desc}; startup: offline",
        plan.bundle.packages.len()
    )
}

fn check_process_artifact(
    dir: &Path,
    diagnostics: &mut Vec<Diagnostic>,
) -> io::Result<(bool, Option<String>)> {
    let artifact_rel = process_artifact::default_artifact_path();
    let actual_text = match std::fs::read_to_string(dir.join(artifact_rel)) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok((
                true,
                Some(format!(
                    "{artifact_rel} not found; export it for provenance with `nopal export process --output {artifact_rel}`"
                )),
            ));
        }
        Err(err) => return Err(err),
    };
    let artifact = process_artifact::build(dir)?;
    let expected_json = process_artifact::artifact_json(&artifact).map_err(io::Error::other)?;
    let report =
        process_artifact::check_report(artifact_rel, &artifact, &expected_json, Some(&actual_text));
    let process_only = report
        .diagnostics
        .into_iter()
        .filter(|diagnostic| {
            matches!(
                diagnostic.code,
                Code::ProcessArtifactDrift
                    | Code::ProcessArtifactParseError
                    | Code::ProcessArtifactRedacted
            )
        })
        .collect::<Vec<_>>();
    let ok = process_only
        .iter()
        .all(|diagnostic| diagnostic.severity != Severity::Error);
    diagnostics.extend(process_only);
    Ok((ok, None))
}

fn build_pi_argv(distribution: &DistributionReport, ambient: AmbientInherit) -> Vec<String> {
    // Pi's offline flag is part of the generated launch plan rather than an
    // environment-only promise, so dry-run and E2E evidence can prove it.
    let mut argv = vec!["--offline".to_owned()];
    if !ambient.extensions {
        argv.push("--no-extensions".to_owned());
    }
    if !ambient.skills {
        argv.push("--no-skills".to_owned());
    }
    if !ambient.prompt_templates {
        argv.push("--no-prompt-templates".to_owned());
    }
    if !ambient.themes {
        argv.push("--no-themes".to_owned());
    }
    for resource in &distribution.resources {
        argv.push(resource.kind.pi_flag().to_owned());
        argv.push(resource.resolved_path.display().to_string());
    }
    argv
}

pub fn launch_toon(plan: &LaunchPlan) -> String {
    let doc = vec![
        ("kind".into(), Value::str(plan.kind)),
        ("ok".into(), Value::Bool(plan.ok)),
        ("would_exec".into(), Value::Bool(plan.would_exec)),
        ("validity_ok".into(), Value::Bool(plan.validity_ok)),
        ("ready".into(), Value::Bool(plan.ready)),
        ("scaffold".into(), Value::str(plan.scaffold.as_str())),
        ("ambient".into(), Value::Bool(plan.ambient)),
        (
            "ambient_kinds".into(),
            Value::Arr(
                plan.ambient_kinds
                    .iter()
                    .map(|kind| Value::str(*kind))
                    .collect(),
            ),
        ),
        (
            "process_artifact_note".into(),
            Value::str(plan.process_artifact_note.as_deref().unwrap_or("-")),
        ),
        ("bundle_ok".into(), Value::Bool(plan.bundle.ok)),
        ("resources".into(), resources_table(&plan.bundle.resources)),
        (
            "pi_argv".into(),
            Value::Arr(plan.pi_argv.iter().map(Value::str).collect()),
        ),
        (
            "diagnostics".into(),
            diagnostics::toon_table(&plan.diagnostics),
        ),
    ];
    toon::encode(&doc)
}

fn resources_table(resources: &[ResolvedResource]) -> Value {
    Value::Table {
        fields: vec![
            "package_id".into(),
            "kind".into(),
            "package_path".into(),
            "resolved_path".into(),
        ],
        rows: resources
            .iter()
            .map(|resource| {
                vec![
                    Value::str(resource.package_id.clone()),
                    Value::str(resource.kind.as_str()),
                    Value::str(resource.package_path.clone()),
                    Value::str(resource.resolved_path.display().to_string()),
                ]
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn context<'a>(temp: &'a tempfile::TempDir) -> LaunchContext<'a> {
        let distribution = temp.path().join("distribution");
        let adapter = distribution.join("extensions/policy-gate");
        fs::create_dir_all(&adapter).unwrap();
        fs::write(adapter.join("index.ts"), "export default 1;\n").unwrap();
        fs::write(
            adapter.join("classifier.ts"),
            "export const classify = 1;\n",
        )
        .unwrap();
        fs::write(adapter.join("guard.ts"), "export const guard = 1;\n").unwrap();
        fs::write(adapter.join("nopal-cli.ts"), "export const cli = 1;\n").unwrap();
        let skill = distribution.join("resources/beislid/skills/kickoff/SKILL.md");
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(skill, "# Kickoff\n").unwrap();
        let leaked = Box::leak(Box::new(distribution));
        LaunchContext {
            store_root: temp.path(),
            builtin: BuiltinDistribution {
                version: "0.3.0",
                root: leaked,
            },
        }
    }

    #[test]
    fn unknown_git_project_plans_complete_scaffold_without_pi_launch() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        let plan = plan(temp.path(), context(&temp)).unwrap();
        assert!(!plan.ok);
        assert_eq!(plan.scaffold, Scaffold::WouldCreate);
        assert_eq!(plan.bundle.packages.len(), 1);
        assert_eq!(plan.bundle.resources.len(), 2);
        assert!(plan.pi_argv.is_empty());
        assert!(
            plan.diagnostics
                .iter()
                .any(|diagnostic| { diagnostic.code == Code::GateConfigurationRequired })
        );
    }

    #[test]
    fn partial_beislid_project_is_preserved_and_blocked() {
        let temp = tempfile::tempdir().unwrap();
        fs::create_dir(temp.path().join(".git")).unwrap();
        fs::create_dir(temp.path().join(".beislid")).unwrap();
        let plan = plan(temp.path(), context(&temp)).unwrap();
        assert!(!plan.ok);
        assert_eq!(plan.scaffold, Scaffold::None);
        assert_eq!(plan.diagnostics[0].code, Code::ScaffoldIncomplete);
    }
}
