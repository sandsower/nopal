//! `nopal.bundle/v1` module validation - the Pi resource bundle `nopal
//! launch` hands off to Pi.
//!
//! Cold: parses `.nopal/bundle.jsonc`, expands `~`/`${ENV}` in each declared
//! resource path, and existence-checks the result. No network, no process
//! spawn, and no version enforcement because versions are recorded metadata.

use std::env;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::config;
use crate::diagnostics::{self, Code, Diagnostic, Severity};

pub const BUNDLE_KIND: &str = "nopal.bundle/v1";

const NOPAL_DIR: &str = ".nopal";
const BUNDLE_FILE: &str = "bundle.jsonc";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceKind {
    Extension,
    Skill,
    PromptTemplate,
    Theme,
}

impl ResourceKind {
    const ALL: [ResourceKind; 4] = [
        ResourceKind::Extension,
        ResourceKind::Skill,
        ResourceKind::PromptTemplate,
        ResourceKind::Theme,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ResourceKind::Extension => "extension",
            ResourceKind::Skill => "skill",
            ResourceKind::PromptTemplate => "prompt_template",
            ResourceKind::Theme => "theme",
        }
    }

    /// The Pi CLI flag that loads one resource of this kind.
    pub fn pi_flag(self) -> &'static str {
        match self {
            ResourceKind::Extension => "-e",
            ResourceKind::Skill => "--skill",
            ResourceKind::PromptTemplate => "--prompt-template",
            ResourceKind::Theme => "--theme",
        }
    }

    fn field_name(self) -> &'static str {
        match self {
            ResourceKind::Extension => "extensions",
            ResourceKind::Skill => "skills",
            ResourceKind::PromptTemplate => "prompt_templates",
            ResourceKind::Theme => "themes",
        }
    }

    /// Parses one `inherit_ambient` array token back to its kind. Tokens are
    /// the same plural names as the bundle's own resource-array field names
    /// (and the `--no-<kind>` Pi flag suffixes), so a token round-trips
    /// exactly what a caller wrote in `.nopal/bundle.jsonc`.
    fn parse_field_name(token: &str) -> Option<ResourceKind> {
        ResourceKind::ALL
            .into_iter()
            .find(|kind| kind.field_name() == token)
    }
}

/// Per-kind ambient inheritance (`inherit_ambient` in `nopal.bundle/v1`).
/// `true`/absent-boolean collapse to [`AmbientInherit::ALL`]/[`AmbientInherit::NONE`];
/// an array of kind tokens sets exactly those kinds. A set (not a single
/// bool) so a bundle can inherit ambient themes while still pinning a
/// hermetic extension list, for example.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub struct AmbientInherit {
    pub extensions: bool,
    pub skills: bool,
    pub prompt_templates: bool,
    pub themes: bool,
}

impl AmbientInherit {
    pub const NONE: Self = Self {
        extensions: false,
        skills: false,
        prompt_templates: false,
        themes: false,
    };

    pub const ALL: Self = Self {
        extensions: true,
        skills: true,
        prompt_templates: true,
        themes: true,
    };

    pub fn inherits(&self, kind: ResourceKind) -> bool {
        match kind {
            ResourceKind::Extension => self.extensions,
            ResourceKind::Skill => self.skills,
            ResourceKind::PromptTemplate => self.prompt_templates,
            ResourceKind::Theme => self.themes,
        }
    }

    fn set(&mut self, kind: ResourceKind, value: bool) {
        match kind {
            ResourceKind::Extension => self.extensions = value,
            ResourceKind::Skill => self.skills = value,
            ResourceKind::PromptTemplate => self.prompt_templates = value,
            ResourceKind::Theme => self.themes = value,
        }
    }

    pub fn is_all(&self) -> bool {
        *self == Self::ALL
    }

    /// Widens `self` with `other`; never narrows (D2: `--with-ambient` is an
    /// upward override of the bundle's own declaration, not a replacement).
    pub fn union(self, other: Self) -> Self {
        Self {
            extensions: self.extensions || other.extensions,
            skills: self.skills || other.skills,
            prompt_templates: self.prompt_templates || other.prompt_templates,
            themes: self.themes || other.themes,
        }
    }

