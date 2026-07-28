#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use base64::Engine as _;
use nopal_core::distribution::{
    self, BuiltinDistribution, DistributionContext, NpmResolution, ResourceKind, SourceSpec,
};

fn write_adapter(root: &Path) {
    let adapter = root.join("extensions/policy-gate");
    fs::create_dir_all(&adapter).unwrap();
    fs::write(adapter.join("index.ts"), "export default 1;\n").unwrap();
    fs::write(
        adapter.join("classifier.ts"),
        "export const classify = 1;\n",
    )
    .unwrap();
    fs::write(adapter.join("nopal-cli.ts"), "export const cli = 1;\n").unwrap();
}

fn builtin_bundle() -> &'static str {
    r#"{
  "version": "nopal.bundle/v2",
  "inherit_ambient": [],
  "packages": [
    {
      "id": "nopal",
      "source": { "type": "builtin", "package": "nopal" },
      "requirement": "=0.3.0",
      "resources": [
        { "kind": "extension", "path": "extensions/policy-gate/index.ts" }
      ]
    }
  ]
}
"#
}

fn duplicate_builtin_bundle() -> &'static str {
    r#"{
  "version": "nopal.bundle/v2",
  "inherit_ambient": [],
  "packages": [
    {
      "id": "nopal-a",
      "source": { "type": "builtin", "package": "nopal" },
      "requirement": "=0.3.0",
      "resources": [
        { "kind": "extension", "path": "extensions/policy-gate/index.ts" }
      ]
    },
    {
      "id": "nopal-b",
      "source": { "type": "builtin", "package": "nopal" },
      "requirement": "=0.3.0",
      "resources": [
        { "kind": "extension", "path": "extensions/policy-gate/index.ts" }
      ]
    }
  ]
}
"#
}

#[test]
fn builtin_lock_and_inspection_resolve_the_exact_verified_resource() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let distribution_root = temp.path().join("distribution");
    let store = temp.path().join("store");
    fs::create_dir_all(project.join(".nopal")).unwrap();
    write_adapter(&distribution_root);
    fs::write(project.join(".nopal/bundle.jsonc"), builtin_bundle()).unwrap();

    let builtin = BuiltinDistribution {
        version: "0.3.0",
        root: &distribution_root,
    };
    let lock = distribution::build_lock_from_local_sources(&project, builtin_bundle(), &builtin)
        .expect("builtin contract resolves");
    fs::write(
        project.join(".nopal/nopal.lock"),
        distribution::lock_json(&lock).unwrap(),
    )
    .unwrap();

    let report = distribution::inspect(DistributionContext {
        project_root: &project,
        store_root: &store,
        builtin,
    })
    .unwrap();

    assert!(report.ok, "{:?}", report.diagnostics);
    assert_eq!(report.resources.len(), 1);
    assert_eq!(report.resources[0].kind, ResourceKind::Extension);
    assert_eq!(
        report.resources[0].resolved_path,
        distribution_root.join("extensions/policy-gate/index.ts")
    );
    assert_eq!(report.packages[0].resolved, "0.3.0");
}

#[test]
fn canonical_resource_paths_are_unique_across_package_ids() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let distribution_root = temp.path().join("distribution");
    fs::create_dir_all(project.join(".nopal")).unwrap();
    write_adapter(&distribution_root);
    let builtin = BuiltinDistribution {
        version: "0.3.0",
        root: &distribution_root,
    };

    let diagnostics =
        distribution::build_lock_from_local_sources(&project, duplicate_builtin_bundle(), &builtin)
            .unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionPackageInvalid
            && diagnostic.message.contains("canonical resource")
            && diagnostic.message.contains("nopal-a")
            && diagnostic.message.contains("nopal-b")
            && diagnostic.message.contains("resource_export")
    }));

    let single =
        distribution::build_lock_from_local_sources(&project, builtin_bundle(), &builtin).unwrap();
    let mut first = single.packages[0].clone();
    first.id = "nopal-a".to_owned();
    let mut second = first.clone();
    second.id = "nopal-b".to_owned();
    let preexisting_lock = distribution::LockDocument {
        version: distribution::LOCK_KIND.to_owned(),
        contract_digest: format!("sha256:{}", "0".repeat(64)),
        packages: vec![first, second],
    };
    let report = distribution::inspect_texts(
        DistributionContext {
            project_root: &project,
            store_root: &temp.path().join("store"),
            builtin,
        },
        duplicate_builtin_bundle(),
        &distribution::lock_json(&preexisting_lock).unwrap(),
    )
    .unwrap();
    assert!(!report.ok);
    assert!(report.resources.is_empty());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionPackageInvalid
            && diagnostic.message.contains("canonical resource")
            && diagnostic.message.contains("nopal-a")
            && diagnostic.message.contains("nopal-b")
    }));
}

