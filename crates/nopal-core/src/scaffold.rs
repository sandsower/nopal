//! Silent auto-scaffold of `.nopal/` defaults with a user-level bundle
//! template and a hermetic fallback.
//!
//! Supersedes the former zero-config in-memory synthesis: a real (not
//! `--dry-run`) bare `nopal` launch against a repo with no `.nopal/`
//! directory at all at the discovered project root
//! (`discover::project_root`) writes both files before handing off to Pi -
//! no prompt. An existing `.nopal/` is never written into, even when both
//! config files are absent: that state fails closed through the ordinary
//! missing-file diagnostics instead.
//! Writing real files, rather than synthesizing a
//! plan in memory and never persisting it, means the second and every
//! later launch sees an ordinary configured repo: there is no permanent
//! "first run" branch in the launch gates to keep in sync with the
//! configured path.
//!
//! The caller (`nopal-cli`'s `dispatch_launch`) re-runs the normal launch
//! plan against the files this writes rather than trusting its own output;
//! that re-validation is the actual gate. A scaffold that assumed its own
//! output was valid without re-parsing it through `config.rs`/`bundle.rs`
//! would drift the moment either format changes.
//!
//! The scaffolded manifest is always the same minimal `profile: "minimal"`
//! constant. The scaffolded *bundle* has two possible sources through
//! [`ScaffoldSource`]: a user-level default
//! template at `${NOPAL_CONFIG_DIR:-$HOME/.config/nopal}/bundle-default.jsonc`,
//! copied verbatim (comments and all) when it exists and validates; nopal's
//! own built-in hermetic constant otherwise. Neither this module nor
//! `nopal-cli`'s `launch.rs` reads `NOPAL_CONFIG_DIR` itself - the resolved
//! config directory is threaded in as `config_dir: Option<&Path>` from the
//! CLI boundary (`nopal-cli::main::resolve_config_dir`), so unit tests here
//! can drive template lookup with an explicit temp directory instead of
//! mutating process environment (see the module tests below) and can never
//! accidentally pick up a real `~/.config/nopal/bundle-default.jsonc` on the
//! machine running the tests.

use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};

use crate::bundle::{self, BundleReport};
use crate::diagnostics::{Code, Diagnostic, Severity};
use crate::discover;
use crate::run_ledger_store::token_hex;

const MANIFEST_DEFAULTS: &str = "{\n  \
    // Created by nopal on first launch in an unconfigured repo.\n  \
    \"version\": \"nopal.project/v1\",\n  \
    \"profile\": \"minimal\"\n\
}\n";

/// Built-in hermetic bundle default. A scaffolded repo with no user-level
/// template inherits nothing from ambient Pi state by default because silent
/// full ambient inheritance on first launch was surprising. A
/// hermetic launch that fails closed to zero resources is the safer
/// unfamiliar-repo default, and the doc comment baked into the file itself
/// tells the reader how to opt back in (`inherit_ambient: true`/an array, or
/// pin resources) or how to set a standing user default via the template
/// this module also looks up.
const BUNDLE_DEFAULTS: &str = "{\n  \
    // Created by nopal on first launch. Hermetic by default: no ambient pi\n  \
    // resources are inherited. Set \"inherit_ambient\": true (or a list like\n  \
    // [\"skills\"]) or pin resources explicitly to opt in. A user-wide default\n  \
    // for new repos can live at ~/.config/nopal/bundle-default.jsonc.\n  \
    \"version\": \"nopal.bundle/v1\",\n  \
    \"inherit_ambient\": false\n\
}\n";

/// User-level default-bundle template filename, looked up under the caller-
/// resolved config dir.
const TEMPLATE_FILE: &str = "bundle-default.jsonc";

/// Where a scaffolded `.nopal/bundle.jsonc`'s content came from.
/// Carried on [`Scaffolded`] (a completed write) and folded into
/// `nopal-cli::launch`'s `scaffold_defaults` diagnostic and always-on launch
/// notice so an operator can tell "this repo got my standing template" from
/// "this repo got nopal's stock hermetic default" without diffing the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaffoldSource {
    /// Copied verbatim from the user-level template at this path.
    Template(PathBuf),
    /// No template found (or no config dir at all); nopal's own
    /// [`BUNDLE_DEFAULTS`] constant was used.
    BuiltinHermetic,
}

