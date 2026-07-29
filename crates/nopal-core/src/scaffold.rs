//! Deterministic first-run scaffolding for portable Nopal projects.
//!
//! A fresh supported Git worktree receives one complete six-file baseline.
//! Existing Nopal, Beislið, or legacy state is never inferred, merged, or overwritten.
//! The caller re-runs the ordinary configured launch planner against the written bytes before Pi starts.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Read as _, Seek as _, Write as _};
use std::path::{Path, PathBuf};

use cap_std::fs::{Dir, OpenOptions as CapOpenOptions};
use same_file::Handle;

use crate::diagnostics::{Code, Severity};
use crate::run_ledger_store::token_hex;
use crate::{discover, distribution, gate_scaffold, gates, policy};

const BASELINE_MANIFEST: &str = r#"{
  // Created by Nopal for a portable enforced Pi project.
  "version": "nopal.project/v1",
  "profile": "nopal",
  "profiles": {
    "nopal": { "required_modules": ["gates", "policy"] }
  }
}
"#;

const BASELINE_POLICY: &str = r#"{
  "version": "nopal.policy/v1",
  "modes": {
    "supervised_auto": {
      "rules": [
        { "id": "normal-push", "actions": ["git.push"], "decision": "ask" },
        { "id": "force-push", "actions": ["git.push_force"], "decision": "deny" }
      ]
    },
    "unattended_auto": {
      "rules": [
        { "id": "normal-push", "actions": ["git.push"], "decision": "deny" },
        { "id": "force-push", "actions": ["git.push_force"], "decision": "deny" }
      ]
    }
  }
}
"#;

const BASELINE_WORKFLOW: &str = r#"<!-- beislid-workflow: v1 -->

# Nopal project workflow

This repository uses Nopal's checked-in policy, gate, and distribution contracts.
Typed `beislid:*` enforcement blocks may tighten those contracts, but prose never grants authority.
"#;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScaffoldSource {
    BuiltinDistribution,
}

impl ScaffoldSource {
    pub fn describe(&self) -> &'static str {
        match self {
            Self::BuiltinDistribution => "the executing Nopal distribution",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Scaffolded {
    pub rel_paths: Vec<String>,
    pub source: ScaffoldSource,
}

#[derive(Debug, Clone)]
pub struct BaselineFile {
    pub rel_path: String,
    pub text: String,
}

#[derive(Debug, Clone)]
pub struct Baseline {
    pub files: Vec<BaselineFile>,
    pub source: ScaffoldSource,
    pub gate_scaffold: gate_scaffold::GateScaffoldPlan,
}

impl Baseline {
    pub fn text(&self, rel_path: &str) -> Option<&str> {
        self.files
            .iter()
            .find(|file| file.rel_path == rel_path)
            .map(|file| file.text.as_str())
    }
}

/// Compile and validate the exact complete baseline before touching a destination.
/// Dry-run and real launch share this constructor so preview cannot drift from written authority.
pub fn build_baseline(
    root: &Path,
    builtin: distribution::BuiltinDistribution<'_>,
) -> io::Result<Baseline> {
    let root = std::path::absolute(root)?;
    let bundle_text = format!(
        r#"{{
  "version": "nopal.bundle/v2",
  "inherit_ambient": [],
  "packages": [
    {{
      "id": "nopal",
      "source": {{ "type": "builtin", "package": "nopal" }},
      "requirement": "={}",
      "resources": [
        {{ "kind": "extension", "path": "extensions/policy-gate/index.ts" }},
        {{ "kind": "skill", "path": "resources/beislid/skills" }}
      ]
    }}
  ]
}}
"#,
        builtin.version
    );
    let lock = distribution::build_lock_from_local_sources(&root, &bundle_text, &builtin).map_err(
        |diagnostics| {
            io::Error::other(format!(
                "generated distribution baseline is invalid: {}",
                diagnostics
                    .iter()
                    .map(|diagnostic| diagnostic.message.as_str())
                    .collect::<Vec<_>>()
                    .join("; ")
            ))
        },
    )?;
    let lock_text = distribution::lock_json(&lock).map_err(io::Error::other)?;
    let gate_scaffold = gate_scaffold::inspect(&root)?;
    let gates_text = gate_scaffold.gates_json().map_err(io::Error::other)?;