#[test]
fn workspace_packages_cannot_alias_one_canonical_resource() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let package = project.join("packages/shared");
    let distribution_root = temp.path().join("distribution");
    fs::create_dir_all(&package).unwrap();
    write_adapter(&distribution_root);
    fs::write(
        package.join("package.json"),
        r#"{ "name": "shared", "version": "1.2.3" }"#,
    )
    .unwrap();
    fs::write(package.join("skill.md"), "shared skill\n").unwrap();
    let bundle = r#"{
  "version": "nopal.bundle/v2",
  "packages": [
    {
      "id": "shared-a",
      "source": { "type": "workspace", "package": "shared", "root": "packages/shared" },
      "requirement": "=1.2.3",
      "resources": [{ "kind": "skill", "path": "skill.md" }]
    },
    {
      "id": "shared-b",
      "source": { "type": "workspace", "package": "shared", "root": "packages/shared" },
      "requirement": "=1.2.3",
      "resources": [{ "kind": "skill", "path": "skill.md" }]
    }
  ]
}"#;

    let diagnostics = distribution::build_lock_from_local_sources(
        &project,
        bundle,
        &BuiltinDistribution {
            version: "0.3.0",
            root: &distribution_root,
        },
    )
    .unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionPackageInvalid
            && diagnostic.message.contains("canonical resource")
            && diagnostic.message.contains("shared-a")
            && diagnostic.message.contains("shared-b")
    }));
}

#[test]
fn builtin_integrity_ignores_source_only_files_but_covers_adapter_dependencies() {
    let temp = tempfile::tempdir().unwrap();
    let source = temp.path().join("source");
    let packaged = temp.path().join("packaged");
    write_adapter(&source);
    write_adapter(&packaged);
    fs::create_dir_all(source.join("extensions/policy-gate/tests")).unwrap();
    fs::write(
        source.join("extensions/policy-gate/tests/source-only.test.ts"),
        "source-only\n",
    )
    .unwrap();
    let source_lock = distribution::build_lock_from_local_sources(
        temp.path(),
        builtin_bundle(),
        &BuiltinDistribution {
            version: "0.3.0",
            root: &source,
        },
    )
    .unwrap();
    let packaged_lock = distribution::build_lock_from_local_sources(
        temp.path(),
        builtin_bundle(),
        &BuiltinDistribution {
            version: "0.3.0",
            root: &packaged,
        },
    )
    .unwrap();
    assert_eq!(
        source_lock.packages[0].installed_tree_integrity,
        packaged_lock.packages[0].installed_tree_integrity
    );

    fs::write(
        packaged.join("extensions/policy-gate/nopal-cli.ts"),
        "tampered dependency\n",
    )
    .unwrap();
    let changed = distribution::build_lock_from_local_sources(
        temp.path(),
        builtin_bundle(),
        &BuiltinDistribution {
            version: "0.3.0",
            root: &packaged,
        },
    )
    .unwrap();
    assert_ne!(
        source_lock.packages[0].installed_tree_integrity,
        changed.packages[0].installed_tree_integrity
    );
}

#[test]
fn contract_drift_and_installed_resource_mutation_fail_closed() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let distribution_root = temp.path().join("distribution");
    fs::create_dir_all(project.join(".nopal")).unwrap();
    write_adapter(&distribution_root);
    let builtin = BuiltinDistribution {
        version: "0.3.0",
        root: &distribution_root,
    };
    let lock =
        distribution::build_lock_from_local_sources(&project, builtin_bundle(), &builtin).unwrap();
    fs::write(project.join(".nopal/bundle.jsonc"), builtin_bundle()).unwrap();
    fs::write(
        project.join(".nopal/nopal.lock"),
        distribution::lock_json(&lock).unwrap(),
    )
    .unwrap();

    fs::write(
        distribution_root.join("extensions/policy-gate/classifier.ts"),
        "tampered\n",
    )
    .unwrap();
    let report = distribution::inspect(DistributionContext {
        project_root: &project,
        store_root: &temp.path().join("store"),
        builtin,
    })
    .unwrap();
    assert!(!report.ok);
    assert!(report.resources.is_empty());
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionIntegrityMismatch
            && diagnostic.message.contains("installed_integrity")
            && diagnostic.message.contains("package \"nopal\"")
    }));

    fs::write(
        distribution_root.join("extensions/policy-gate/classifier.ts"),
        "export const classify = 1;\n",
    )
    .unwrap();
    fs::write(
        project.join(".nopal/bundle.jsonc"),
        builtin_bundle().replace("=0.3.0", "=0.3.1"),
    )
    .unwrap();
    let drift = distribution::inspect(DistributionContext {
        project_root: &project,
        store_root: &temp.path().join("store"),
        builtin,
    })
    .unwrap();
    assert!(!drift.ok);
    assert!(drift.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionLockDrift
            && diagnostic.message.contains("contract_lock")
    }));

    let mut source_drift = lock;
    source_drift.packages[0].source = SourceSpec::Builtin {
        package: "other".to_owned(),
    };
    let source_drift = distribution::inspect_texts(
        DistributionContext {
            project_root: &project,
            store_root: &temp.path().join("store"),
            builtin,
        },
        builtin_bundle(),
        &distribution::lock_json(&source_drift).unwrap(),
    )
    .unwrap();
    assert!(source_drift.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionLockDrift
            && diagnostic.path == distribution::LOCK_PATH
            && diagnostic
                .message
                .contains("control boundary contract_lock")
    }));
}