    /// Inherited kinds' field names, in `ResourceKind::ALL` order.
    pub fn kind_names(&self) -> Vec<&'static str> {
        ResourceKind::ALL
            .into_iter()
            .filter(|kind| self.inherits(*kind))
            .map(ResourceKind::field_name)
            .collect()
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolvedResource {
    pub kind: ResourceKind,
    pub source: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    pub declared_path: String,
    pub resolved_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct BundleReport {
    pub kind: &'static str,
    pub ok: bool,
    pub inherit_ambient: AmbientInherit,
    pub resources: Vec<ResolvedResource>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Project-relative path of the bundle file (used in diagnostics and display).
pub fn bundle_rel_path() -> String {
    format!("{NOPAL_DIR}/{BUNDLE_FILE}")
}

fn bundle_path(root: &Path) -> PathBuf {
    root.join(NOPAL_DIR).join(BUNDLE_FILE)
}

/// Load and validate `.nopal/bundle.jsonc` from a project root. A missing
/// bundle fails closed (D10): hermetic launch with no bundle is a
/// misconfiguration, not an empty-but-fine state.
///
/// `root` is absolutized before any resource path is resolved against it.
/// The caller (bare `nopal`) may `chdir` into `root` before handing the
/// resolved paths to Pi's argv; a relative `root` would otherwise make
/// resolution depend on which process ends up reading the path, silently
/// pointing at `<root>/<root>/...` once the cwd changes.
pub fn bundle_report(root: &Path) -> io::Result<BundleReport> {
    let root = std::path::absolute(root)?;
    let root = root.as_path();
    let rel = bundle_rel_path();
    let text = match std::fs::read_to_string(bundle_path(root)) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(BundleReport {
                kind: BUNDLE_KIND,
                ok: false,
                inherit_ambient: AmbientInherit::NONE,
                resources: Vec::new(),
                diagnostics: vec![Diagnostic::error(
                    Code::BundleMissing,
                    rel.clone(),
                    format!("`nopal cli` requires {rel}"),
                )],
            });
        }
        Err(err) => return Err(err),
    };

    Ok(validate_bundle_text(root, &text, &rel))
}

/// Validates already-loaded `nopal.bundle/v1` JSONC `text` against
/// `project_root` - the same parse-then-shape-then-resolve pipeline
/// [`bundle_report`] runs after reading `.nopal/bundle.jsonc` off disk, split
/// out so a second caller can validate content that either isn't on disk yet
/// or lives somewhere other than `<project_root>/.nopal/bundle.jsonc`.
/// `nopal-core::scaffold` is that second caller: a user-level
/// default-bundle template at `~/.config/nopal/bundle-default.jsonc` must be
/// validated, with its resource paths resolved against the *new* repo's
/// root, before a byte of it is copied into a fresh `.nopal/`; reusing this
/// function instead of a second parse-and-check pipeline is what keeps that
/// validation identical to what `bundle_report` would report the moment
/// after the copy lands. `project_root` must already be absolutized by the
/// caller (as [`bundle_report`] does above) - this function does not do it
/// again, since a template caller resolves against a root that may not even
/// have a `.nopal/` directory yet to `chdir` relative to.
pub fn validate_bundle_text(project_root: &Path, text: &str, path: &str) -> BundleReport {
    let value = match config::parse_jsonc(text, path, Code::BundleParseError) {
        Ok(value) => value,
        Err(diagnostic) => {
            return BundleReport {
                kind: BUNDLE_KIND,
                ok: false,
                inherit_ambient: AmbientInherit::NONE,
                resources: Vec::new(),
                diagnostics: vec![diagnostic],
            };
        }
    };

    let (inherit_ambient, resources, mut diags) = validate_document(project_root, &value, path);
    diagnostics::sort(&mut diags);
    let ok = diags.iter().all(|d| d.severity != Severity::Error);
    BundleReport {
        kind: BUNDLE_KIND,
        ok,
        inherit_ambient,
        resources,
        diagnostics: diags,
    }
}

fn validate_document(
    project_root: &Path,
    root: &serde_json::Value,
    path: &str,
) -> (AmbientInherit, Vec<ResolvedResource>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();

    match root.get("version").and_then(|v| v.as_str()) {
        Some(BUNDLE_KIND) => {}
        Some(other) => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("unsupported bundle version {other:?}; expected {BUNDLE_KIND:?}"),
        )),
        None => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("missing string field \"version\"; expected {BUNDLE_KIND:?}"),
        )),
    }

    let inherit_ambient = match root.get("inherit_ambient") {
        None => AmbientInherit::NONE,
        Some(v) => parse_inherit_ambient(v, path, &mut diagnostics),
    };

    let mut resources = Vec::new();
    for kind in ResourceKind::ALL {
        let field = kind.field_name();
        let Some(items) = root.get(field) else {
            continue;
        };
        let Some(items) = items.as_array() else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{field} must be an array of resource entries"),
            ));
            continue;
        };
        for (index, item) in items.iter().enumerate() {
            match validate_entry(project_root, item, kind, &format!("{field}[{index}]"), path) {
                Ok(resource) => resources.push(resource),
                Err(diagnostic) => diagnostics.push(diagnostic),
            }
        }
    }

    (inherit_ambient, resources, diagnostics)
}