    let (_, manifest_diagnostics) =
        crate::config::parse_manifest(BASELINE_MANIFEST, &discover::manifest_rel_path());
    let (_, gate_diagnostics) = gates::parse_gates(&gates_text, ".nopal/gates.jsonc");
    let mut validation_diagnostics = manifest_diagnostics;
    validation_diagnostics.extend(gate_diagnostics);
    match crate::config::parse_jsonc(
        BASELINE_POLICY,
        ".nopal/policy.jsonc",
        Code::ModuleParseError,
    ) {
        Ok(value) => {
            let (_, diagnostics) = policy::validate_document(&value, ".nopal/policy.jsonc");
            validation_diagnostics.extend(diagnostics);
        }
        Err(diagnostic) => validation_diagnostics.push(diagnostic),
    }
    if validation_diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Err(io::Error::other(format!(
            "generated project baseline is invalid: {}",
            validation_diagnostics
                .iter()
                .map(|diagnostic| diagnostic.message.as_str())
                .collect::<Vec<_>>()
                .join("; ")
        )));
    }

    Ok(Baseline {
        files: vec![
            BaselineFile {
                rel_path: discover::manifest_rel_path(),
                text: BASELINE_MANIFEST.to_owned(),
            },
            BaselineFile {
                rel_path: ".nopal/policy.jsonc".to_owned(),
                text: BASELINE_POLICY.to_owned(),
            },
            BaselineFile {
                rel_path: ".nopal/gates.jsonc".to_owned(),
                text: gates_text,
            },
            BaselineFile {
                rel_path: distribution::BUNDLE_PATH.to_owned(),
                text: bundle_text,
            },
            BaselineFile {
                rel_path: distribution::LOCK_PATH.to_owned(),
                text: lock_text,
            },
            BaselineFile {
                rel_path: ".beislid/workflow.md".to_owned(),
                text: BASELINE_WORKFLOW.to_owned(),
            },
        ],
        source: ScaffoldSource::BuiltinDistribution,
        gate_scaffold,
    })
}

/// Write one complete baseline into a truly unconfigured repository.
/// Held directory capabilities prevent path swaps from redirecting writes.
/// A failed transaction preserves partial generated state so cleanup never deletes concurrent bytes.
pub fn write_baseline(
    root: &Path,
    builtin: distribution::BuiltinDistribution<'_>,
) -> io::Result<Scaffolded> {
    let root = std::path::absolute(root)?;
    reject_existing_project_state(&root)?;

    let baseline = build_baseline(&root, builtin)?;
    write_planned_baseline(&root, baseline)
}

/// Persist the already-inspected baseline used by launch preview. Repository
/// evidence is not rediscovered between the decision and the capability-based
/// transaction.
pub fn write_planned_baseline(root: &Path, baseline: Baseline) -> io::Result<Scaffolded> {
    if !baseline.gate_scaffold.ok
        || baseline.gate_scaffold.readiness == gate_scaffold::Readiness::Blocked
    {
        return Err(io::Error::other(
            "refusing to publish a baseline with blocked gate detection",
        ));
    }
    let root = std::path::absolute(root)?;
    reject_existing_project_state(&root)?;
    write_built_baseline(&root, baseline)
}

fn write_built_baseline(root: &Path, baseline: Baseline) -> io::Result<Scaffolded> {
    write_built_baseline_with_hook(root, baseline, |_| Ok(()))
}

