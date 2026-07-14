//! `nopal.guidance/v1` module validation.
//!
//! Guidance is intentionally non-authoritative: it may hint skills, agents,
//! models, context, or local conventions, but cannot define gates, policy,
//! lifecycle/progression, or proof requirements.

use crate::config;
use crate::diagnostics::{Code, Diagnostic};

pub const GUIDANCE_KIND: &str = "nopal.guidance/v1";

const AUTHORITATIVE_KEYS: &[&str] = &[
    "gates",
    "gate_sets",
    "policy",
    "action_policy",
    "workflow",
    "lifecycle",
    "lifecycle_actions",
    "progression",
    "proof_requirements",
];

pub fn validate_document(root: &serde_json::Value, path: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    match root.get("version").and_then(|v| v.as_str()) {
        Some(GUIDANCE_KIND) => {}
        Some(other) => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("unsupported guidance version {other:?}; expected {GUIDANCE_KIND:?}"),
        )),
        None => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("missing string field \"version\"; expected {GUIDANCE_KIND:?}"),
        )),
    }

    reject_authoritative_keys(root, path, "$", &mut diagnostics);
    diagnostics
}

pub fn parse_guidance(text: &str, path: &str) -> (Option<serde_json::Value>, Vec<Diagnostic>) {
    let root = match config::parse_jsonc(text, path, Code::ModuleParseError) {
        Ok(value) => value,
        Err(diagnostic) => return (None, vec![diagnostic]),
    };
    let diagnostics = validate_document(&root, path);
    (Some(root), diagnostics)
}

fn reject_authoritative_keys(
    value: &serde_json::Value,
    path: &str,
    ctx: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match value {
        serde_json::Value::Object(obj) => {
            for (key, child) in obj {
                if AUTHORITATIVE_KEYS.contains(&key.as_str()) {
                    diagnostics.push(Diagnostic::error(
                        Code::GuidanceAuthorityInvalid,
                        path,
                        format!(
                            "guidance key {key:?} at {ctx} is authoritative; use the dedicated Nopal module instead"
                        ),
                    ));
                }
                reject_authoritative_keys(child, path, &format!("{ctx}.{key}"), diagnostics);
            }
        }
        serde_json::Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                reject_authoritative_keys(child, path, &format!("{ctx}[{index}]"), diagnostics);
            }
        }
        serde_json::Value::Null
        | serde_json::Value::Bool(_)
        | serde_json::Value::Number(_)
        | serde_json::Value::String(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(diags: &[Diagnostic]) -> Vec<Code> {
        diags.iter().map(|d| d.code).collect()
    }

    #[test]
    fn valid_hints_are_non_authoritative() {
        let text = r#"{
            "version": "nopal.guidance/v1",
            "hints": {
                "skills": ["blueprint", "implement"],
                "agents": ["domain-expert"],
                "models": { "blueprint": "opus" },
                "context": ["read docs/surface/config-and-envelopes.md"]
            }
        }"#;
        let (_, diags) = parse_guidance(text, "g.jsonc");
        assert_eq!(diags, vec![]);
    }

    #[test]
    fn authoritative_keys_are_errors_even_nested() {
        let text = r#"{
            "version": "nopal.guidance/v1",
            "hints": { "policy": { "modes": {} } },
            "progression": { "next": "implement" }
        }"#;
        let (_, diags) = parse_guidance(text, "g.jsonc");
        assert_eq!(
            codes(&diags),
            vec![
                Code::GuidanceAuthorityInvalid,
                Code::GuidanceAuthorityInvalid
            ]
        );
    }
}
