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
