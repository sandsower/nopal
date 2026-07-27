#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::fs;
use std::path::Path;

use nopal_core::distribution::{self, BuiltinDistribution, DistributionContext, ResourceKind};

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
}
