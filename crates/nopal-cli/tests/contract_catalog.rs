// Integration tests may panic freely; clippy's in-tests allowance only covers
// #[test] fns, not shared helpers in the tests/ tree.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn read_catalog() -> serde_json::Value {
    read_json_path(repo_root().join("contracts/catalog.json"))
}

fn string_field<'a>(entry: &'a serde_json::Value, field: &str) -> &'a str {
    entry[field]
        .as_str()
        .unwrap_or_else(|| panic!("{field} should be a string in {entry:?}"))
}

fn string_array(entry: &serde_json::Value, field: &str) -> Vec<String> {
    entry[field]
        .as_array()
        .unwrap_or_else(|| panic!("{field} should be an array in {entry:?}"))
        .iter()
        .map(|value| {
            value
                .as_str()
                .unwrap_or_else(|| panic!("{field} entries should be strings in {entry:?}"))
                .to_owned()
        })
        .collect()
}

fn read_json_path(path: impl AsRef<Path>) -> serde_json::Value {
    let path = path.as_ref();
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|err| panic!("{} should be readable: {err}", path.display()));
    serde_json::from_str(&text)
        .unwrap_or_else(|err| panic!("{} should parse: {err}", path.display()))
}

fn assert_array_contains(entry: &serde_json::Value, field: &str, expected: &str) {
    assert!(
        string_array(entry, field).contains(&expected.to_owned()),
        "{field} should contain {expected:?} in {entry:?}"
    );
}

#[test]
fn catalog_lists_execution_and_memory_once() {
    let catalog = read_catalog();
    assert_eq!(catalog["kind"], "nopal.contract_catalog/v1");

    let contracts = catalog["contracts"]
        .as_array()
        .expect("contracts should be an array");
    assert_eq!(contracts.len(), 2, "expected exactly 2 contract entries");
    let ids: BTreeSet<&str> = contracts
        .iter()
        .map(|entry| string_field(entry, "id"))
        .collect();

    assert_eq!(ids.len(), contracts.len(), "contract ids should be unique");
    assert_eq!(ids, BTreeSet::from(["execution", "memory"]));

    let former_ids: BTreeSet<&str> = contracts
        .iter()
        .map(|entry| string_field(entry, "former_id"))
        .collect();
    assert_eq!(
        former_ids,
        BTreeSet::from(["C2", "C4"]),
        "each surviving contract should note its former C-number"
    );
}

#[test]
fn every_catalog_entry_points_at_existing_local_notes_and_conformance_home() {
    let root = repo_root();
    let catalog = read_catalog();
    let contracts = catalog["contracts"]
        .as_array()
        .expect("contracts should be an array");

    for entry in contracts {
        let note = string_field(entry, "catalog_note");
        assert!(
            root.join(note).is_file(),
            "catalog note should exist for {}: {note}",
            entry["id"]
        );

        let home = string_field(entry, "conformance_home");
        assert!(
            root.join(home).is_dir(),
            "conformance home should exist for {}: {home}",
            entry["id"]
        );
        assert!(
            Path::new(home).starts_with("conformance"),
            "conformance home should live under conformance/: {home}"
        );
    }
}

#[test]
fn distribution_contracts_are_active() {
    let catalog = read_catalog();
    let contracts = catalog["contracts"]
        .as_array()
        .expect("contracts should be an array");

    for entry in contracts {
        let id = string_field(entry, "id");
        let status = string_field(entry, "status");
        match id {
            "execution" => assert_eq!(status, "active"),
            "memory" => assert_eq!(status, "active"),
            other => panic!("unexpected contract id: {other}"),
        }
    }
}

#[test]
fn local_schema_pointers_exist() {
    let root = repo_root();
    let catalog = read_catalog();
    let contracts = catalog["contracts"]
        .as_array()
        .expect("contracts should be an array");

    for entry in contracts {
        for pointer in string_array(entry, "schema_pointers") {
            if pointer.starts_with("external:") {
                continue;
            }
            assert!(
                root.join(&pointer).exists(),
                "local schema pointer should exist for {}: {pointer}",
                entry["id"]
            );
        }
    }
}

#[test]
fn execution_advertises_rondo_core_v1() {
    let catalog = read_catalog();
    let contracts = catalog["contracts"]
        .as_array()
        .expect("contracts should be an array");
    let execution = contracts
        .iter()
        .find(|entry| string_field(entry, "id") == "execution")
        .expect("catalog should include the execution contract");

    assert_eq!(string_field(execution, "surface"), "rondo.core/v1");
    assert!(
        string_array(execution, "current_versions").contains(&"rondo.core/v1".to_owned()),
        "execution should name the active service contract version"
    );
    assert!(
        string_array(execution, "schema_pointers")
            .iter()
            .any(|pointer| pointer
                == "conformance/execution/schemas/rondo-core-service-v1.schema.json"),
        "execution should point at the active service schema"
    );

    for fixture in [
        "conformance/execution/fixtures/run-events-archived-replay.json",
        "conformance/execution/fixtures/run-events-resume.json",
    ] {
        assert!(
            string_array(execution, "schema_pointers").contains(&fixture.to_owned()),
            "execution should point at the run-events fixture {fixture}"
        );
    }
}

#[test]
fn execution_minimal_fixture_covers_required_boundary_concepts() {
    let root = repo_root();
    let schema = read_json_path(
        root.join("conformance/execution/schemas/rondo-core-service-v1.schema.json"),
    );
    let fixture =
        read_json_path(root.join("conformance/execution/fixtures/minimal-service-contract.json"));

    assert_eq!(schema["properties"]["surface"]["const"], "rondo.core/v1");
    assert_eq!(fixture["kind"], "rondo.core.service_contract/v1");
    assert_eq!(fixture["surface"], "rondo.core/v1");
    assert_eq!(
        string_array(&fixture, "placements"),
        vec![
            "shared_user_runtime",
            "dedicated_repo_runtime",
            "dedicated_run_runtime",
            "blocked"
        ]
    );

    let operations = fixture["operations"]
        .as_object()
        .expect("operations should be an object");
    for operation in ["run.submit", "run.status", "run.events"] {
        assert!(
            operations.contains_key(operation),
            "fixture should include required execution operation {operation}"
        );
    }

    for deferred_operation in [
        "service.start",
        "service.stop",
        "service.restart",
        "service.health",
        "service.status",
    ] {
        assert!(
            !operations.contains_key(deferred_operation),
            "active single-manifest descriptor must not advertise deferred operation {deferred_operation}"
        );
    }

    assert_array_contains(&operations["run.submit"], "request", "manifest_path");
    assert_array_contains(&operations["run.submit"], "request", "manifest_sha256");
    assert_array_contains(&operations["run.submit"], "response", "deduplicated");
    assert_array_contains(&operations["run.status"], "response", "evidence_pointers");
    assert_array_contains(&operations["run.events"], "response", "next_event_cursor");

    let ownership = &fixture["ownership"];
    for rondo_owned in [
        "execution supervision",
        "run ledger",
        "workspaces",
        "agent adapters",
        "artifacts",
    ] {
        assert_array_contains(ownership, "rondo_core_owns", rondo_owned);
    }
    assert_array_contains(
        ownership,
        "nopal_coordinates",
        "policy-gated run submission",
    );
    assert_array_contains(
        ownership,
        "nopal_coordinates",
        "run status, event, and evidence rendering",
    );
    assert_array_contains(
        ownership,
        "nopal_coordinates",
        "opaque run handle preservation",
    );
}