/// `inherit_ambient` accepts either a bool (existing all-or-nothing
/// semantics) or an array of kind tokens (per-kind semantics). An unknown
/// token in the array is conservative, not fatal - vocabulary stays additive,
/// so a future kind name a newer bundle declares just isn't
/// inherited by this build instead of failing the whole launch. A value that
/// is neither a bool nor an array of strings is a schema error.
fn parse_inherit_ambient(
    value: &serde_json::Value,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> AmbientInherit {
    if let Some(b) = value.as_bool() {
        return if b {
            AmbientInherit::ALL
        } else {
            AmbientInherit::NONE
        };
    }

    let is_array_of_strings = value
        .as_array()
        .is_some_and(|items| items.iter().all(|item| item.as_str().is_some()));
    let Some(items) = value.as_array().filter(|_| is_array_of_strings) else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "inherit_ambient must be a boolean or an array of kind strings \
             (\"extensions\", \"skills\", \"prompt_templates\", \"themes\")",
        ));
        return AmbientInherit::NONE;
    };

    let mut inherit = AmbientInherit::NONE;
    for item in items {
        // Safe: `is_array_of_strings` already proved every entry is a string.
        let token = item.as_str().unwrap_or_default();
        match ResourceKind::parse_field_name(token) {
            Some(kind) => inherit.set(kind, true),
            None => diagnostics.push(Diagnostic::warning(
                Code::BundleAmbientKindUnknown,
                path,
                format!(
                    "inherit_ambient: unknown kind {token:?}; expected one of \
                     \"extensions\", \"skills\", \"prompt_templates\", \"themes\"; not inherited"
                ),
            )),
        }
    }
    inherit
}

fn validate_entry(
    project_root: &Path,
    value: &serde_json::Value,
    kind: ResourceKind,
    ctx: &str,
    path: &str,
) -> Result<ResolvedResource, Diagnostic> {
    let Some(obj) = value.as_object() else {
        return Err(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx} must be an object"),
        ));
    };
    let Some(source) = obj
        .get("source")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    else {
        return Err(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx}: requires non-empty string field \"source\""),
        ));
    };
    let Some(declared_path) = obj
        .get("path")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    else {
        return Err(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx}: requires non-empty string field \"path\""),
        ));
    };
    let version = obj
        .get("version")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    let resolved_path = resolve_path(project_root, declared_path).map_err(|message| {
        Diagnostic::error(
            Code::BundleResourceMissing,
            path,
            format!("{ctx}: {message}"),
        )
    })?;
    if !resolved_path.exists() {
        return Err(Diagnostic::error(
            Code::BundleResourceMissing,
            path,
            format!(
                "{ctx}: resolved path {} does not exist",
                resolved_path.display()
            ),
        ));
    }

    Ok(ResolvedResource {
        kind,
        source: source.to_owned(),
        version,
        declared_path: declared_path.to_owned(),
        resolved_path,
    })
}