impl ScaffoldSource {
    /// Human-readable provenance fragment shared by every surface that
    /// reports where a scaffolded bundle came from, so the wording can never
    /// drift between the `scaffold_defaults` diagnostic and the always-on
    /// stderr notice (`nopal-cli::main::dispatch_launch`).
    pub fn describe(&self) -> String {
        match self {
            ScaffoldSource::Template(path) => format!("from {}", path.display()),
            ScaffoldSource::BuiltinHermetic => "built-in hermetic defaults".to_owned(),
        }
    }
}

/// Result of a completed [`write_defaults`] call: the repo-relative paths
/// written, in write order, and where the bundle half's content came from.
#[derive(Debug, Clone)]
pub struct Scaffolded {
    pub rel_paths: Vec<String>,
    pub source: ScaffoldSource,
}

/// A resolved, already-validated bundle scaffold: the exact bytes
/// `write_defaults` would write to `.nopal/bundle.jsonc`, the source they
/// came from, and the parsed report (so a dry-run caller - `plan_unconfigured`
/// in `nopal-cli::launch` - doesn't have to re-parse text it already has).
#[derive(Debug, Clone)]
pub struct BundleScaffold {
    pub source: ScaffoldSource,
    pub text: String,
    pub report: BundleReport,
}

/// What [`resolve_bundle_scaffold`] found. A missing template falls back to
/// the hermetic default and returns [`BundleScaffold::Ready`]. A template
/// that exists but fails validation is an error. The no-silent-fallback
/// rule gives that case its own variant rather than
/// quietly collapsing into the hermetic default.
#[derive(Debug, Clone)]
pub enum BundleScaffoldOutcome {
    Ready(BundleScaffold),
    /// The template at `path` exists but failed `bundle::validate_bundle_text`;
    /// `diagnostics` are that failure's diagnostics, retargeted (`path` and
    /// `message` rewritten) to name the template file rather than the
    /// not-yet-written `.nopal/bundle.jsonc` the raw validation ran against.
    TemplateInvalid {
        path: PathBuf,
        diagnostics: Vec<Diagnostic>,
    },
}

/// Looks up `<config_dir>/bundle-default.jsonc` and validates it exactly as
/// `bundle::bundle_report` would once it is actually sitting at
/// `<root>/.nopal/bundle.jsonc` - pinned resource paths resolve against
/// `root` (the repo being scaffolded), not the template's own location, so
/// template authors anchor pins with `~`/absolute paths or `${ENV}` tokens
/// (see the README's Launch section). `root` must already be absolutized by
/// the caller, matching `bundle::validate_bundle_text`'s own contract.
///
/// `config_dir: None` (no `NOPAL_CONFIG_DIR` and no resolvable `$HOME` at
/// the CLI boundary) skips the lookup entirely and goes straight to the
/// hermetic default - there is nowhere to look.
///
/// Shared by both `nopal-cli::launch::plan_unconfigured` (dry-run/preflight
/// synthesis) and [`write_defaults`] (the real write): both must resolve the
/// *same* source for the *same* repo, or a `--dry-run` preview could show a
/// template-sourced bundle while the real launch that follows it writes the
/// hermetic default (or vice versa) - see this module's top doc comment.
pub fn resolve_bundle_scaffold(
    root: &Path,
    config_dir: Option<&Path>,
) -> io::Result<BundleScaffoldOutcome> {
    let rel = bundle::bundle_rel_path();

    let Some(config_dir) = config_dir else {
        return Ok(hermetic_scaffold(root, &rel));
    };
    let template_path = config_dir.join(TEMPLATE_FILE);
    let text = match fs::read_to_string(&template_path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(hermetic_scaffold(root, &rel));
        }
        // Any other read failure (EACCES, a directory at the template path)
        // fails closed, but the raw io error alone never says WHICH file
        // broke - name the template so the operator can act on it.
        Err(err) => {
            return Err(io::Error::new(
                err.kind(),
                format!(
                    "reading user default-bundle template {}: {err}",
                    template_path.display()
                ),
            ));
        }
    };

    let report = bundle::validate_bundle_text(root, &text, &rel);
    if report.ok {
        Ok(BundleScaffoldOutcome::Ready(BundleScaffold {
            source: ScaffoldSource::Template(template_path),
            text,
            report,
        }))
    } else {
        Ok(BundleScaffoldOutcome::TemplateInvalid {
            diagnostics: retarget_to_template(&template_path, report),
            path: template_path,
        })
    }
}