fn write_built_baseline_with_hook<F>(
    root: &Path,
    baseline: Baseline,
    mut hook: F,
) -> io::Result<Scaffolded>
where
    F: FnMut(ScaffoldPoint<'_>) -> io::Result<()>,
{
    let mut ownership = AuthorityOwnership::acquire(root)?;
    hook(ScaffoldPoint::DirectoriesClaimed)?;
    reject_legacy_state(root)?;

    for (index, baseline_file) in baseline.files.iter().enumerate() {
        let path = root.join(&baseline_file.rel_path);
        let authority = authority_for_rel_path(&baseline_file.rel_path)?;
        ownership.commit_file(authority, baseline_file)?;
        hook(ScaffoldPoint::FileCommitted { index, path: &path })?;
        hook(ScaffoldPoint::FileFinalized { index, path: &path })?;
    }

    hook(ScaffoldPoint::BeforeFinalVerification)?;
    ownership.verify_complete(root, &baseline)?;

    Ok(Scaffolded {
        rel_paths: baseline
            .files
            .into_iter()
            .map(|file| file.rel_path)
            .collect(),
        source: baseline.source,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AuthorityKind {
    Nopal,
    Beislid,
}

// The payloads are a deterministic interleaving seam consumed only by unit tests.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy)]
enum ScaffoldPoint<'a> {
    DirectoriesClaimed,
    FileCommitted { index: usize, path: &'a Path },
    FileFinalized { index: usize, path: &'a Path },
    BeforeFinalVerification,
}

#[derive(Debug)]
struct OwnedDirectory {
    path: PathBuf,
    identity: Handle,
    dir: Dir,
}

#[derive(Debug)]
struct CommittedFile {
    authority: AuthorityKind,
    name: OsString,
    expected: Vec<u8>,
}

#[derive(Debug)]
struct AuthorityOwnership {
    nopal: OwnedDirectory,
    beislid: Option<OwnedDirectory>,
    committed: Vec<CommittedFile>,
}

impl AuthorityOwnership {
    fn acquire(root: &Path) -> io::Result<Self> {
        let nopal_path = root.join(discover::NOPAL_DIR);
        fs::create_dir(&nopal_path).map_err(|error| claim_error(&nopal_path, error))?;
        let nopal = open_claimed_directory(nopal_path)?;

        let beislid_path = root.join(".beislid");
        fs::create_dir(&beislid_path).map_err(|error| claim_error(&beislid_path, error))?;
        let beislid = open_claimed_directory(beislid_path)?;

        Ok(Self {
            nopal,
            beislid: Some(beislid),
            committed: Vec::new(),
        })
    }

    fn directory(&self, authority: AuthorityKind) -> io::Result<&OwnedDirectory> {
        match authority {
            AuthorityKind::Nopal => Ok(&self.nopal),
            AuthorityKind::Beislid => self
                .beislid
                .as_ref()
                .ok_or_else(|| io::Error::other("Beislid authority directory was not claimed")),
        }
    }

    fn commit_file(
        &mut self,
        authority: AuthorityKind,
        baseline_file: &BaselineFile,
    ) -> io::Result<()> {
        let name = baseline_file_name(baseline_file)?;
        let directory = self.directory(authority)?;
        let temporary = OsString::from(format!(".{}.{}.tmp", name.to_string_lossy(), token_hex(8)));
        let mut options = CapOpenOptions::new();
        options.read(true).write(true).create_new(true);
        let mut file = directory.dir.open_with(&temporary, &options)?;
        file.write_all(baseline_file.text.as_bytes())?;
        file.sync_all()?;

        // The final pathname appears only after every byte is durable.
        // Hard-link creation is atomic and refuses a concurrent destination,
        // so Git and other readers can observe either absence or the complete file.
        directory.dir.hard_link(&temporary, &directory.dir, &name)?;
        directory.dir.remove_file(&temporary)?;
        directory.dir.try_clone()?.into_std_file().sync_all()?;
        self.committed.push(CommittedFile {
            authority,
            name,
            expected: baseline_file.text.as_bytes().to_vec(),
        });
        Ok(())
    }

    fn verify_complete(&self, root: &Path, baseline: &Baseline) -> io::Result<()> {
        reject_legacy_state(root)?;
        ensure_current_directory(&self.nopal)?;
        ensure_current_directory(self.directory(AuthorityKind::Beislid)?)?;

        let expected_nopal = expected_names(baseline, AuthorityKind::Nopal)?;
        let expected_beislid = expected_names(baseline, AuthorityKind::Beislid)?;
        verify_directory_entries(&self.nopal, &expected_nopal)?;
        verify_directory_entries(self.directory(AuthorityKind::Beislid)?, &expected_beislid)?;

        if self.committed.len() != baseline.files.len() {
            return Err(io::Error::other(
                "scaffold did not commit every generated baseline file",
            ));
        }
        for committed in &self.committed {
            let directory = self.directory(committed.authority)?;
            let actual = read_regular_file_without_symlinks(directory, &committed.name)?;
            if actual != committed.expected {
                return Err(concurrent_state_error(
                    &directory.path.join(&committed.name),
                ));
            }
        }
        Ok(())
    }
}

fn authority_for_rel_path(rel_path: &str) -> io::Result<AuthorityKind> {
    if rel_path.starts_with(".nopal/") {
        Ok(AuthorityKind::Nopal)
    } else if rel_path.starts_with(".beislid/") {
        Ok(AuthorityKind::Beislid)
    } else {
        Err(io::Error::other(format!(
            "generated scaffold path is outside authority directories: {rel_path}"
        )))
    }
}

fn expected_names(baseline: &Baseline, authority: AuthorityKind) -> io::Result<BTreeSet<OsString>> {
    baseline
        .files
        .iter()
        .filter(|file| authority_for_rel_path(&file.rel_path).ok() == Some(authority))
        .map(|file| {
            Path::new(&file.rel_path)
                .file_name()
                .map(OsString::from)
                .ok_or_else(|| {
                    io::Error::other(format!(
                        "generated scaffold path has no file name: {}",
                        file.rel_path
                    ))
                })
        })
        .collect()
}

fn verify_directory_entries(
    directory: &OwnedDirectory,
    expected: &BTreeSet<OsString>,
) -> io::Result<()> {
    let actual = directory
        .dir
        .entries()?
        .map(|entry| entry.map(|entry| entry.file_name()))
        .collect::<io::Result<BTreeSet<_>>>()?;
    if actual != *expected {
        return Err(concurrent_state_error(&directory.path));
    }
    Ok(())
}

fn ensure_current_directory(directory: &OwnedDirectory) -> io::Result<()> {
    let current = open_real_directory(&directory.path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "scaffold authority directory changed after it was claimed {}: {error}",
                directory.path.display()
            ),
        )
    })?;
    if current.1 != directory.identity {
        return Err(concurrent_state_error(&directory.path));
    }
    Ok(())
}

fn baseline_file_name(file: &BaselineFile) -> io::Result<OsString> {
    Path::new(&file.rel_path)
        .file_name()
        .map(OsString::from)
        .ok_or_else(|| {
            io::Error::other(format!(
                "generated scaffold path has no file name: {}",
                file.rel_path
            ))
        })
}

fn open_claimed_directory(path: PathBuf) -> io::Result<OwnedDirectory> {
    let (dir, identity) = open_real_directory(&path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!(
                "failed to identify claimed scaffold authority directory {}: {error}",
                path.display()
            ),
        )
    })?;
    Ok(OwnedDirectory {
        path,
        identity,
        dir,
    })
}