/// Expand `~`/`${ENV}` in a declared path, then anchor a still-relative
/// result to the project root rather than the process's current directory -
/// bundle resolution stays cold and deterministic regardless of invocation cwd.
fn resolve_path(project_root: &Path, declared: &str) -> Result<PathBuf, String> {
    let expanded = expand_path(declared)?;
    if expanded.is_absolute() {
        Ok(expanded)
    } else {
        Ok(project_root.join(expanded))
    }
}

/// Expand a leading `~` and any `${VAR}` tokens. An undefined variable is a
/// resolution failure, not a literal pass-through: a path that can't be
/// expanded can never exist.
fn expand_path(declared: &str) -> Result<PathBuf, String> {
    let mut expanded = String::new();
    let mut rest = declared;

    if let Some(after) = rest.strip_prefix('~') {
        if !after.is_empty() && !after.starts_with('/') {
            // `~user/...` (another user's home directory) is a distinct,
            // unsupported form - expanding it against $HOME would silently
            // resolve to the wrong path instead of failing loudly.
            return Err(format!(
                "\"~user\"-style expansion is not supported (got {declared:?}); use \
                 ${{ENV}} or a project-relative path"
            ));
        }
        let home =
            env::var("HOME").map_err(|_| "~ expansion requires $HOME to be set".to_owned())?;
        expanded.push_str(&home);
        rest = after;
    }

    let mut chars = rest.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '$' && chars.peek() == Some(&'{') {
            chars.next();
            let mut name = String::new();
            let mut closed = false;
            for c in chars.by_ref() {
                if c == '}' {
                    closed = true;
                    break;
                }
                name.push(c);
            }
            if !closed {
                return Err(format!("unterminated \"${{{name}\" in path"));
            }
            let value = env::var(&name).map_err(|_| format!("${{{name}}} is not set"))?;
            expanded.push_str(&value);
        } else {
            expanded.push(c);
        }
    }

    Ok(PathBuf::from(expanded))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_bundle(root: &Path, text: &str) {
        let dir = root.join(NOPAL_DIR);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join(BUNDLE_FILE), text).unwrap();
    }

    #[test]
    fn missing_bundle_file_is_bundle_missing() {
        let temp = tempfile::tempdir().unwrap();
        let report = bundle_report(temp.path()).unwrap();
        assert!(!report.ok);
        assert_eq!(
            report
                .diagnostics
                .iter()
                .map(|d| d.code)
                .collect::<Vec<_>>(),
            vec![Code::BundleMissing]
        );
    }

    #[test]
    fn malformed_jsonc_is_bundle_parse_error() {
        let temp = tempfile::tempdir().unwrap();
        write_bundle(temp.path(), "{ \"version\": ");
        let report = bundle_report(temp.path()).unwrap();
        assert!(!report.ok);
        assert_eq!(report.diagnostics[0].code, Code::BundleParseError);
    }

    #[test]
    fn valid_four_kind_bundle_resolves_all_resources() {
        let temp = tempfile::tempdir().unwrap();
        let resource = temp.path().join("resource.txt");
        fs::write(&resource, "x").unwrap();
        let resource_path = resource.to_str().unwrap();
        let text = format!(
            r#"{{
                "version": "nopal.bundle/v1",
                "extensions": [ {{ "source": "ext-a", "version": "1.0.0", "path": "{resource_path}" }} ],
                "skills": [ {{ "source": "skill-a", "path": "{resource_path}" }} ],
                "prompt_templates": [ {{ "source": "pt-a", "path": "{resource_path}" }} ],
                "themes": [ {{ "source": "theme-a", "path": "{resource_path}" }} ]
            }}"#
        );
        write_bundle(temp.path(), &text);

        let report = bundle_report(temp.path()).unwrap();
        assert!(report.ok, "{:?}", report.diagnostics);
        assert_eq!(report.resources.len(), 4);
        assert!(
            report
                .resources
                .iter()
                .any(|r| r.kind == ResourceKind::Extension)
        );
        assert!(
            report
                .resources
                .iter()
                .any(|r| r.kind == ResourceKind::Skill)
        );
        assert!(
            report
                .resources
                .iter()
                .any(|r| r.kind == ResourceKind::PromptTemplate)
        );
        assert!(
            report
                .resources
                .iter()
                .any(|r| r.kind == ResourceKind::Theme)
        );
    }

    #[test]
    fn relative_path_resolves_against_project_root_not_process_cwd() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("resources");
        fs::create_dir_all(&nested).unwrap();
        fs::write(nested.join("theme.txt"), "x").unwrap();
        write_bundle(
            temp.path(),
            r#"{
                "version": "nopal.bundle/v1",
                "themes": [ { "source": "theme-a", "path": "resources/theme.txt" } ]
            }"#,
        );

        let report = bundle_report(temp.path()).unwrap();
        assert!(report.ok, "{:?}", report.diagnostics);
        assert_eq!(report.resources[0].resolved_path, nested.join("theme.txt"));
    }

    #[test]
    fn unresolvable_path_is_bundle_resource_missing() {
        let temp = tempfile::tempdir().unwrap();
        let text = r#"{
            "version": "nopal.bundle/v1",
            "extensions": [ { "source": "ext-a", "path": "/definitely/not/a/real/path" } ]
        }"#;
        write_bundle(temp.path(), text);

        let report = bundle_report(temp.path()).unwrap();
        assert!(!report.ok);
        assert_eq!(report.diagnostics[0].code, Code::BundleResourceMissing);
    }

    #[test]
    fn tilde_user_form_is_rejected_instead_of_silently_mis_expanded() {
        let temp = tempfile::tempdir().unwrap();
        write_bundle(
            temp.path(),
            r#"{
                "version": "nopal.bundle/v1",
                "extensions": [ { "source": "ext-a", "path": "~alice/skills/foo.md" } ]
            }"#,
        );

        let report = bundle_report(temp.path()).unwrap();
        assert!(!report.ok);
        assert_eq!(report.diagnostics[0].code, Code::BundleResourceMissing);
        assert!(
            report.diagnostics[0].message.contains("not supported"),
            "{:?}",
            report.diagnostics[0]
        );
    }

    #[test]
    fn env_and_home_expansion_resolve_correctly() {
        let temp = tempfile::tempdir().unwrap();
        let nested = temp.path().join("nested");
        fs::create_dir_all(&nested).unwrap();
        let resource = nested.join("skill.txt");
        fs::write(&resource, "x").unwrap();

        // SAFETY: test-only env var, set and removed within this test; no
        // other test reads NOPAL_TEST_BUNDLE_ROOT.
        unsafe {
            env::set_var("NOPAL_TEST_BUNDLE_ROOT", temp.path());
        }
        let text = r#"{
            "version": "nopal.bundle/v1",
            "skills": [ { "source": "skill-a", "path": "${NOPAL_TEST_BUNDLE_ROOT}/nested/skill.txt" } ]
        }"#;
        write_bundle(temp.path(), text);

        let report = bundle_report(temp.path()).unwrap();
        unsafe {
            env::remove_var("NOPAL_TEST_BUNDLE_ROOT");
        }

        assert!(report.ok, "{:?}", report.diagnostics);
        assert_eq!(report.resources[0].resolved_path, resource);
    }

    #[test]
    fn inherit_ambient_true_means_all_four_kinds() {
        let temp = tempfile::tempdir().unwrap();
        write_bundle(
            temp.path(),
            r#"{ "version": "nopal.bundle/v1", "inherit_ambient": true }"#,
        );

        let report = bundle_report(temp.path()).unwrap();
        assert!(report.ok, "{:?}", report.diagnostics);
        assert!(report.inherit_ambient.is_all());
    }

    #[test]
    fn inherit_ambient_false_means_no_kinds() {
        let temp = tempfile::tempdir().unwrap();
        write_bundle(
            temp.path(),
            r#"{ "version": "nopal.bundle/v1", "inherit_ambient": false }"#,
        );

        let report = bundle_report(temp.path()).unwrap();
        assert!(report.ok, "{:?}", report.diagnostics);
        assert_eq!(report.inherit_ambient, AmbientInherit::NONE);
    }

    #[test]
    fn inherit_ambient_absent_means_no_kinds() {
        let temp = tempfile::tempdir().unwrap();
        write_bundle(temp.path(), r#"{ "version": "nopal.bundle/v1" }"#);

        let report = bundle_report(temp.path()).unwrap();
        assert!(report.ok, "{:?}", report.diagnostics);
        assert_eq!(report.inherit_ambient, AmbientInherit::NONE);
    }

    #[test]
    fn inherit_ambient_array_inherits_exactly_the_named_kinds() {
        let temp = tempfile::tempdir().unwrap();
        write_bundle(
            temp.path(),
            r#"{ "version": "nopal.bundle/v1", "inherit_ambient": ["skills", "themes"] }"#,
        );

        let report = bundle_report(temp.path()).unwrap();
        assert!(report.ok, "{:?}", report.diagnostics);
        assert!(!report.inherit_ambient.extensions);
        assert!(report.inherit_ambient.skills);
        assert!(!report.inherit_ambient.prompt_templates);
        assert!(report.inherit_ambient.themes);
        assert!(!report.inherit_ambient.is_all());
    }

    #[test]
    fn inherit_ambient_array_unknown_token_warns_and_is_not_inherited() {
        let temp = tempfile::tempdir().unwrap();
        write_bundle(
            temp.path(),
            r#"{ "version": "nopal.bundle/v1", "inherit_ambient": ["skills", "widgets"] }"#,
        );

        let report = bundle_report(temp.path()).unwrap();
        assert!(report.ok, "{:?}", report.diagnostics);
        assert!(report.inherit_ambient.skills);
        assert_eq!(
            report.inherit_ambient,
            AmbientInherit {
                skills: true,
                ..AmbientInherit::NONE
            }
        );
        assert_eq!(
            report
                .diagnostics
                .iter()
                .map(|d| d.code)
                .collect::<Vec<_>>(),
            vec![Code::BundleAmbientKindUnknown]
        );
        assert_eq!(
            report.diagnostics[0].severity,
            crate::diagnostics::Severity::Warning
        );
    }

    #[test]
    fn inherit_ambient_malformed_value_is_field_invalid_error() {
        let temp = tempfile::tempdir().unwrap();
        write_bundle(
            temp.path(),
            r#"{ "version": "nopal.bundle/v1", "inherit_ambient": "yes" }"#,
        );

        let report = bundle_report(temp.path()).unwrap();
        assert!(!report.ok);
        assert_eq!(report.diagnostics[0].code, Code::FieldInvalid);
        assert_eq!(report.inherit_ambient, AmbientInherit::NONE);
    }

    #[test]
    fn inherit_ambient_array_with_non_string_entry_is_field_invalid_error() {
        let temp = tempfile::tempdir().unwrap();
        write_bundle(
            temp.path(),
            r#"{ "version": "nopal.bundle/v1", "inherit_ambient": ["skills", 5] }"#,
        );

        let report = bundle_report(temp.path()).unwrap();
        assert!(!report.ok);
        assert_eq!(report.diagnostics[0].code, Code::FieldInvalid);
        assert_eq!(report.inherit_ambient, AmbientInherit::NONE);
    }

    #[test]
    fn empty_bundle_with_no_resources_is_ok() {
        let temp = tempfile::tempdir().unwrap();
        write_bundle(temp.path(), r#"{ "version": "nopal.bundle/v1" }"#);

        let report = bundle_report(temp.path()).unwrap();
        assert!(report.ok, "{:?}", report.diagnostics);
        assert_eq!(report.resources.len(), 0);
    }
}