#[test]
fn workspace_package_locks_exact_manifest_version_and_resource_tree() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let package = project.join("packages/team-guidance");
    let adapter = temp.path().join("adapter");
    fs::create_dir_all(package.join("skills/review")).unwrap();
    fs::create_dir_all(&adapter).unwrap();
    fs::write(adapter.join("index.ts"), "x").unwrap();
    fs::write(
        package.join("package.json"),
        r#"{ "name": "@team/guidance", "version": "1.4.2" }"#,
    )
    .unwrap();
    fs::write(package.join("skills/review/SKILL.md"), "# Review\n").unwrap();
    let bundle = r#"{
  "version": "nopal.bundle/v2",
  "packages": [{
    "id": "guidance",
    "source": { "type": "workspace", "package": "@team/guidance", "root": "packages/team-guidance" },
    "requirement": "=1.4.2",
    "resources": [{ "kind": "skill", "path": "skills/review" }]
  }]
}"#;
    let builtin = BuiltinDistribution {
        version: "0.3.0",
        root: &adapter,
    };
    let lock = distribution::build_lock_from_local_sources(&project, bundle, &builtin).unwrap();
    assert_eq!(lock.packages[0].resolved, "1.4.2");
    assert_eq!(lock.packages[0].source.package(), "@team/guidance");
    assert!(lock.packages[0].artifact_integrity.starts_with("sha256:"));
    assert!(
        lock.packages[0].resources[0]
            .tree_integrity
            .starts_with("sha256:")
    );

    fs::write(
        package.join("package.json"),
        r#"{ "name": "@team/guidance", "version": "1.4.3" }"#,
    )
    .unwrap();
    let report = distribution::inspect_texts(
        DistributionContext {
            project_root: &project,
            store_root: &temp.path().join("store"),
            builtin,
        },
        bundle,
        &distribution::lock_json(&lock).unwrap(),
    )
    .unwrap();
    assert!(!report.ok);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionIntegrityMismatch
            && diagnostic
                .message
                .contains("control boundary installed_manifest")
            && diagnostic.message.contains("does not match locked version")
    }));
}

#[test]
#[cfg(unix)]
fn linked_control_files_are_rejected_without_following_external_authority() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let distribution_root = temp.path().join("distribution");
    fs::create_dir_all(project.join(".nopal")).unwrap();
    write_adapter(&distribution_root);
    let builtin = BuiltinDistribution {
        version: "0.3.0",
        root: &distribution_root,
    };
    let lock =
        distribution::build_lock_from_local_sources(&project, builtin_bundle(), &builtin).unwrap();
    let external = temp.path().join("external-bundle.jsonc");
    fs::write(&external, builtin_bundle()).unwrap();
    symlink(&external, project.join(".nopal/bundle.jsonc")).unwrap();
    fs::write(
        project.join(".nopal/nopal.lock"),
        distribution::lock_json(&lock).unwrap(),
    )
    .unwrap();

    let report = distribution::inspect(DistributionContext {
        project_root: &project,
        store_root: &temp.path().join("store"),
        builtin,
    })
    .unwrap();
    assert!(!report.ok);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionPackageInvalid
            && diagnostic.message.contains("real regular file")
    }));
}

