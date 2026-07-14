//! Profiles and Nopal module homes.
//!
//! Profile names and required-module membership are manifest/config data. Nopal
//! keeps only the generic built-in defaults (`minimal`, `portable`) here; product
//! profiles such as `nopal` are declared by project manifests.

use serde::Serialize;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Profile(String);

impl Profile {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Built-in generic profile defaults. Product profile membership belongs in
/// manifest/config data, not compiled Nopal core variants.
pub fn builtin_required_modules(profile: &str) -> Option<&'static [Module]> {
    match profile {
        "minimal" => Some(&[]),
        "portable" => Some(&[Module::Gates, Module::Policy]),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Module {
    Gates,
    Policy,
    Workflow,
    Roots,
    Integrations,
    Guidance,
    ReviewPolicy,
}

impl Module {
    /// Declaration order is the canonical display/report order.
    pub const ALL: [Module; 7] = [
        Module::Gates,
        Module::Policy,
        Module::Workflow,
        Module::Roots,
        Module::Integrations,
        Module::Guidance,
        Module::ReviewPolicy,
    ];

    pub fn parse(s: &str) -> Option<Module> {
        match s {
            "gates" => Some(Module::Gates),
            "policy" => Some(Module::Policy),
            "workflow" => Some(Module::Workflow),
            "roots" => Some(Module::Roots),
            "integrations" => Some(Module::Integrations),
            "guidance" => Some(Module::Guidance),
            "review_policy" => Some(Module::ReviewPolicy),
            _ => None,
        }
    }

    pub fn known_names() -> String {
        Self::ALL
            .iter()
            .map(|module| format!("{:?}", module.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Module::Gates => "gates",
            Module::Policy => "policy",
            Module::Workflow => "workflow",
            Module::Roots => "roots",
            Module::Integrations => "integrations",
            Module::Guidance => "guidance",
            Module::ReviewPolicy => "review_policy",
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            Module::Gates => "gates.jsonc",
            Module::Policy => "policy.jsonc",
            Module::Workflow => "workflow.jsonc",
            Module::Roots => "roots.jsonc",
            Module::Integrations => "integrations.jsonc",
            Module::Guidance => "guidance.jsonc",
            Module::ReviewPolicy => "review_policy.jsonc",
        }
    }
}
