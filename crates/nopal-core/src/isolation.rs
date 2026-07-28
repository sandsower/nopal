//! Typed projection of Beislið's `agent_isolation` workflow contract.
//!
//! This module validates desired placement and atomic runtime-profile shape.
//! It does not create worktrees, allocate services, execute provider commands,
//! or coordinate sessions. A host may advertise only capability it can prove;
//! unsupported placement therefore remains a deterministic blocked result.

use std::collections::BTreeSet;
use std::path::Path;

use serde::Serialize;

use crate::diagnostics::{Code, Diagnostic};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OrchestratorPlacement {
    Current,
    Native,
    Manual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegatePlacement {
    Native,
    Manual,
    Sequential,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentIsolation {
    pub orchestrator: OrchestratorPlacement,
    pub delegate: DelegatePlacement,
    pub manual_root: String,
    pub runtime_profiles: Vec<RuntimeProfile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeProfile {
    pub name: String,
    pub required_bindings: Vec<String>,
    pub provider: RuntimeProvider,
}

#[derive(Debug, Clone, Serialize)]
pub struct RuntimeProvider {
    pub allocate: String,
    pub verify: String,
    pub release: String,
    pub reconcile: String,
}

pub fn validate(
    value: &serde_json::Value,
    path: &str,
) -> (Option<AgentIsolation>, Vec<Diagnostic>) {
    let mut diagnostics = Vec::new();
    let Some(object) = value.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "agent_isolation must be an object",
        ));
        return (None, diagnostics);
    };

    let orchestrator = match object.get("orchestrator") {
        None => Some(OrchestratorPlacement::Current),
        Some(serde_json::Value::String(value)) => match value.as_str() {
            "current" => Some(OrchestratorPlacement::Current),
            "native" => Some(OrchestratorPlacement::Native),
            "manual" => Some(OrchestratorPlacement::Manual),
            other => {
                diagnostics.push(Diagnostic::error(
                    Code::FieldInvalid,
                    path,
                    format!(
                        "agent_isolation.orchestrator {other:?} must be current, native, or manual"
                    ),
                ));
                None
            }
        },
        Some(_) => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                "agent_isolation.orchestrator must be a string",
            ));
            None
        }
    };
    let delegate = match object.get("delegate") {
        None => Some(DelegatePlacement::Sequential),
        Some(serde_json::Value::String(value)) => match value.as_str() {
            "native" => Some(DelegatePlacement::Native),
            "manual" => Some(DelegatePlacement::Manual),
            "sequential" => Some(DelegatePlacement::Sequential),
            other => {
                diagnostics.push(Diagnostic::error(
                    Code::FieldInvalid,
                    path,
                    format!(
                        "agent_isolation.delegate {other:?} must be native, manual, or sequential"
                    ),
                ));
                None
            }
        },
        Some(_) => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                "agent_isolation.delegate must be a string",
            ));
            None
        }
    };
    let manual_root = match object.get("manual_root") {
        None => Some("repo-sibling".to_owned()),
        Some(serde_json::Value::String(value)) if manual_root_valid(value) => Some(value.clone()),
        Some(serde_json::Value::String(_)) => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                "agent_isolation.manual_root must be repo-sibling or a durable absolute path outside system temporary roots",
            ));
            None
        }
        Some(_) => {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                "agent_isolation.manual_root must be a string",
            ));
            None
        }
    };

    validate_fallback(object.get("fallback"), path, &mut diagnostics);
    validate_preparation(object.get("preparation"), path, &mut diagnostics);
    let runtime_profiles =
        validate_profiles(object.get("runtime_profiles"), path, &mut diagnostics);

    let isolation = diagnostics.is_empty().then(|| {
        orchestrator.zip(delegate).zip(manual_root).map(
            |((orchestrator, delegate), manual_root)| AgentIsolation {
                orchestrator,
                delegate,
                manual_root,
                runtime_profiles,
            },
        )
    });
    (isolation.flatten(), diagnostics)
}

fn manual_root_valid(value: &str) -> bool {
    if value == "repo-sibling" {
        return true;
    }
    let path = Path::new(value);
    if !path.is_absolute() {
        return false;
    }
    !["/tmp", "/private/tmp", "/var/tmp", "/private/var/folders"]
        .iter()
        .any(|root| path == Path::new(root) || path.starts_with(root))
}

fn validate_fallback(
    value: Option<&serde_json::Value>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(value) = value else {
        return;
    };
    let Some(object) = value.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "agent_isolation.fallback must be an object",
        ));
        return;
    };
    if let Some(value) = object.get("orchestrator")
        && value.as_str() != Some("manual-transition-required")
    {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "agent_isolation.fallback.orchestrator must be the string manual-transition-required",
        ));
    }
    if let Some(value) = object.get("delegate")
        && !matches!(value.as_str(), Some("manual" | "sequential"))
    {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "agent_isolation.fallback.delegate must be the string manual or sequential",
        ));
    }
}

fn validate_preparation(
    value: Option<&serde_json::Value>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(value) = value else {
        return;
    };
    let Some(object) = value.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "agent_isolation.preparation must be an object",
        ));
        return;
    };
    if !non_empty(object.get("command")) {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "agent_isolation.preparation.command must be a non-empty string",
        ));
    }
    if let Some(readiness) = object.get("readiness")
        && readiness
            .as_array()
            .is_none_or(|items| items.iter().any(|item| !non_empty(Some(item))))
    {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "agent_isolation.preparation.readiness must be an array of non-empty commands",
        ));
    }
}