#[test]
fn duplicate_or_malformed_lock_evidence_is_rejected_before_resolution() {
    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let distribution_root = temp.path().join("distribution");
    write_adapter(&distribution_root);
    let builtin = BuiltinDistribution {
        version: "0.3.0",
        root: &distribution_root,
    };
    let mut lock =
        distribution::build_lock_from_local_sources(&project, builtin_bundle(), &builtin).unwrap();
    lock.packages[0].installed_tree_integrity = "sha256:not-a-digest".to_owned();
    lock.packages.push(lock.packages[0].clone());
    let report = distribution::inspect_texts(
        DistributionContext {
            project_root: &project,
            store_root: &temp.path().join("store"),
            builtin,
        },
        builtin_bundle(),
        &distribution::lock_json(&lock).unwrap(),
    )
    .unwrap();
    assert!(!report.ok);
    assert!(report.packages.is_empty());
    assert!(
        report
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == nopal_core::diagnostics::Code::DuplicateId)
    );
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionLockParseError
            && diagnostic.message.contains("installed tree integrity")
            && diagnostic.message.contains("builtin source \"nopal\"")
            && diagnostic
                .message
                .contains("control boundary installed_integrity")
    }));
}

#[test]
fn unsafe_package_paths_and_third_party_extensions_are_rejected() {
    let temp = tempfile::tempdir().unwrap();
    let adapter = temp.path().join("adapter");
    fs::create_dir_all(&adapter).unwrap();
    fs::write(adapter.join("index.ts"), "x").unwrap();
    let bundle = r#"{
  "version": "nopal.bundle/v2",
  "packages": [{
    "id": "escape",
    "source": { "type": "workspace", "package": "escape", "root": "../escape" },
    "requirement": "=1.0.0",
    "resources": [{ "kind": "extension", "path": "../evil.ts" }]
  }]
}"#;
    let diagnostics = distribution::build_lock_from_local_sources(
        temp.path(),
        bundle,
        &BuiltinDistribution {
            version: "0.3.0",
            root: &adapter,
        },
    )
    .unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionPackageInvalid
            && diagnostic.message.contains("control boundary")
    }));

    let option_injection = bundle.replace("\"package\": \"escape\"", "\"package\": \"--force\"");
    let diagnostics = distribution::package_requests(&option_injection).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionPackageInvalid
            && diagnostic.message.contains("package source identity")
    }));

    let aliased_resource = bundle
        .replace("../escape", "packages/escape")
        .replace("../evil.ts", "./skills/review");
    let diagnostics = distribution::package_requests(&aliased_resource).unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionPackageInvalid
            && diagnostic.message.contains("canonical portable spelling")
    }));
}

#[test]
#[cfg(unix)]
fn workspace_roots_reject_intermediate_symlink_escape() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let external = temp.path().join("external");
    let adapter = temp.path().join("adapter");
    fs::create_dir_all(&project).unwrap();
    fs::create_dir_all(external.join("guidance/skills/review")).unwrap();
    fs::create_dir_all(&adapter).unwrap();
    fs::write(adapter.join("index.ts"), "x").unwrap();
    fs::write(
        external.join("guidance/package.json"),
        r#"{ "name": "guidance", "version": "1.0.0" }"#,
    )
    .unwrap();
    fs::write(
        external.join("guidance/skills/review/SKILL.md"),
        "# Review\n",
    )
    .unwrap();
    symlink(&external, project.join("packages")).unwrap();
    let bundle = r#"{
  "version": "nopal.bundle/v2",
  "packages": [{
    "id": "guidance",
    "source": { "type": "workspace", "package": "guidance", "root": "packages/guidance" },
    "requirement": "=1.0.0",
    "resources": [{ "kind": "skill", "path": "skills/review" }]
  }]
}"#;
    let diagnostics = distribution::build_lock_from_local_sources(
        &project,
        bundle,
        &BuiltinDistribution {
            version: "0.3.0",
            root: &adapter,
        },
    )
    .unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionPackageInvalid
            && diagnostic
                .message
                .contains("control boundary workspace_root")
            && diagnostic.message.contains("symbolic link")
    }));
}

