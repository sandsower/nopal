//! `nopal.launch/v1` plan - the cold gates `nopal cli` runs before handing
//! off to Pi.
//!
//! `plan` never execs and never spawns Pi; it is both the `--dry-run` payload
//! and the pre-flight `main.rs` runs before a real handoff. This split is
//! what keeps tests hermetic (design D-notes: "tests never spawn Pi").

use std::io;
use std::path::Path;

use nopal_core::bundle::{self, AmbientInherit, BundleReport, ResolvedResource};
use nopal_core::diagnostics::{self, Code, Diagnostic, Severity};
use nopal_core::discover;
use nopal_core::process_artifact;
use nopal_core::scaffold::{self, ScaffoldSource};
use nopal_core::toon::{self, Value};
use nopal_core::validate::{self, Validation};

pub const LAUNCH_KIND: &str = "nopal.launch/v1";

/// Where a launch plan stands relative to `.nopal/` scaffolding.
/// Replaces the former `zero_config: bool` field: that boolean could only
/// distinguish "used in-memory defaults" from "read real files", which
/// collapsed the dry-run preview and the post-write confirmation into the
/// same value even though a caller needs to tell them apart (one must
/// never write, the other already has).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scaffold {
    /// The `.nopal/` directory exists (fully configured, partially
    /// configured, or even empty - the fail-closed D10 diagnostics apply to
    /// the incomplete cases); scaffold never touches an existing `.nopal/`.
    None,
    /// No `.nopal/` directory at all; a real (non-dry-run) launch would
    /// create it and write both default files before exec-ing Pi. Reported
    /// by `--dry-run` and by `plan_unconfigured` generally - never writes
    /// anything itself.
    WouldCreate,
    /// No `.nopal/` directory existed before this launch;
    /// `scaffold::write_defaults` has already created it with both default
    /// files and the plan was re-validated against them.
    Created,
}