fn hermetic_scaffold(root: &Path, rel: &str) -> BundleScaffoldOutcome {
    BundleScaffoldOutcome::Ready(BundleScaffold {
        source: ScaffoldSource::BuiltinHermetic,
        text: BUNDLE_DEFAULTS.to_owned(),
        report: bundle::validate_bundle_text(root, BUNDLE_DEFAULTS, rel),
    })
}

/// Rewrites a template validation report's diagnostics to point at the
/// template's real filesystem path instead of the target repo's
/// `.nopal/bundle.jsonc` (which does not exist yet - nothing has been
/// written there), and prepends one summary [`Code::ScaffoldTemplateInvalid`]
/// diagnostic so a consumer that only reads the first/leading diagnostic
/// still gets the "the template is invalid, nothing was written, here's the
/// path" headline. `position` (line/column into the template's own text) on
/// the retargeted per-field diagnostics is left untouched; only their `path`
/// and `message` change, since the whole point of surfacing these
/// underneath the summary is telling the operator exactly what in the
/// template to go fix.
fn retarget_to_template(path: &Path, report: BundleReport) -> Vec<Diagnostic> {
    let display = path.display().to_string();
    let summary = Diagnostic::error(
        Code::ScaffoldTemplateInvalid,
        display.clone(),
        format!(
            "user default-bundle template {display} failed validation; nothing was written \
             (see the accompanying diagnostics for detail)"
        ),
    );
    let mut diagnostics = vec![summary];
    diagnostics.extend(report.diagnostics.into_iter().map(|d| Diagnostic {
        path: display.clone(),
        message: format!("user default-bundle template {display}: {}", d.message),
        ..d
    }));
    diagnostics
}

/// Writes `.nopal/nopal.jsonc` and `.nopal/bundle.jsonc` with minimal
/// defaults under `root`, creating `.nopal/` if it does not already exist.
/// The manifest is always [`MANIFEST_DEFAULTS`]; the bundle's content comes
/// from [`resolve_bundle_scaffold`] - a verbatim copy of the user's template
/// when one validates, [`BUNDLE_DEFAULTS`] otherwise.
///
/// Refuses to overwrite: an existing file at either destination returns
/// `ErrorKind::AlreadyExists` naming the path, before anything is written.
/// The commit step itself is also exclusive (`hard_link`, below), so even a
/// file that appears *between* that pre-check and the commit fails the
/// launch instead of getting its content silently replaced - the pre-check
/// exists for the readable error message, not for the guarantee.
///
/// An invalid template also fails closed *before* anything is written -
/// including the manifest half, which has nothing to do with the bundle:
/// silently falling back to the hermetic default is forbidden, and a
/// half-scaffolded `.nopal/` (manifest written, bundle rejected) would be
/// worse than neither file existing, since the next launch would treat it
/// as merely misconfigured rather than retrying the scaffold. The error
/// message names the template path; the caller (`nopal-cli::main::
/// dispatch_launch`) normally never reaches this case anyway, since
/// `nopal-cli::launch::plan` already re-derives the same
/// `resolve_bundle_scaffold` outcome and reports `ok: false` for it - this
/// check exists for the TOCTOU sliver between that plan and this write.
///
/// Each file lands via a temp-file + fsync + exclusive hard-link commit,
/// mirroring `run_ledger_store::write_json_durable`'s durability pattern
/// with the rename swapped for `hard_link`: a crash mid-write leaves either
/// the previous state or nothing, never a half-written file, and an
/// existing destination is never replaced. That guarantee is per FILE, not
/// per scaffold: an IO failure between the manifest commit and the bundle
/// commit (ENOSPC, a `bundle.jsonc` appearing in the gap) leaves a
/// manifest-only `.nopal/`, which every later launch fails closed on
/// (`bundle_missing`, D10) rather than re-scaffolding - accepted residual,
/// surfaced not hidden. `write_json_durable` itself is
/// not reused because it serializes a `serde_json::Value`, which would
/// silently drop the leading comments the defaults above (and most
/// templates) carry.
pub fn write_defaults(root: &Path, config_dir: Option<&Path>) -> io::Result<Scaffolded> {
    let root = std::path::absolute(root)?;
    let manifest_rel = discover::manifest_rel_path();
    let bundle_rel = bundle::bundle_rel_path();
    for rel in [&manifest_rel, &bundle_rel] {
        let path = root.join(rel);
        if path.exists() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("refusing to overwrite existing {}", path.display()),
            ));
        }
    }

    let (bundle_text, source) = match resolve_bundle_scaffold(&root, config_dir)? {
        BundleScaffoldOutcome::Ready(ready) => (ready.text, ready.source),
        BundleScaffoldOutcome::TemplateInvalid { path, diagnostics } => {
            let detail = diagnostics
                .iter()
                .filter(|d| {
                    d.severity == Severity::Error && d.code != Code::ScaffoldTemplateInvalid
                })
                .map(|d| d.message.as_str())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(io::Error::other(format!(
                "user default-bundle template {} is invalid; nothing written ({detail})",
                path.display()
            )));
        }
    };

    write_text_durable(&root.join(&manifest_rel), MANIFEST_DEFAULTS)?;
    write_text_durable(&root.join(&bundle_rel), &bundle_text)?;
    Ok(Scaffolded {
        rel_paths: vec![manifest_rel, bundle_rel],
        source,
    })
}

