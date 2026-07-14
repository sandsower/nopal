//! `nopal.roots/v1`: authoritative requirements and their proof declarations.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::config;
use crate::diagnostics::{Code, Diagnostic};

pub const ROOTS_KIND: &str = "nopal.roots/v1";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootDocument {
    pub version: String,
    pub roots: Vec<RootDeclaration>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootDeclaration {
    pub id: String,
    pub statement: String,
    #[serde(default)]
    pub proof_requirements: Vec<ProofRequirement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProofRequirement {
    pub id: String,
    pub stage: String,
    pub required: bool,
    pub gates: Vec<String>,
    pub on_missing: String,
    pub on_failure: String,
}

pub fn parse_document(text: &str, path: &str) -> (Option<RootDocument>, Vec<Diagnostic>) {
    let value = match config::parse_jsonc(text, path, Code::ModuleParseError) {
        Ok(value) => value,
        Err(diagnostic) => return (None, vec![diagnostic]),
    };
    let diagnostics = validate_document(&value, path);
    if !diagnostics.is_empty() {
        return (None, diagnostics);
    }
    match serde_json::from_value(value) {
        Ok(document) => (Some(document), diagnostics),
        Err(error) => (
            None,
            vec![Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("invalid Roots document: {error}"),
            )],
        ),
    }
}

pub fn validate_document(value: &serde_json::Value, path: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();
    if value.get("version").and_then(serde_json::Value::as_str) != Some(ROOTS_KIND) {
        diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("expected version {ROOTS_KIND:?}"),
        ));
    }
    let Some(roots) = value.get("roots").and_then(serde_json::Value::as_array) else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "\"roots\" must be an array",
        ));
        return diagnostics;
    };
    let mut root_ids = BTreeSet::new();
    for (root_index, root) in roots.iter().enumerate() {
        let context = format!("roots[{root_index}]");
        let Some(root) = root.as_object() else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{context} must be an object"),
            ));
            continue;
        };
        let root_id = nonempty(root.get("id"));
        match root_id {
            Some(root_id) if !root_ids.insert(root_id.to_owned()) => {
                diagnostics.push(Diagnostic::error(
                    Code::DuplicateId,
                    path,
                    format!("duplicate Root id {root_id:?}"),
                ));
            }
            Some(_) => {}
            None => diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{context}.id must be a non-empty string"),
            )),
        }
        if nonempty(root.get("statement")).is_none() {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{context}.statement must be a non-empty string"),
            ));
        }
        let Some(proofs) = root
            .get("proof_requirements")
            .and_then(serde_json::Value::as_array)
        else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{context}.proof_requirements must be an array"),
            ));
            continue;
        };
        let mut proof_ids = BTreeSet::new();
        for (proof_index, proof) in proofs.iter().enumerate() {
            validate_proof(
                proof,
                &format!("{context}.proof_requirements[{proof_index}]"),
                path,
                &mut proof_ids,
                &mut diagnostics,
            );
        }
    }
    diagnostics
}

fn validate_proof(
    proof: &serde_json::Value,
    context: &str,
    path: &str,
    ids: &mut BTreeSet<String>,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(proof) = proof.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{context} must be an object"),
        ));
        return;
    };
    match nonempty(proof.get("id")) {
        Some(id) if !ids.insert(id.to_owned()) => diagnostics.push(Diagnostic::error(
            Code::DuplicateId,
            path,
            format!("duplicate Proof Requirement id {id:?}"),
        )),
        Some(_) => {}
        None => diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{context}.id must be a non-empty string"),
        )),
    }
    if nonempty(proof.get("stage")).is_none() {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{context}.stage must be a non-empty string"),
        ));
    }
    if !proof
        .get("required")
        .is_some_and(serde_json::Value::is_boolean)
    {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{context}.required must be a bool"),
        ));
    }
    match proof.get("gates").and_then(serde_json::Value::as_array) {
        Some(gates)
            if !gates.is_empty() && gates.iter().all(|gate| nonempty(Some(gate)).is_some()) => {}
        _ => diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{context}.gates must contain non-empty gate ids"),
        )),
    }
    for field in ["on_missing", "on_failure"] {
        let valid = proof
            .get(field)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| matches!(value, "block" | "warn" | "ask"));
        if !valid {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{context}.{field} must be one of \"block\", \"warn\", or \"ask\""),
            ));
        }
    }
}

fn nonempty(value: Option<&serde_json::Value>) -> Option<&str> {
    value
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnostics::Code;

    #[test]
    fn valid_roots_preserve_proof_requirements() {
        let text = r#"{
            "version": "nopal.roots/v1",
            "roots": [{
                "id": "repository-quality",
                "statement": "Changes preserve repository quality",
                "proof_requirements": [{
                    "id": "pre-pr",
                    "stage": "pre_pr",
                    "required": true,
                    "gates": ["fmt", "test"],
                    "on_missing": "block",
                    "on_failure": "block"
                }]
            }]
        }"#;

        let (document, diagnostics) = parse_document(text, "roots.jsonc");

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let document = document.unwrap();
        assert_eq!(document.roots.len(), 1);
        assert_eq!(
            document.roots[0].proof_requirements[0].gates,
            ["fmt", "test"]
        );
    }

    #[test]
    fn duplicate_ids_and_invalid_failure_policy_fail_closed() {
        let text = r#"{
            "version": "nopal.roots/v1",
            "roots": [
                {"id":"same","statement":"A","proof_requirements":[]},
                {"id":"same","statement":"B","proof_requirements":[{
                    "id":"proof","stage":"pre_pr","required":true,
                    "gates":[],"on_missing":"guess","on_failure":"block"
                }]}
            ]
        }"#;

        let (_, diagnostics) = parse_document(text, "roots.jsonc");
        let codes: Vec<Code> = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.code)
            .collect();

        assert!(codes.contains(&Code::DuplicateId));
        assert!(codes.contains(&Code::FieldInvalid));
    }
}