impl Scaffold {
    pub fn as_str(self) -> &'static str {
        match self {
            Scaffold::None => "none",
            Scaffold::WouldCreate => "would_create",
            Scaffold::Created => "created",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct LaunchPlan {
    pub kind: &'static str,
    pub ok: bool,
    pub would_exec: bool,
    pub validity_ok: bool,
    pub ready: bool,
    /// `true` only when all four resource kinds are inherited from ambient
    /// Pi state; kept as a plain bool for backward compatibility. See
    /// `ambient_kinds` for the per-kind breakdown.
    pub ambient: bool,
    /// Field names (`"extensions"`, `"skills"`, `"prompt_templates"`,
    /// `"themes"`) of the resource kinds inherited from ambient Pi state,
    /// after folding in `--with-ambient` (which only ever widens this set).
    pub ambient_kinds: Vec<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub process_artifact_note: Option<String>,
    /// `.nopal/` scaffold status for this plan. See `Scaffold`
    /// and the `scaffold_defaults` diagnostic for the human-readable note.
    pub scaffold: Scaffold,
    pub bundle: BundleReport,
    pub pi_argv: Vec<String>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Run the cold launch gates: manifest/module validity (hard), readiness
/// (soft, note only), process-artifact drift when a snapshot exists (hard),
/// and bundle resolution (hard). Never execs and never writes.
///
/// `dir` must already be the discovered project root (`discover::
/// project_root`), not the raw `--dir` value - callers resolve that once,
/// centrally.
///
/// A repo where the `.nopal/` directory does not exist at all is
/// unconfigured, not misconfigured: new users should not need to hand-author
/// these files, so this branches to
/// [`plan_unconfigured`] instead of the two fail-closed `*_missing`
/// diagnostics below. Scaffold only ever creates a brand-new `.nopal/`
/// directory. A `.nopal/` that already exists -
/// even with neither manifest nor bundle inside, e.g. part-populated with
/// other module files - takes the normal path below, so the standard
/// `manifest_missing`/`bundle_missing` fail-closed diagnostics fire and a
/// real launch never writes into a directory the user may have started
/// filling. Since discovery (`discover::project_root`) anchors on an
/// existing `.nopal/` directory, this also guarantees scaffold can only
/// land at the git toplevel (or the non-git starting dir). Exactly one of
/// the two files present still fails closed - that is a
/// partially-configured repo, and D10 in `bundle.rs` stands for it.
///
/// `config_dir` is the already-resolved user-level config directory:
/// `${NOPAL_CONFIG_DIR:-$HOME/.config/nopal}`, computed once by the caller -
/// see `main::resolve_config_dir`) that [`plan_unconfigured`] consults for a
/// default-bundle template. It is unused once `.nopal/` exists, since a
/// configured repo's bundle comes from its own `.nopal/bundle.jsonc`, never
/// the template.
pub fn plan(dir: &Path, with_ambient: bool, config_dir: Option<&Path>) -> io::Result<LaunchPlan> {
    let nopal_dir_present = dir.join(discover::NOPAL_DIR).exists();
    if !nopal_dir_present {
        return plan_unconfigured(dir, with_ambient, config_dir);
    }

    let validation = validate::validate(dir)?;
    let validity_ok = required_scope_ok(&validation);
    let ready = validation.ok();
    let mut diagnostics = validation.diagnostics.clone();

    let (process_artifact_ok, process_artifact_note) =
        check_process_artifact(dir, &mut diagnostics)?;

    let bundle_report = bundle::bundle_report(dir)?;
    diagnostics.extend(bundle_report.diagnostics.clone());
    diagnostics::sort(&mut diagnostics);

    let ok = validity_ok && process_artifact_ok && bundle_report.ok;
    // `--with-ambient` only ever widens the bundle's own declaration (D2:
    // union, never narrows) - a bundle that pins `["skills"]` plus
    // `--with-ambient` still inherits all four, not just skills.
    let with_ambient_kinds = if with_ambient {
        AmbientInherit::ALL
    } else {
        AmbientInherit::NONE
    };
    let ambient_kinds = bundle_report.inherit_ambient.union(with_ambient_kinds);
    let ambient = ambient_kinds.is_all();
    let pi_argv = if ok {
        build_pi_argv(&bundle_report, ambient_kinds)
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
        ambient_kinds: ambient_kinds.kind_names(),
        process_artifact_note,
        scaffold: Scaffold::None,
        bundle: bundle_report,
        pi_argv,
        diagnostics,
    })
}

/// Dry-run/preflight view of an unconfigured repo. The `.nopal/` directory does
/// not exist at all, so there is nothing to parse or fail closed over for
/// the manifest half. Synthesizes the same shape a configured launch would
/// reach - manifest profile `"minimal"` (no required modules,
/// `validate::validate` would report `ready`) - entirely in memory; no file
/// is read or written here except the optional bundle-default template probe
/// inside `scaffold::resolve_bundle_scaffold`.
///
/// The bundle half is resolved through the exact same
/// `scaffold::resolve_bundle_scaffold(dir, config_dir)` call [`crate::main::
/// dispatch_launch`]'s real write later makes, so a `--dry-run` preview and
/// the launch that follows it can never disagree about which source (user
/// template vs. built-in hermetic default) would be used - see that
/// function's doc comment. A template that fails validation makes the whole
/// plan `ok: false` with diagnostics naming the template path
/// ([`template_invalid_plan`]), the dry-run equivalent of `write_defaults`
/// refusing to write anything.
///
/// This is no longer the path a real (non-dry-run) launch takes against an
/// unconfigured repo: `dispatch_launch` in `nopal-cli/src/main.rs`
/// calls `scaffold::write_defaults` first and re-plans against the files it
/// wrote, so only `--dry-run` (and any real launch that turns out `!ok`,
/// e.g. the process-artifact or template-validity gates failing) ever
/// returns a plan built here.
fn plan_unconfigured(
    dir: &Path,
    with_ambient: bool,
    config_dir: Option<&Path>,
) -> io::Result<LaunchPlan> {
    let (bundle_report, source) = match scaffold::resolve_bundle_scaffold(dir, config_dir)? {
        scaffold::BundleScaffoldOutcome::Ready(ready) => (ready.report, ready.source),
        scaffold::BundleScaffoldOutcome::TemplateInvalid {
            path: _,
            diagnostics,
        } => {
            return Ok(template_invalid_plan(diagnostics));
        }
    };

    let mut diagnostics = vec![scaffold_defaults_diagnostic(false, &source)];
    // Carry the template report's non-error diagnostics (e.g. an unknown
    // ambient-kind warning) into the plan, mirroring the configured branch's
    // `extend` - otherwise a TOON dry-run hides a warning that the very next
    // real launch (re-planned through the configured branch) will surface.
    diagnostics.extend(bundle_report.diagnostics.clone());

    let (process_artifact_ok, process_artifact_note) =
        check_process_artifact(dir, &mut diagnostics)?;
    diagnostics::sort(&mut diagnostics);

    // `resolve_bundle_scaffold`'s `Ready` variant only ever carries an `ok`
    // report (an invalid template took the early return above instead), so
    // the bundle gate itself never fails this branch - only the
    // process-artifact gate can.
    let ok = process_artifact_ok;
    let with_ambient_kinds = if with_ambient {
        AmbientInherit::ALL
    } else {
        AmbientInherit::NONE
    };
    let ambient_kinds = bundle_report.inherit_ambient.union(with_ambient_kinds);
    let ambient = ambient_kinds.is_all();
    let pi_argv = if ok {
        build_pi_argv(&bundle_report, ambient_kinds)
    } else {
        Vec::new()
    };

    Ok(LaunchPlan {
        kind: LAUNCH_KIND,
        ok,
        would_exec: ok,
        validity_ok: true,
        ready: true,
        ambient,
        ambient_kinds: ambient_kinds.kind_names(),
        process_artifact_note,
        scaffold: Scaffold::WouldCreate,
        bundle: bundle_report,
        pi_argv,
        diagnostics,
    })
}

/// The `ok: false` plan for an unconfigured repo whose default-bundle
/// template failed validation: a real launch must not write
/// anything - not the manifest, not a hermetic-fallback bundle - so this
/// reports `scaffold: WouldCreate` (a real launch *would* attempt to
/// scaffold) alongside `ok: false`/`would_exec: false`, matching the shape
/// `dispatch_launch` already expects from any other failed plan. The
/// process-artifact gate is deliberately skipped here: there is no bundle to
/// launch with regardless of what it would say.
fn template_invalid_plan(diagnostics: Vec<Diagnostic>) -> LaunchPlan {
    let mut diagnostics = diagnostics;
    diagnostics::sort(&mut diagnostics);
    let bundle_report = BundleReport {
        kind: bundle::BUNDLE_KIND,
        ok: false,
        inherit_ambient: AmbientInherit::NONE,
        resources: Vec::new(),
        diagnostics: diagnostics.clone(),
    };
    LaunchPlan {
        kind: LAUNCH_KIND,
        ok: false,
        would_exec: false,
        validity_ok: true,
        ready: true,
        ambient: false,
        ambient_kinds: Vec::new(),
        process_artifact_note: None,
        scaffold: Scaffold::WouldCreate,
        bundle: bundle_report,
        pi_argv: Vec::new(),
        diagnostics,
    }
}

/// Info-severity note recorded whenever a plan touches scaffolding, so
/// dry-run output and field logs show that defaults were (or would be)
/// written instead of read from a hand-authored `.nopal/`. `created`
/// selects between the dry-run/preflight wording and the post-write
/// confirmation wording; the set of paths is the same either way. `source`
/// names where the bundle half came from - a user-level template or
/// nopal's built-in hermetic default - so the message never has to say
/// "full ambient inheritance" again now that that is no longer always true.
fn scaffold_defaults_diagnostic(created: bool, source: &ScaffoldSource) -> Diagnostic {
    let manifest_rel = discover::manifest_rel_path();
    let bundle_rel = bundle::bundle_rel_path();
    let provenance = source.describe();
    let message = if created {
        format!(
            "no {manifest_rel} or {bundle_rel} found; created both with minimal defaults \
             (profile \"minimal\", bundle {provenance}) before launching"
        )
    } else {
        format!(
            "no {manifest_rel} or {bundle_rel} found; a real launch would create both with \
             minimal defaults (profile \"minimal\", bundle {provenance})"
        )
    };
    Diagnostic {
        severity: Severity::Info,
        code: Code::ScaffoldDefaults,
        path: manifest_rel,
        position: None,
        message,
    }
}

/// Stamps a freshly re-validated `LaunchPlan` as post-scaffold:
/// the caller (`dispatch_launch` in `nopal-cli/src/main.rs`) calls
/// `scaffold::write_defaults` against an unconfigured repo, then re-runs
/// `plan` against the files it just wrote. That re-plan takes the normal
/// (now-configured) branch of `plan` above, which reports `scaffold: None`
/// and carries no scaffold diagnostic - both true of a repo that has
/// *always* been configured, but not of one this launch just scaffolded.
/// This corrects both: `scaffold` becomes `Created` and the info diagnostic
/// (with the created-paths wording) is folded in and re-sorted. `source`
/// is `write_defaults`'s own `Scaffolded::source` - the caller
/// passes through what actually got written, not a re-derived guess.
pub fn mark_scaffolded(mut plan: LaunchPlan, source: &ScaffoldSource) -> LaunchPlan {
    plan.scaffold = Scaffold::Created;
    plan.diagnostics
        .push(scaffold_defaults_diagnostic(true, source));
    diagnostics::sort(&mut plan.diagnostics);
    plan
}

/// Gate 1 (hard): a bad manifest or a *required* module missing/broken
/// blocks launch. A schema problem confined to an optional-but-present
/// module is a readiness signal only (`ready`), never a launch blocker
/// (design D4). `Validation::ok()` alone can't tell these apart - it flags
/// schema errors in any present module regardless of whether the active
/// profile requires it - so this walks `modules[].required` and matches
/// diagnostics by the module/manifest path they're attributed to.
fn required_scope_ok(validation: &Validation) -> bool {
    let manifest_path = nopal_core::discover::manifest_rel_path();
    let required_paths: Vec<String> = validation
        .modules
        .iter()
        .filter(|m| m.required)
        .map(|m| nopal_core::discover::module_rel_path(m.module))
        .collect();
    !validation.diagnostics.iter().any(|d| {
        d.severity == Severity::Error
            && (d.path == manifest_path || required_paths.contains(&d.path))
    })
}

/// One-line provenance summary emitted to stderr immediately before exec.
/// Opt-in via `--verbose` (`dispatch_launch` in `nopal-cli/src/main.rs`) -
/// unlike [`resource_surface_line`] below, which is always-on.
pub fn summary_line(plan: &LaunchPlan) -> String {
    format!(
        "{}: would_exec={} resources={} ambient={} scaffold={}",
        plan.kind,
        plan.would_exec,
        plan.bundle.resources.len(),
        plan.ambient,
        plan.scaffold.as_str()
    )
}

/// Always-on, non-verbose stderr line naming the exact resource surface a
/// real launch is about to hand Pi: pinned-resource count and which
/// kinds (if any) inherit from ambient state. Printed on every real launch
/// immediately before `exec_pi`, never gated behind `--verbose` like
/// [`summary_line`] - a hermetic-by-default scaffold fallback is easy to
/// mistake for "nothing configured yet" without some
/// always-visible confirmation of what is actually about to load.
pub fn resource_surface_line(plan: &LaunchPlan) -> String {
    let resource_count = plan.bundle.resources.len();
    let ambient_kinds = &plan.ambient_kinds;

    if resource_count == 0 && ambient_kinds.is_empty() {
        return "nopal: hermetic launch - no ambient, no pinned resources".to_owned();
    }
    if plan.ambient && resource_count == 0 {
        return "nopal: full ambient inheritance; no pinned resources".to_owned();
    }

    let resource_word = if resource_count == 1 {
        "resource"
    } else {
        "resources"
    };
    let ambient_desc = if ambient_kinds.is_empty() {
        "none".to_owned()
    } else if plan.ambient {
        "full".to_owned()
    } else {
        ambient_kinds.join(", ")
    };
    format!("nopal: {resource_count} pinned {resource_word}; ambient: {ambient_desc}")
}

/// Gate 3: only checked when `.nopal/process-artifact.json` exists. A
/// missing artifact is the normal case (never committed) and is a
/// non-blocking note, not drift (design D3).
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

    // `report.diagnostics` also carries `artifact.diagnostics` (re-derived
    // manifest/module validation), which `validation_report` already
    // contributed above; only the process-artifact-specific codes are new.
    // `ProcessArtifactRedacted` is a warning (secret-looking value found) -
    // surfaced for visibility but never blocking, unlike drift/parse-error.
    let process_only: Vec<Diagnostic> = report
        .diagnostics
        .into_iter()
        .filter(|d| {
            matches!(
                d.code,
                Code::ProcessArtifactDrift
                    | Code::ProcessArtifactParseError
                    | Code::ProcessArtifactRedacted
            )
        })
        .collect();
    let ok = process_only.iter().all(|d| d.severity != Severity::Error);
    diagnostics.extend(process_only);
    Ok((ok, None))
}

/// Hermetic by default, per kind: each of the four `--no-*` flags disables
/// ambient discovery for its own resource kind unless that kind is in
/// `ambient`, then every pinned resource loads through its explicit-path
/// flag regardless - pi treats explicit `-e`/`--skill`/etc. paths as
/// additive even alongside `--no-*` for that same kind.
fn build_pi_argv(bundle: &BundleReport, ambient: AmbientInherit) -> Vec<String> {
    let mut argv = Vec::new();
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
    for resource in &bundle.resources {
        argv.push(resource.kind.pi_flag().to_owned());
        argv.push(resource.resolved_path.display().to_string());
    }
    argv
}

pub fn launch_toon(plan: &LaunchPlan) -> String {
    let doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(plan.kind)),
        ("ok".into(), Value::Bool(plan.ok)),
        ("would_exec".into(), Value::Bool(plan.would_exec)),
        ("validity_ok".into(), Value::Bool(plan.validity_ok)),
        ("ready".into(), Value::Bool(plan.ready)),
        ("scaffold".into(), Value::str(plan.scaffold.as_str())),
        ("ambient".into(), Value::Bool(plan.ambient)),
        (
            "ambient_kinds".into(),
            Value::Arr(plan.ambient_kinds.iter().map(|k| Value::str(*k)).collect()),
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
        fields: vec!["kind".into(), "source".into(), "resolved_path".into()],
        rows: resources
            .iter()
            .map(|r| {
                vec![
                    Value::str(r.kind.as_str()),
                    Value::str(r.source.clone()),
                    Value::str(r.resolved_path.display().to_string()),
                ]
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plan_with(resources: usize, ambient_kinds: Vec<&'static str>) -> LaunchPlan {
        let bundle = BundleReport {
            kind: bundle::BUNDLE_KIND,
            ok: true,
            inherit_ambient: AmbientInherit::NONE,
            resources: (0..resources)
                .map(|i| ResolvedResource {
                    kind: bundle::ResourceKind::Skill,
                    source: format!("res-{i}"),
                    version: None,
                    declared_path: format!("res-{i}.md"),
                    resolved_path: format!("/tmp/res-{i}.md").into(),
                })
                .collect(),
            diagnostics: Vec::new(),
        };
        LaunchPlan {
            kind: LAUNCH_KIND,
            ok: true,
            would_exec: true,
            validity_ok: true,
            ready: true,
            ambient: ambient_kinds.len() == 4,
            ambient_kinds,
            process_artifact_note: None,
            scaffold: Scaffold::None,
            bundle,
            pi_argv: Vec::new(),
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn resource_surface_line_hermetic_launch() {
        let plan = plan_with(0, Vec::new());
        assert_eq!(
            resource_surface_line(&plan),
            "nopal: hermetic launch - no ambient, no pinned resources"
        );
    }

    #[test]
    fn resource_surface_line_full_ambient_no_pins() {
        let plan = plan_with(
            0,
            vec!["extensions", "skills", "prompt_templates", "themes"],
        );
        assert_eq!(
            resource_surface_line(&plan),
            "nopal: full ambient inheritance; no pinned resources"
        );
    }

    #[test]
    fn resource_surface_line_pinned_resources_with_partial_ambient() {
        let plan = plan_with(10, vec!["skills"]);
        assert_eq!(
            resource_surface_line(&plan),
            "nopal: 10 pinned resources; ambient: skills"
        );
    }

    #[test]
    fn resource_surface_line_singular_resource_word() {
        let plan = plan_with(1, Vec::new());
        assert_eq!(
            resource_surface_line(&plan),
            "nopal: 1 pinned resource; ambient: none"
        );
    }
}