fn write_text_durable(path: &Path, text: &str) -> io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("scaffold.jsonc");
    let tmp = parent.join(format!(".{name}.{}.tmp", token_hex(4)));
    let result = (|| -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        // `hard_link` is the exclusive-commit half of the contract: it
        // fails with `AlreadyExists` when the destination appeared after
        // the caller's pre-check, where `rename` would silently replace it
        // (POSIX rename semantics) - exactly the clobber the fail-closed
        // scaffold forbids.
        fs::hard_link(&tmp, path)?;
        fs::remove_file(&tmp)?;
        if let Ok(dir) = File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();
    if result.is_err() && tmp.exists() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config;

    #[test]
    fn write_text_durable_refuses_destination_that_appeared_mid_gap() {
        // Simulates the destination showing up between `write_defaults`'
        // pre-check and the commit: the exclusive hard-link commit must
        // surface `AlreadyExists` and leave the existing bytes untouched,
        // where a rename would have silently replaced them.
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path().join("nopal.jsonc");
        fs::write(&dest, "sentinel").unwrap();

        let err = write_text_durable(&dest, "clobber").unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&dest).unwrap(), "sentinel");
        // The failed commit must also clean up its temp file.
        let entries = fs::read_dir(temp.path()).unwrap().count();
        assert_eq!(entries, 1, "temp file leaked next to {}", dest.display());
    }

    #[test]
    fn write_defaults_round_trips_through_manifest_and_bundle_parsers() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();

        // No config dir at all: must land on the built-in hermetic default,
        // never touch any real `~/.config/nopal` on the machine running the
        // test isolation.
        let written = write_defaults(root, None).unwrap();
        assert_eq!(
            written.rel_paths,
            vec![discover::manifest_rel_path(), bundle::bundle_rel_path()]
        );
        assert_eq!(written.source, ScaffoldSource::BuiltinHermetic);

        let manifest_text = fs::read_to_string(root.join(discover::manifest_rel_path())).unwrap();
        let (manifest, diagnostics) =
            config::parse_manifest(&manifest_text, &discover::manifest_rel_path());
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let manifest = manifest.expect("manifest parses");
        assert_eq!(
            manifest.profile.map(|p| p.as_str().to_owned()),
            Some("minimal".to_owned())
        );
        assert!(manifest.required_modules.is_empty());

        let bundle_report = bundle::bundle_report(root).unwrap();
        assert!(bundle_report.ok, "{:?}", bundle_report.diagnostics);
        assert_eq!(
            bundle_report.inherit_ambient,
            bundle::AmbientInherit::NONE,
            "scaffolding must be hermetic by default, not full ambient inheritance"
        );
        assert!(bundle_report.resources.is_empty());
    }

    #[test]
    fn write_defaults_creates_nopal_dir_when_absent() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        assert!(!root.join(discover::NOPAL_DIR).exists());

        write_defaults(root, None).unwrap();

        assert!(root.join(discover::NOPAL_DIR).is_dir());
        assert!(root.join(discover::manifest_rel_path()).is_file());
        assert!(root.join(bundle::bundle_rel_path()).is_file());
    }

    #[test]
    fn write_defaults_refuses_to_overwrite_an_existing_file() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path();
        let manifest = root.join(discover::manifest_rel_path());
        fs::create_dir_all(root.join(discover::NOPAL_DIR)).unwrap();
        fs::write(&manifest, "sentinel-user-content").unwrap();

        let err = write_defaults(root, None).expect_err("must refuse to overwrite");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        assert!(
            err.to_string().contains("nopal.jsonc"),
            "error names the offending path: {err}"
        );
        assert_eq!(
            fs::read_to_string(&manifest).unwrap(),
            "sentinel-user-content",
            "existing content must be untouched"
        );
        assert!(
            !root.join(bundle::bundle_rel_path()).exists(),
            "nothing else may be written either"
        );
    }

    #[test]
    fn write_defaults_with_no_template_file_present_falls_back_to_hermetic() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        // A real config dir that simply has no template in it - distinct
        // from `None`, which skips the lookup entirely.
        let config_dir = temp.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();

        let written = write_defaults(&root, Some(&config_dir)).unwrap();
        assert_eq!(written.source, ScaffoldSource::BuiltinHermetic);
        let bundle_text = fs::read_to_string(root.join(bundle::bundle_rel_path())).unwrap();
        assert_eq!(bundle_text, BUNDLE_DEFAULTS);
    }

    #[test]
    fn write_defaults_copies_a_valid_template_verbatim() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let config_dir = temp.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        let template_text = "{\n  // my team's standing default\n  \"version\": \"nopal.bundle/v1\",\n  \"inherit_ambient\": [\"skills\"]\n}\n";
        fs::write(config_dir.join(TEMPLATE_FILE), template_text).unwrap();

        let written = write_defaults(&root, Some(&config_dir)).unwrap();
        assert_eq!(
            written.source,
            ScaffoldSource::Template(config_dir.join(TEMPLATE_FILE))
        );
        let bundle_text = fs::read_to_string(root.join(bundle::bundle_rel_path())).unwrap();
        assert_eq!(
            bundle_text, template_text,
            "template content must be copied byte-for-byte, comments included"
        );

        let bundle_report = bundle::bundle_report(&root).unwrap();
        assert!(bundle_report.ok, "{:?}", bundle_report.diagnostics);
        assert!(bundle_report.inherit_ambient.skills);
        assert!(!bundle_report.inherit_ambient.is_all());
    }

    #[test]
    fn write_defaults_with_invalid_template_writes_nothing_and_names_the_template_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let config_dir = temp.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        let template_path = config_dir.join(TEMPLATE_FILE);
        fs::write(&template_path, "{ \"version\": \"nopal.bundle/v2\" }").unwrap();

        let err = write_defaults(&root, Some(&config_dir)).expect_err("invalid template must fail");
        assert!(
            err.to_string()
                .contains(&template_path.display().to_string()),
            "error names the template path: {err}"
        );
        assert!(
            !root.join(discover::NOPAL_DIR).exists(),
            "nothing at all may be written when the template is invalid, not even the manifest"
        );
    }

    #[test]
    fn resolve_bundle_scaffold_resolves_template_resource_paths_against_the_new_root_not_the_template_dir()
     {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let config_dir = temp.path().join("config");
        fs::create_dir_all(&config_dir).unwrap();
        // A relative pin in the template resolves against the *repo* root,
        // not next to the template file - so it must exist under `root`,
        // not under `config_dir`, to validate.
        fs::write(root.join("skill.md"), "x").unwrap();
        fs::write(
            config_dir.join(TEMPLATE_FILE),
            r#"{
  "version": "nopal.bundle/v1",
  "skills": [ { "source": "team-skill", "path": "skill.md" } ]
}
"#,
        )
        .unwrap();

        let outcome = resolve_bundle_scaffold(&root, Some(&config_dir)).unwrap();
        let BundleScaffoldOutcome::Ready(ready) = outcome else {
            panic!("expected a ready scaffold: {outcome:?}");
        };
        assert!(ready.report.ok, "{:?}", ready.report.diagnostics);
        assert_eq!(ready.report.resources.len(), 1);
        assert_eq!(
            ready.report.resources[0].resolved_path,
            root.join("skill.md")
        );
    }
}