fn open_real_directory(path: &Path) -> io::Result<(Dir, Handle)> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(concurrent_state_error(path));
    }

    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_dir() {
        return Err(concurrent_state_error(path));
    }
    let identity = Handle::from_file(file.try_clone()?)?;
    Ok((Dir::from_std_file(file), identity))
}

fn read_regular_file_without_symlinks(
    directory: &OwnedDirectory,
    name: &OsString,
) -> io::Result<Vec<u8>> {
    let metadata = directory.dir.symlink_metadata(name)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(concurrent_state_error(&directory.path.join(name)));
    }

    let mut options = CapOpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use cap_std::fs::OpenOptionsExt as _;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = directory.dir.open_with(name, &options)?;
    if !file.metadata()?.is_file() {
        return Err(concurrent_state_error(&directory.path.join(name)));
    }
    let mut bytes = Vec::new();
    file.seek(io::SeekFrom::Start(0))?;
    file.read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn concurrent_state_error(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "concurrent state changed scaffold authority at {}",
            path.display()
        ),
    )
}

fn reject_existing_project_state(root: &Path) -> io::Result<()> {
    for existing in [
        root.join(discover::NOPAL_DIR),
        root.join(".beislid"),
        root.join(discover::LEGACY_DIR),
    ] {
        if path_entry_exists(&existing)? {
            return Err(existing_state_error(&existing));
        }
    }
    Ok(())
}