#[test]
fn npm_resolution_must_match_the_exact_contract_requirement() {
    let temp = tempfile::tempdir().unwrap();
    let package = temp.path().join("package");
    let adapter = temp.path().join("adapter");
    fs::create_dir_all(package.join("skills/review")).unwrap();
    fs::create_dir_all(&adapter).unwrap();
    fs::write(adapter.join("index.ts"), "x").unwrap();
    fs::write(
        package.join("package.json"),
        r#"{ "name": "guidance", "version": "1.2.4" }"#,
    )
    .unwrap();
    fs::write(package.join("skills/review/SKILL.md"), "# Review\n").unwrap();
    let bundle = r#"{
  "version": "nopal.bundle/v2",
  "packages": [{
    "id": "guidance",
    "source": { "type": "npm", "package": "guidance", "registry": "https://registry.npmjs.org" },
    "requirement": "=1.2.3",
    "resources": [{ "kind": "skill", "path": "skills/review" }]
  }]
}"#;
    let sri = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode([0_u8; 64])
    );
    let diagnostics = distribution::build_lock_from_resolved_sources(
        temp.path(),
        bundle,
        &BuiltinDistribution {
            version: "0.3.0",
            root: &adapter,
        },
        &[NpmResolution {
            package_id: "guidance".to_owned(),
            resolved: "1.2.4".to_owned(),
            artifact_integrity: sri.clone(),
            root: package.clone(),
        }],
    )
    .unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionLockDrift
            && diagnostic.message.contains("control boundary resolution")
            && diagnostic
                .message
                .contains("does not match exact requirement")
    }));

    fs::write(
        package.join("package.json"),
        r#"{ "name": "guidance", "version": "9.9.9" }"#,
    )
    .unwrap();
    let diagnostics = distribution::build_lock_from_resolved_sources(
        temp.path(),
        &bundle.replace("=1.2.3", "=1.2.4"),
        &BuiltinDistribution {
            version: "0.3.0",
            root: &adapter,
        },
        &[NpmResolution {
            package_id: "guidance".to_owned(),
            resolved: "1.2.4".to_owned(),
            artifact_integrity: sri,
            root: package,
        }],
    )
    .unwrap_err();
    assert!(diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionPackageInvalid
            && diagnostic
                .message
                .contains("control boundary resolved_manifest")
            && diagnostic
                .message
                .contains("does not match adapter evidence")
    }));
}

#[test]
#[cfg(unix)]
fn npm_store_rejects_intermediate_symlink_escape() {
    use std::os::unix::fs::symlink;

    let temp = tempfile::tempdir().unwrap();
    let project = temp.path().join("project");
    let source = temp.path().join("source");
    let adapter = temp.path().join("adapter");
    let store = temp.path().join("store");
    let external = temp.path().join("external");
    fs::create_dir_all(source.join("skills/review")).unwrap();
    fs::create_dir_all(&adapter).unwrap();
    fs::create_dir_all(&store).unwrap();
    fs::create_dir_all(&external).unwrap();
    fs::write(adapter.join("index.ts"), "x").unwrap();
    fs::write(
        source.join("package.json"),
        r#"{ "name": "guidance", "version": "1.2.3" }"#,
    )
    .unwrap();
    fs::write(source.join("skills/review/SKILL.md"), "# Review\n").unwrap();
    let bundle = r#"{
  "version": "nopal.bundle/v2",
  "packages": [{
    "id": "guidance",
    "source": { "type": "npm", "package": "guidance", "registry": "https://registry.npmjs.org" },
    "requirement": "=1.2.3",
    "resources": [{ "kind": "skill", "path": "skills/review" }]
  }]
}"#;
    let sri = format!(
        "sha512-{}",
        base64::engine::general_purpose::STANDARD.encode([0_u8; 64])
    );
    let lock = distribution::build_lock_from_resolved_sources(
        &project,
        bundle,
        &BuiltinDistribution {
            version: "0.3.0",
            root: &adapter,
        },
        &[NpmResolution {
            package_id: "guidance".to_owned(),
            resolved: "1.2.3".to_owned(),
            artifact_integrity: sri.clone(),
            root: source.clone(),
        }],
    )
    .unwrap();
    let expected = distribution::npm_store_path(&store, "guidance", "1.2.3", &sri);
    let escaped = external.join(expected.file_name().unwrap());
    fs::create_dir_all(escaped.join("skills/review")).unwrap();
    fs::copy(source.join("package.json"), escaped.join("package.json")).unwrap();
    fs::copy(
        source.join("skills/review/SKILL.md"),
        escaped.join("skills/review/SKILL.md"),
    )
    .unwrap();
    symlink(&external, store.join("npm")).unwrap();

    let report = distribution::inspect_texts(
        DistributionContext {
            project_root: &project,
            store_root: &store,
            builtin: BuiltinDistribution {
                version: "0.3.0",
                root: &adapter,
            },
        },
        bundle,
        &distribution::lock_json(&lock).unwrap(),
    )
    .unwrap();
    assert!(!report.ok);
    assert!(report.diagnostics.iter().any(|diagnostic| {
        diagnostic.code == nopal_core::diagnostics::Code::DistributionPackageInvalid
            && diagnostic
                .message
                .contains("control boundary installed_store")
            && diagnostic.message.contains("npm store component")
            && diagnostic.message.contains("symbolic link")
    }));
}