fn validate_profiles(
    value: Option<&serde_json::Value>,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Vec<RuntimeProfile> {
    let Some(value) = value else {
        return Vec::new();
    };
    let Some(profiles) = value.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "agent_isolation.runtime_profiles must be an object",
        ));
        return Vec::new();
    };
    let mut output = Vec::new();
    for (name, value) in profiles {
        if !profile_name_valid(name) {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("runtime profile name {name:?} is not a lowercase path-safe segment"),
            ));
        }
        let Some(profile) = value.as_object() else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("runtime profile {name:?} must be an object"),
            ));
            continue;
        };
        let (bindings, bindings_well_typed) = match profile.get("required_bindings") {
            Some(serde_json::Value::Array(items)) => (
                items
                    .iter()
                    .filter_map(serde_json::Value::as_str)
                    .map(str::to_owned)
                    .collect::<Vec<_>>(),
                items.iter().all(serde_json::Value::is_string),
            ),
            _ => (Vec::new(), false),
        };
        let unique = bindings.iter().collect::<BTreeSet<_>>().len() == bindings.len();
        if !bindings_well_typed
            || bindings.is_empty()
            || !unique
            || bindings.iter().any(|binding| !binding_valid(binding))
        {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("runtime profile {name:?} required_bindings must be a non-empty array of unique uppercase environment-name strings"),
            ));
        }
        let Some(provider) = profile
            .get("provider")
            .and_then(serde_json::Value::as_object)
        else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("runtime profile {name:?} requires a provider object"),
            ));
            continue;
        };
        let command = |key: &str| {
            provider
                .get(key)
                .and_then(serde_json::Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .map(str::to_owned)
        };
        let (Some(allocate), Some(verify), Some(release), Some(reconcile)) = (
            command("allocate"),
            command("verify"),
            command("release"),
            command("reconcile"),
        ) else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("runtime profile {name:?} provider requires non-empty allocate, verify, release, and reconcile commands"),
            ));
            continue;
        };
        output.push(RuntimeProfile {
            name: name.clone(),
            required_bindings: bindings,
            provider: RuntimeProvider {
                allocate,
                verify,
                release,
                reconcile,
            },
        });
    }
    output
}

fn non_empty(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(serde_json::Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
}

fn profile_name_valid(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'.' | b'_' | b'-')
        })
        && value
            .as_bytes()
            .first()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
}

fn binding_valid(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_uppercase())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_uppercase() || byte.is_ascii_digit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_atomic_profile_projects_without_secret_values() {
        let value = serde_json::json!({
            "orchestrator": "current",
            "delegate": "manual",
            "manual_root": "repo-sibling",
            "fallback": {
                "orchestrator": "manual-transition-required",
                "delegate": "sequential"
            },
            "runtime_profiles": {
                "integration": {
                    "required_bindings": ["PRIMARY_DATABASE_URL", "REDIS_URL"],
                    "provider": {
                        "allocate": "python3 scripts/runtime.py allocate",
                        "verify": "python3 scripts/runtime.py verify",
                        "release": "python3 scripts/runtime.py release",
                        "reconcile": "python3 scripts/runtime.py reconcile"
                    }
                }
            }
        });
        let (isolation, diagnostics) = validate(&value, "workflow");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        let isolation = isolation.unwrap();
        assert_eq!(isolation.orchestrator, OrchestratorPlacement::Current);
        assert_eq!(isolation.runtime_profiles[0].required_bindings.len(), 2);
        let serialized = serde_json::to_string(&isolation).unwrap();
        assert!(!serialized.contains("postgres://"));
    }

    #[test]
    fn present_malformed_fields_never_downgrade_to_defaults() {
        for value in [
            serde_json::json!({ "orchestrator": 1 }),
            serde_json::json!({ "delegate": {} }),
            serde_json::json!({ "manual_root": ["repo-sibling"] }),
            serde_json::json!({
                "fallback": {
                    "orchestrator": true,
                    "delegate": 1
                }
            }),
            serde_json::json!({
                "runtime_profiles": {
                    "integration": {
                        "required_bindings": ["DATABASE_URL", 7],
                        "provider": {
                            "allocate": "runtime allocate",
                            "verify": "runtime verify",
                            "release": "runtime release",
                            "reconcile": "runtime reconcile"
                        }
                    }
                }
            }),
        ] {
            let (isolation, diagnostics) = validate(&value, "workflow");
            assert!(isolation.is_none(), "{value}: {isolation:?}");
            assert!(!diagnostics.is_empty(), "{value}");
        }
    }

    #[test]
    fn malformed_profile_and_ephemeral_root_fail_closed_together() {
        let value = serde_json::json!({
            "orchestrator": "future",
            "delegate": "parallel",
            "manual_root": "/tmp/worktrees",
            "runtime_profiles": {
                "Bad/Profile": {
                    "required_bindings": ["database_url", "database_url"],
                    "provider": { "allocate": "true" }
                }
            }
        });
        let (_, diagnostics) = validate(&value, "workflow");
        assert!(diagnostics.len() >= 6, "{diagnostics:?}");
    }
}