fn reject_legacy_state(root: &Path) -> io::Result<()> {
    let legacy = root.join(discover::LEGACY_DIR);
    if path_entry_exists(&legacy)? {
        return Err(existing_state_error(&legacy));
    }
    Ok(())
}

fn path_entry_exists(path: &Path) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

fn existing_state_error(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!(
            "refusing to scaffold over existing or legacy project state {}",
            path.display()
        ),
    )
}

fn claim_error(path: &Path, error: io::Error) -> io::Error {
    io::Error::new(
        error.kind(),
        format!(
            "failed to exclusively claim scaffold authority directory {}: {error}",
            path.display()
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_adapter(root: &Path) {
        let adapter = root.join("extensions/policy-gate");
        fs::create_dir_all(&adapter).unwrap();
        fs::write(adapter.join("index.ts"), "export default 1;\n").unwrap();
        fs::write(
            adapter.join("classifier.ts"),
            "export const classify = 1;\n",
        )
        .unwrap();
        fs::write(adapter.join("guard.ts"), "export const guard = 1;\n").unwrap();
        fs::write(adapter.join("nopal-cli.ts"), "export const cli = 1;\n").unwrap();
        let skills = root.join("resources/beislid/skills/kickoff");
        fs::create_dir_all(&skills).unwrap();
        fs::write(skills.join("SKILL.md"), "# Kickoff\n").unwrap();
    }

    #[test]
    fn durable_write_refuses_an_existing_destination() {
        let temp = tempfile::tempdir().unwrap();
        let mut ownership = AuthorityOwnership::acquire(temp.path()).unwrap();
        let destination = temp.path().join(".nopal/nopal.jsonc");
        fs::write(&destination, "sentinel").unwrap();
        let baseline_file = BaselineFile {
            rel_path: ".nopal/nopal.jsonc".to_owned(),
            text: "clobber".to_owned(),
        };

        let error = ownership
            .commit_file(AuthorityKind::Nopal, &baseline_file)
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&destination).unwrap(), "sentinel");
        let names = fs::read_dir(temp.path().join(".nopal"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect::<Vec<_>>();
        assert_eq!(names.len(), 2);
        assert!(
            names
                .iter()
                .any(|name| name.to_string_lossy().ends_with(".tmp"))
        );
    }

    #[test]
    fn complete_baseline_creates_every_contract_file_and_a_valid_lock() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("distribution");
        fs::create_dir_all(&root).unwrap();
        write_adapter(&adapter);

        let written = write_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap();

        assert_eq!(
            written.rel_paths,
            vec![
                ".nopal/nopal.jsonc",
                ".nopal/policy.jsonc",
                ".nopal/gates.jsonc",
                ".nopal/bundle.jsonc",
                ".nopal/nopal.lock",
                ".beislid/workflow.md",
            ]
        );
        for path in &written.rel_paths {
            assert!(root.join(path).is_file(), "missing scaffold output {path}");
        }
        let report = distribution::inspect(distribution::DistributionContext {
            project_root: &root,
            store_root: &temp.path().join("store"),
            builtin: distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        })
        .unwrap();
        assert!(report.ok, "{:?}", report.diagnostics);
        assert_eq!(report.resources.len(), 2);
    }

    #[test]
    fn existing_authority_is_preserved_and_rejected_before_any_write() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("adapter");
        fs::create_dir_all(root.join(".beislid")).unwrap();
        fs::write(root.join(".beislid/workflow.md"), "sentinel\n").unwrap();
        write_adapter(&adapter);

        let error = write_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(root.join(".beislid/workflow.md")).unwrap(),
            "sentinel\n"
        );
        assert!(!root.join(".nopal").exists());
    }

    #[test]
    fn concurrent_scaffolds_cannot_merge_into_shared_authority_directories() {
        use std::sync::{Arc, Barrier};

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("adapter");
        fs::create_dir_all(&root).unwrap();
        write_adapter(&adapter);
        let baseline = build_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap();
        let barrier = Arc::new(Barrier::new(2));

        let handles = (0..2)
            .map(|_| {
                let root = root.clone();
                let baseline = baseline.clone();
                let barrier = Arc::clone(&barrier);
                std::thread::spawn(move || {
                    barrier.wait();
                    write_built_baseline(&root, baseline)
                })
            })
            .collect::<Vec<_>>();
        let results = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>();

        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        let loser = results
            .iter()
            .find_map(|result| result.as_ref().err())
            .expect("one scaffold must lose the exclusive directory claim");
        assert_eq!(loser.kind(), io::ErrorKind::AlreadyExists);
        for file in &baseline.files {
            assert!(root.join(&file.rel_path).is_file());
        }
        assert_eq!(fs::read_dir(root.join(".nopal")).unwrap().count(), 5);
        assert_eq!(fs::read_dir(root.join(".beislid")).unwrap().count(), 1);
    }

    #[test]
    fn losing_the_second_directory_claim_preserves_partial_generated_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("adapter");
        fs::create_dir_all(&root).unwrap();
        write_adapter(&adapter);
        let baseline = build_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap();
        fs::create_dir(root.join(".beislid")).unwrap();
        fs::write(root.join(".beislid/concurrent-state"), "sentinel\n").unwrap();

        let error = write_built_baseline(&root, baseline).unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(root.join(".nopal").is_dir());
        assert_eq!(fs::read_dir(root.join(".nopal")).unwrap().count(), 0);
        assert_eq!(
            fs::read_to_string(root.join(".beislid/concurrent-state")).unwrap(),
            "sentinel\n"
        );
    }

    #[test]
    fn legacy_state_appearing_during_writes_aborts_and_preserves_partial_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("adapter");
        fs::create_dir_all(&root).unwrap();
        write_adapter(&adapter);
        let baseline = build_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap();
        let legacy = root.join(discover::LEGACY_DIR);

        let error = write_built_baseline_with_hook(&root, baseline, |point| {
            if matches!(point, ScaffoldPoint::FileFinalized { index: 0, .. }) {
                fs::create_dir(&legacy)?;
                fs::write(legacy.join("state"), "sentinel\n")?;
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(root.join(".nopal/nopal.jsonc").is_file());
        assert!(root.join(".beislid").is_dir());
        assert_eq!(
            fs::read_to_string(root.join(discover::LEGACY_DIR).join("state")).unwrap(),
            "sentinel\n"
        );
    }

    #[test]
    fn unexpected_state_injected_during_writes_is_not_merged() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("adapter");
        fs::create_dir_all(&root).unwrap();
        write_adapter(&adapter);
        let baseline = build_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap();
        let concurrent = root.join(".nopal/concurrent-state");

        let error = write_built_baseline_with_hook(&root, baseline.clone(), |point| {
            if matches!(point, ScaffoldPoint::FileFinalized { index: 0, .. }) {
                fs::write(&concurrent, "sentinel\n")?;
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(fs::read_to_string(&concurrent).unwrap(), "sentinel\n");
        for file in &baseline.files {
            assert!(root.join(&file.rel_path).is_file());
        }
        assert!(root.join(".beislid").is_dir());
    }

    #[test]
    fn failure_after_file_commit_preserves_partial_generated_state() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("adapter");
        fs::create_dir_all(&root).unwrap();
        write_adapter(&adapter);
        let baseline = build_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap();

        let error = write_built_baseline_with_hook(&root, baseline, |point| {
            if matches!(point, ScaffoldPoint::FileCommitted { index: 0, .. }) {
                return Err(io::Error::other("injected post-commit failure"));
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error.to_string(), "injected post-commit failure");
        assert!(root.join(".nopal/nopal.jsonc").is_file());
        assert!(root.join(".beislid").is_dir());
    }

    #[test]
    fn failure_preserves_a_concurrent_replacement_of_a_generated_path() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("adapter");
        fs::create_dir_all(&root).unwrap();
        write_adapter(&adapter);
        let baseline = build_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap();
        let replaced = root.join(".nopal/nopal.jsonc");

        let error = write_built_baseline_with_hook(&root, baseline, |point| {
            if let ScaffoldPoint::FileFinalized { index: 0, path } = point {
                assert_eq!(path, replaced);
                fs::remove_file(path)?;
                fs::write(path, "sentinel\n")?;
                return Err(io::Error::other("injected replacement failure"));
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error.to_string(), "injected replacement failure");
        assert_eq!(fs::read_to_string(&replaced).unwrap(), "sentinel\n");
        assert!(root.join(".nopal").is_dir());
        assert!(root.join(".beislid").is_dir());
    }

    #[test]
    #[cfg(unix)]
    fn failure_does_not_follow_a_replaced_authority_directory_symlink() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("adapter");
        let external = temp.path().join("external");
        fs::create_dir_all(&root).unwrap();
        fs::create_dir_all(&external).unwrap();
        fs::write(external.join("nopal.jsonc"), "external-sentinel\n").unwrap();
        write_adapter(&adapter);
        let baseline = build_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap();
        let displaced = root.join(".nopal-owned");

        let error = write_built_baseline_with_hook(&root, baseline, |point| {
            if matches!(point, ScaffoldPoint::FileFinalized { index: 0, .. }) {
                fs::rename(root.join(".nopal"), &displaced)?;
                symlink(&external, root.join(".nopal"))?;
                return Err(io::Error::other("injected directory replacement"));
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error.to_string(), "injected directory replacement");
        assert_eq!(
            fs::read_to_string(external.join("nopal.jsonc")).unwrap(),
            "external-sentinel\n"
        );
        assert!(displaced.join("nopal.jsonc").is_file());
        assert!(root.join(".nopal").is_symlink());
        assert!(root.join(".beislid").is_dir());
    }

    #[test]
    fn in_place_mutation_is_rejected_by_exact_byte_verification() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("adapter");
        fs::create_dir_all(&root).unwrap();
        write_adapter(&adapter);
        let baseline = build_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap();

        let error = write_built_baseline_with_hook(&root, baseline, |point| {
            if matches!(point, ScaffoldPoint::BeforeFinalVerification) {
                fs::write(root.join(".nopal/nopal.jsonc"), "mutated in place\n")?;
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(
            fs::read_to_string(root.join(".nopal/nopal.jsonc")).unwrap(),
            "mutated in place\n"
        );
    }

    #[test]
    #[cfg(unix)]
    fn same_inode_symlink_alias_cannot_become_authority() {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("repo");
        let adapter = temp.path().join("adapter");
        fs::create_dir_all(&root).unwrap();
        write_adapter(&adapter);
        let baseline = build_baseline(
            &root,
            distribution::BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        )
        .unwrap();
        let displaced = root.join(".nopal-owned");

        let error = write_built_baseline_with_hook(&root, baseline, |point| {
            if matches!(point, ScaffoldPoint::DirectoriesClaimed) {
                fs::rename(root.join(".nopal"), &displaced)?;
                symlink(".nopal-owned", root.join(".nopal"))?;
            }
            Ok(())
        })
        .unwrap_err();

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(root.join(".nopal").is_symlink());
        assert_eq!(fs::read_dir(&displaced).unwrap().count(), 5);
        assert!(root.join(".beislid/workflow.md").is_file());
    }
}
