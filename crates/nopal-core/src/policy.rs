//! `nopal.policy/v1`: action policy decisions and runtime isolation placement.
//!
//! Nopal decides and explains; it never enforces. Given a run mode, an action
//! identity, declared action classes, and referenced env vars, the evaluator
//! returns the matched rules, the winning action decision, and the winning
//! runtime placement, each with an explanation of where it came from.
//!
//! The v1 safety lattices are fixed, while modes and action classes are open
//! data vocabulary:
//!
//! - built-in generic modes: `manual`, `supervised_auto`, `unattended_auto`, `ci`
//! - known action classes: `read`, `workspace_write`, `dependency_install`,
//!   `network_read`, `git_local`, `git_remote`, `destructive`,
//!   `secret_bearing` (names shared with the beislid action-policy contract,
//!   normalized to snake_case)
//! - decisions, most restrictive wins: `deny` > `ask` > `allow`
//! - placements, strongest wins: `blocked` > `dedicated_run_runtime` >
//!   `dedicated_repo_runtime` > `shared_user_runtime`
//!
//! Rule matching is any-of (v1): a rule matches when the action id appears in
//! its `actions` or when any of its `classes` intersects the action's
//! effective classes. Effective classes are the caller-declared classes plus
//! the classes of every referenced env ref classified in the policy document;
//! `secret_bearing` classification rides this env-ref path.
//!
//! When no matched rule sets a decision (or placement), the mode's configured
//! `default_decision` (`default_placement`) applies; absent that, the
//! built-in default for the mode applies. Built-in defaults are part of the
//! v1 contract; `ask` in an unattended mode means interrupt and park for a
//! human, and `ci` denies rather than blocks on one.

use std::io;
use std::path::Path;

use serde::Serialize;

use crate::config;
use crate::diagnostics::{self, Code, Diagnostic, Severity};
use crate::discover;
use crate::profile::Module;
use crate::toon::{self, Value};

pub const POLICY_KIND: &str = "nopal.policy/v1";

// ---------------------------------------------------------------------------
// Open vocabulary + closed safety lattices
// ---------------------------------------------------------------------------

const BUILTIN_MODES: [&str; 4] = ["manual", "supervised_auto", "unattended_auto", "ci"];
const KNOWN_CLASSES: [&str; 9] = [
    "read",
    "workspace_write",
    "dependency_install",
    "network_read",
    "network_write",
    "git_local",
    "git_remote",
    "destructive",
    "secret_bearing",
];
const PROTECTED_CLASSES: [&str; 2] = ["destructive", "secret_bearing"];

/// Interns non-builtin vocabulary tokens so repeated parses of the same
/// unknown token reuse a single allocation. Memory growth is bounded by the
/// number of distinct tokens seen in a process lifetime, not by the number
/// of parse calls over untrusted input.
fn intern(s: String) -> &'static str {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock, PoisonError};

    static INTERNED: OnceLock<Mutex<HashSet<&'static str>>> = OnceLock::new();
    let mut set = INTERNED
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    match set.get(s.as_str()) {
        Some(existing) => existing,
        None => {
            let leaked: &'static str = Box::leak(s.into_boxed_str());
            set.insert(leaked);
            leaked
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct Mode(&'static str);

impl Mode {
    #[allow(non_upper_case_globals)]
    pub const Manual: Mode = Mode("manual");
    #[allow(non_upper_case_globals)]
    pub const SupervisedAuto: Mode = Mode("supervised_auto");
    #[allow(non_upper_case_globals)]
    pub const UnattendedAuto: Mode = Mode("unattended_auto");
    #[allow(non_upper_case_globals)]
    pub const Ci: Mode = Mode("ci");
    pub const ALL: [Mode; 4] = [
        Mode::Manual,
        Mode::SupervisedAuto,
        Mode::UnattendedAuto,
        Mode::Ci,
    ];

    pub fn new(s: impl Into<String>) -> Mode {
        Mode(intern(s.into()))
    }

    pub fn parse(s: &str) -> Option<Mode> {
        (!s.is_empty()).then(|| match s {
            "manual" => Mode::Manual,
            "supervised_auto" => Mode::SupervisedAuto,
            "unattended_auto" => Mode::UnattendedAuto,
            "ci" => Mode::Ci,
            _ => Mode::new(s),
        })
    }

    pub fn as_str(&self) -> &str {
        self.0
    }

    pub fn is_builtin(&self) -> bool {
        BUILTIN_MODES.contains(&self.as_str())
    }

    /// Built-in decision when neither a rule nor a configured mode default
    /// applies. Product-specific and unknown modes are additive data; when no
    /// configured default exists they fall back to automation-safe ask.
    pub fn builtin_default_decision(&self) -> Decision {
        match self.as_str() {
            "manual" => Decision::Allow,
            "supervised_auto" | "unattended_auto" => Decision::Ask,
            "ci" => Decision::Deny,
            _ => Decision::Ask,
        }
    }

    /// Built-in placement when neither a rule nor a configured mode default
    /// applies. Unknown/product modes fall back to at least repo-isolated.
    pub fn builtin_default_placement(&self) -> Placement {
        match self.as_str() {
            "manual" | "supervised_auto" => Placement::SharedUserRuntime,
            "unattended_auto" => Placement::DedicatedRepoRuntime,
            "ci" => Placement::DedicatedRunRuntime,
            _ => Placement::DedicatedRepoRuntime,
        }
    }
}

impl From<&str> for Mode {
    fn from(value: &str) -> Self {
        Mode::new(value)
    }
}

/// Action-policy class vocabulary is open data. Known class names keep stable
/// display/order semantics; unknown class names are treated as protected/unsafe
/// by evaluation so newer safety vocabulary never silently relaxes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ActionClass(&'static str);

impl ActionClass {
    #[allow(non_upper_case_globals)]
    pub const Read: ActionClass = ActionClass("read");
    #[allow(non_upper_case_globals)]
    pub const WorkspaceWrite: ActionClass = ActionClass("workspace_write");
    #[allow(non_upper_case_globals)]
    pub const DependencyInstall: ActionClass = ActionClass("dependency_install");
    #[allow(non_upper_case_globals)]
    pub const NetworkRead: ActionClass = ActionClass("network_read");
    #[allow(non_upper_case_globals)]
    pub const NetworkWrite: ActionClass = ActionClass("network_write");
    #[allow(non_upper_case_globals)]
    pub const GitLocal: ActionClass = ActionClass("git_local");
    #[allow(non_upper_case_globals)]
    pub const GitRemote: ActionClass = ActionClass("git_remote");
    #[allow(non_upper_case_globals)]
    pub const Destructive: ActionClass = ActionClass("destructive");
    #[allow(non_upper_case_globals)]
    pub const SecretBearing: ActionClass = ActionClass("secret_bearing");
    pub const ALL: [ActionClass; 9] = [
        ActionClass::Read,
        ActionClass::WorkspaceWrite,
        ActionClass::DependencyInstall,
        ActionClass::NetworkRead,
        ActionClass::NetworkWrite,
        ActionClass::GitLocal,
        ActionClass::GitRemote,
        ActionClass::Destructive,
        ActionClass::SecretBearing,
    ];

    pub fn new(s: impl Into<String>) -> ActionClass {
        ActionClass(intern(s.into()))
    }

    pub fn parse(s: &str) -> Option<ActionClass> {
        (!s.is_empty()).then(|| match s {
            "read" => ActionClass::Read,
            "workspace_write" => ActionClass::WorkspaceWrite,
            "dependency_install" => ActionClass::DependencyInstall,
            "network_read" => ActionClass::NetworkRead,
            "network_write" => ActionClass::NetworkWrite,
            "git_local" => ActionClass::GitLocal,
            "git_remote" => ActionClass::GitRemote,
            "destructive" => ActionClass::Destructive,
            "secret_bearing" => ActionClass::SecretBearing,
            _ => ActionClass::new(s),
        })
    }

    pub fn as_str(&self) -> &str {
        self.0
    }

    pub fn is_known(&self) -> bool {
        KNOWN_CLASSES.contains(&self.as_str())
    }

    pub fn is_protected_or_unknown(&self) -> bool {
        !self.is_known() || PROTECTED_CLASSES.contains(&self.as_str())
    }
}

impl From<&str> for ActionClass {
    fn from(value: &str) -> Self {
        ActionClass::new(value)
    }
}

/// Variant order is the restrictiveness order: `deny` > `ask` > `allow`.
/// The most restrictive matched decision wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    Allow,
    Ask,
    Deny,
}

impl Decision {
    pub fn parse(s: &str) -> Option<Decision> {
        match s {
            "allow" => Some(Decision::Allow),
            "ask" => Some(Decision::Ask),
            "deny" => Some(Decision::Deny),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Decision::Allow => "allow",
            Decision::Ask => "ask",
            Decision::Deny => "deny",
        }
    }
}

/// Variant order is the strength order, weakest first: `shared_user_runtime`,
/// `dedicated_repo_runtime`, `dedicated_run_runtime`, `blocked`. The
/// strongest matched placement wins.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Placement {
    SharedUserRuntime,
    DedicatedRepoRuntime,
    DedicatedRunRuntime,
    Blocked,
}

impl Placement {
    pub fn parse(s: &str) -> Option<Placement> {
        match s {
            "shared_user_runtime" => Some(Placement::SharedUserRuntime),
            "dedicated_repo_runtime" => Some(Placement::DedicatedRepoRuntime),
            "dedicated_run_runtime" => Some(Placement::DedicatedRunRuntime),
            "blocked" => Some(Placement::Blocked),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Placement::SharedUserRuntime => "shared_user_runtime",
            Placement::DedicatedRepoRuntime => "dedicated_repo_runtime",
            Placement::DedicatedRunRuntime => "dedicated_run_runtime",
            Placement::Blocked => "blocked",
        }
    }
}

pub fn known_modes() -> String {
    quoted_list(BUILTIN_MODES.into_iter())
}

pub fn known_classes() -> String {
    quoted_list(KNOWN_CLASSES.into_iter())
}

fn known_decisions() -> String {
    quoted_list(["allow", "ask", "deny"].into_iter())
}

fn known_placements() -> String {
    quoted_list(
        [
            "shared_user_runtime",
            "dedicated_repo_runtime",
            "dedicated_run_runtime",
            "blocked",
        ]
        .into_iter(),
    )
}

fn quoted_list<'a>(items: impl Iterator<Item = &'a str>) -> String {
    items
        .map(|s| format!("{s:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn sort_classes(classes: &mut [ActionClass]) {
    classes.sort_by(|a, b| class_sort_key(a).cmp(&class_sort_key(b)));
}

fn class_sort_key(class: &ActionClass) -> (usize, &str) {
    match KNOWN_CLASSES
        .iter()
        .position(|known| *known == class.as_str())
    {
        Some(index) => (index, ""),
        None => (KNOWN_CLASSES.len(), class.as_str()),
    }
}

// ---------------------------------------------------------------------------
// Document model
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct PolicyDoc {
    pub modes: Vec<(Mode, ModePolicy)>,
    pub env: Vec<EnvRef>,
}

#[derive(Debug, Clone, Default)]
pub struct ModePolicy {
    pub default_decision: Option<Decision>,
    pub default_placement: Option<Placement>,
    pub rules: Vec<Rule>,
}

#[derive(Debug, Clone)]
pub struct Rule {
    pub id: String,
    pub decision: Option<Decision>,
    pub placement: Option<Placement>,
    pub classes: Vec<ActionClass>,
    pub actions: Vec<String>,
    pub reason: Option<String>,
}

/// Structured env ref: an env var name with its explicit class
/// classification. An empty class list means "known and benign".
#[derive(Debug, Clone)]
pub struct EnvRef {
    pub name: String,
    pub classes: Vec<ActionClass>,
}

// ---------------------------------------------------------------------------
// Document validation
// ---------------------------------------------------------------------------

/// Validate a parsed `.nopal/policy.jsonc` value against the nopal.policy/v1
/// schema. Diagnostic-accumulating like the manifest parser: everything
/// understandable comes back as a best-effort document alongside every
/// problem found. Unknown keys are warnings (typo detection); everything
/// else wrong is an error.
pub fn validate_document(
    root: &serde_json::Value,
    path: &str,
) -> (Option<PolicyDoc>, Vec<Diagnostic>) {
    let mut diags = Vec::new();

    let Some(obj) = root.as_object() else {
        diags.push(Diagnostic::error(
            Code::PolicyShapeInvalid,
            path,
            "policy document must be a JSON object",
        ));
        return (None, diags);
    };

    warn_unknown_keys(obj, &["version", "modes", "env"], None, path, &mut diags);

    match obj.get("version").and_then(|v| v.as_str()) {
        Some(POLICY_KIND) => {}
        Some(other) => diags.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("unsupported policy version {other:?}; expected {POLICY_KIND:?}"),
        )),
        None => diags.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("missing string field \"version\"; expected {POLICY_KIND:?}"),
        )),
    }

    let mut modes = Vec::new();
    match obj.get("modes") {
        Some(serde_json::Value::Object(map)) => {
            for (mode_name, mode_value) in map {
                let Some(mode) = Mode::parse(mode_name) else {
                    diags.push(Diagnostic::error(
                        Code::PolicyModeUnknown,
                        path,
                        format!(
                            "unknown mode {mode_name:?}; expected one of {}",
                            known_modes()
                        ),
                    ));
                    continue;
                };
                if let Some(policy) = parse_mode_policy(mode_value, mode_name, path, &mut diags) {
                    modes.push((mode, policy));
                }
            }
        }
        Some(_) => diags.push(Diagnostic::error(
            Code::PolicyShapeInvalid,
            path,
            "\"modes\" must be an object keyed by mode name",
        )),
        None => diags.push(Diagnostic::error(
            Code::PolicyShapeInvalid,
            path,
            "missing object field \"modes\"",
        )),
    }

    let env = parse_env_refs(obj.get("env"), path, &mut diags);

    (Some(PolicyDoc { modes, env }), diags)
}

fn warn_unknown_keys(
    obj: &serde_json::Map<String, serde_json::Value>,
    allowed: &[&str],
    ctx: Option<&str>,
    path: &str,
    diags: &mut Vec<Diagnostic>,
) {
    let expected = allowed
        .iter()
        .map(|key| format!("{key:?}"))
        .collect::<Vec<_>>()
        .join(", ");
    for key in obj.keys() {
        if !allowed.contains(&key.as_str()) {
            let location = ctx.map_or(String::new(), |ctx| format!(" in {ctx}"));
            diags.push(Diagnostic::warning(
                Code::PolicyKeyUnknown,
                path,
                format!("unknown key {key:?}{location}; expected {expected}"),
            ));
        }
    }
}

fn parse_mode_policy(
    value: &serde_json::Value,
    mode_name: &str,
    path: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<ModePolicy> {
    let ctx = format!("modes.{mode_name}");
    let Some(obj) = value.as_object() else {
        diags.push(Diagnostic::error(
            Code::PolicyShapeInvalid,
            path,
            format!("{ctx} must be an object"),
        ));
        return None;
    };

    warn_unknown_keys(
        obj,
        &["default_decision", "default_placement", "rules"],
        Some(&ctx),
        path,
        diags,
    );

    let default_decision = match obj.get("default_decision") {
        None => None,
        Some(value) => match value.as_str().and_then(Decision::parse) {
            Some(decision) => Some(decision),
            None => {
                diags.push(Diagnostic::error(
                    Code::PolicyDecisionInvalid,
                    path,
                    format!(
                        "{ctx}.default_decision must be one of {}, got {value}",
                        known_decisions()
                    ),
                ));
                None
            }
        },
    };

    let default_placement = match obj.get("default_placement") {
        None => None,
        Some(value) => match value.as_str().and_then(Placement::parse) {
            Some(placement) => Some(placement),
            None => {
                diags.push(Diagnostic::error(
                    Code::PolicyPlacementInvalid,
                    path,
                    format!(
                        "{ctx}.default_placement must be one of {}, got {value}",
                        known_placements()
                    ),
                ));
                None
            }
        },
    };

    let mut rules = Vec::new();
    let mut seen_ids: Vec<String> = Vec::new();
    match obj.get("rules") {
        None => {}
        Some(serde_json::Value::Array(entries)) => {
            for (index, entry) in entries.iter().enumerate() {
                if let Some(rule) = parse_rule(entry, &format!("{ctx}.rules[{index}]"), path, diags)
                {
                    if seen_ids.contains(&rule.id) {
                        diags.push(Diagnostic::error(
                            Code::PolicyRuleDuplicateId,
                            path,
                            format!("{ctx}.rules[{index}]: duplicate rule id {:?}", rule.id),
                        ));
                        continue;
                    }
                    seen_ids.push(rule.id.clone());
                    rules.push(rule);
                }
            }
        }
        Some(_) => diags.push(Diagnostic::error(
            Code::PolicyShapeInvalid,
            path,
            format!("{ctx}.rules must be an array"),
        )),
    }

    Some(ModePolicy {
        default_decision,
        default_placement,
        rules,
    })
}

fn parse_rule(
    value: &serde_json::Value,
    ctx: &str,
    path: &str,
    diags: &mut Vec<Diagnostic>,
) -> Option<Rule> {
    let Some(obj) = value.as_object() else {
        diags.push(Diagnostic::error(
            Code::PolicyRuleInvalid,
            path,
            format!("{ctx} must be an object"),
        ));
        return None;
    };

    warn_unknown_keys(
        obj,
        &[
            "id",
            "decision",
            "placement",
            "classes",
            "actions",
            "reason",
        ],
        Some(ctx),
        path,
        diags,
    );

    let id = match obj.get("id").and_then(|v| v.as_str()) {
        Some(id) if !id.is_empty() => id.to_owned(),
        _ => {
            diags.push(Diagnostic::error(
                Code::PolicyRuleInvalid,
                path,
                format!("{ctx}: missing non-empty string field \"id\""),
            ));
            return None;
        }
    };

    let decision = match obj.get("decision") {
        None => None,
        Some(value) => match value.as_str().and_then(Decision::parse) {
            Some(decision) => Some(decision),
            None => {
                diags.push(Diagnostic::error(
                    Code::PolicyDecisionInvalid,
                    path,
                    format!(
                        "{ctx}.decision must be one of {}, got {value}",
                        known_decisions()
                    ),
                ));
                None
            }
        },
    };

    let placement = match obj.get("placement") {
        None => None,
        Some(value) => match value.as_str().and_then(Placement::parse) {
            Some(placement) => Some(placement),
            None => {
                diags.push(Diagnostic::error(
                    Code::PolicyPlacementInvalid,
                    path,
                    format!(
                        "{ctx}.placement must be one of {}, got {value}",
                        known_placements()
                    ),
                ));
                None
            }
        },
    };

    let diags_before_matchers = diags.len();
    let classes = parse_class_list(obj.get("classes"), ctx, path, diags);
    let actions = parse_action_list(obj.get("actions"), ctx, path, diags);

    // Only flag an empty matcher set when the entries themselves were fine;
    // an unknown class or malformed action id is already reported above.
    if classes.is_empty() && actions.is_empty() && diags.len() == diags_before_matchers {
        diags.push(Diagnostic::error(
            Code::PolicyRuleInvalid,
            path,
            format!("{ctx}: rule matches nothing; declare \"classes\" and/or \"actions\""),
        ));
    }
    if !obj.contains_key("decision") && !obj.contains_key("placement") {
        diags.push(Diagnostic::error(
            Code::PolicyRuleInvalid,
            path,
            format!("{ctx}: rule has no effect; declare \"decision\" and/or \"placement\""),
        ));
    }

    let reason = obj
        .get("reason")
        .and_then(|v| v.as_str())
        .map(str::to_owned);

    Some(Rule {
        id,
        decision,
        placement,
        classes,
        actions,
        reason,
    })
}

fn parse_class_list(
    value: Option<&serde_json::Value>,
    ctx: &str,
    path: &str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<ActionClass> {
    let mut classes = Vec::new();
    match value {
        None => {}
        Some(serde_json::Value::Array(entries)) => {
            for entry in entries {
                match entry.as_str().and_then(ActionClass::parse) {
                    Some(class) => {
                        if !class.is_known() {
                            diags.push(Diagnostic::warning(
                                Code::PolicyClassUnknown,
                                path,
                                format!(
                                    "{ctx}: unknown class {entry}; treating as protected/unsafe policy data"
                                ),
                            ));
                        }
                        if !classes.contains(&class) {
                            classes.push(class);
                        }
                    }
                    None => diags.push(Diagnostic::error(
                        Code::PolicyClassUnknown,
                        path,
                        format!(
                            "{ctx}: unknown class {entry}; expected one of {}",
                            known_classes()
                        ),
                    )),
                }
            }
        }
        Some(_) => diags.push(Diagnostic::error(
            Code::PolicyShapeInvalid,
            path,
            format!("{ctx}.classes must be an array of class names"),
        )),
    }
    classes
}

fn parse_action_list(
    value: Option<&serde_json::Value>,
    ctx: &str,
    path: &str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<String> {
    let mut actions = Vec::new();
    match value {
        None => {}
        Some(serde_json::Value::Array(entries)) => {
            for entry in entries {
                match entry.as_str() {
                    Some(action) if !action.is_empty() => {
                        if !actions.iter().any(|a| a == action) {
                            actions.push(action.to_owned());
                        }
                    }
                    _ => diags.push(Diagnostic::error(
                        Code::PolicyRuleInvalid,
                        path,
                        format!("{ctx}.actions entries must be non-empty strings, got {entry}"),
                    )),
                }
            }
        }
        Some(_) => diags.push(Diagnostic::error(
            Code::PolicyShapeInvalid,
            path,
            format!("{ctx}.actions must be an array of action ids"),
        )),
    }
    actions
}

fn parse_env_refs(
    value: Option<&serde_json::Value>,
    path: &str,
    diags: &mut Vec<Diagnostic>,
) -> Vec<EnvRef> {
    let mut env = Vec::new();
    let Some(value) = value else {
        return env;
    };
    let Some(entries) = value.as_array() else {
        diags.push(Diagnostic::error(
            Code::PolicyShapeInvalid,
            path,
            "\"env\" must be an array of env refs",
        ));
        return env;
    };

    for (index, entry) in entries.iter().enumerate() {
        let ctx = format!("env[{index}]");
        let Some(obj) = entry.as_object() else {
            diags.push(Diagnostic::error(
                Code::PolicyEnvInvalid,
                path,
                format!("{ctx} must be an object with \"name\" and \"classes\""),
            ));
            continue;
        };
        warn_unknown_keys(obj, &["name", "classes"], Some(&ctx), path, diags);
        let name = match obj.get("name").and_then(|v| v.as_str()) {
            Some(name) if !name.is_empty() => name.to_owned(),
            _ => {
                diags.push(Diagnostic::error(
                    Code::PolicyEnvInvalid,
                    path,
                    format!("{ctx}: missing non-empty string field \"name\""),
                ));
                continue;
            }
        };
        if env.iter().any(|e: &EnvRef| e.name == name) {
            diags.push(Diagnostic::error(
                Code::PolicyEnvInvalid,
                path,
                format!("{ctx}: duplicate env ref {name:?}"),
            ));
            continue;
        }
        if !obj.contains_key("classes") {
            diags.push(Diagnostic::error(
                Code::PolicyEnvInvalid,
                path,
                format!(
                    "{ctx}: missing \"classes\"; classify explicitly (an empty array means \
                     known and benign)"
                ),
            ));
            continue;
        }
        let classes = parse_class_list(obj.get("classes"), &ctx, path, diags);
        env.push(EnvRef { name, classes });
    }
    env
}

// ---------------------------------------------------------------------------
// Loading
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct PolicyLoad {
    pub doc: Option<PolicyDoc>,
    pub diagnostics: Vec<Diagnostic>,
}

/// Load and validate `.nopal/policy.jsonc` from a project root. Policy
/// evaluation depends only on the policy module, deliberately: a typo in
/// guidance.jsonc must never flip an action verdict.
pub fn load(root: &Path) -> io::Result<PolicyLoad> {
    let rel = discover::module_rel_path(Module::Policy);
    let text = match std::fs::read_to_string(discover::module_path(root, Module::Policy)) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => {
            return Ok(PolicyLoad {
                doc: None,
                diagnostics: vec![Diagnostic::error(
                    Code::ModuleMissing,
                    rel.clone(),
                    format!("policy evaluation requires {rel}"),
                )],
            });
        }
        Err(err) => return Err(err),
    };

    let value = match config::parse_jsonc(&text, &rel, Code::ModuleParseError) {
        Ok(value) => value,
        Err(diagnostic) => {
            return Ok(PolicyLoad {
                doc: None,
                diagnostics: vec![diagnostic],
            });
        }
    };

    let (doc, mut diags) = validate_document(&value, &rel);
    diagnostics::sort(&mut diags);
    Ok(PolicyLoad {
        doc,
        diagnostics: diags,
    })
}

// ---------------------------------------------------------------------------
// Evaluation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    Rule,
    ModeDefault,
    BuiltinDefault,
    SafetyFloor,
}

impl Source {
    pub fn as_str(self) -> &'static str {
        match self {
            Source::Rule => "rule",
            Source::ModeDefault => "mode_default",
            Source::BuiltinDefault => "builtin_default",
            Source::SafetyFloor => "safety_floor",
        }
    }
}

#[derive(Debug)]
pub struct EvalRequest<'a> {
    pub mode: Mode,
    pub action: &'a str,
    pub classes: &'a [ActionClass],
    pub env: &'a [String],
}

#[derive(Debug, Clone, Serialize)]
pub struct EnvRefReport {
    pub name: String,
    pub known: bool,
    pub classes: Vec<ActionClass>,
}

#[derive(Debug, Clone, Serialize)]
pub struct MatchedRule {
    pub id: String,
    /// What matched: `action <id>` or `class <name>`.
    pub via: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug)]
pub struct Evaluation {
    pub mode: Mode,
    pub action: String,
    /// Effective classes: declared plus env-ref classified, vocabulary order.
    pub classes: Vec<ActionClass>,
    pub env: Vec<EnvRefReport>,
    pub matched_rules: Vec<MatchedRule>,
    pub decision: Decision,
    pub decision_source: Source,
    pub placement: Placement,
    pub placement_source: Source,
    /// Provenance of each effective class and each unclassified env ref.
    pub class_notes: Vec<String>,
}

pub fn evaluate(doc: &PolicyDoc, req: &EvalRequest) -> Evaluation {
    let mut env_reports: Vec<EnvRefReport> = Vec::new();
    for name in req.env {
        if env_reports.iter().any(|e| &e.name == name) {
            continue;
        }
        match doc.env.iter().find(|e| &e.name == name) {
            Some(env_ref) => env_reports.push(EnvRefReport {
                name: name.clone(),
                known: true,
                classes: env_ref.classes.clone(),
            }),
            None => env_reports.push(EnvRefReport {
                name: name.clone(),
                known: false,
                classes: Vec::new(),
            }),
        }
    }

    let mut classes: Vec<ActionClass> = Vec::new();
    let mut class_notes: Vec<String> = Vec::new();
    for class in req.classes {
        if !classes.contains(class) {
            classes.push(*class);
            class_notes.push(format!("class {}: declared by caller", class.as_str()));
        }
    }
    for env_report in &env_reports {
        if !env_report.known {
            class_notes.push(format!(
                "env ref {}: not classified in policy env refs",
                env_report.name
            ));
            continue;
        }
        for class in &env_report.classes {
            if !classes.contains(class) {
                classes.push(*class);
                class_notes.push(format!(
                    "class {}: added by env ref {}",
                    class.as_str(),
                    env_report.name
                ));
            }
        }
    }
    sort_classes(&mut classes);

    let mode_policy = doc
        .modes
        .iter()
        .find(|(mode, _)| *mode == req.mode)
        .map(|(_, policy)| policy);

    let mut matched_rules = Vec::new();
    for rule in mode_policy.map_or(&[][..], |p| p.rules.as_slice()) {
        let via = if rule.actions.iter().any(|a| a == req.action) {
            Some(format!("action {}", req.action))
        } else {
            rule.classes
                .iter()
                .find(|c| classes.contains(c))
                .map(|c| format!("class {}", c.as_str()))
        };
        if let Some(via) = via {
            matched_rules.push(MatchedRule {
                id: rule.id.clone(),
                via,
                decision: rule.decision,
                placement: rule.placement,
                reason: rule.reason.clone(),
            });
        }
    }

    let (mut decision, mut decision_source) =
        match matched_rules.iter().filter_map(|r| r.decision).max() {
            Some(decision) => (decision, Source::Rule),
            None => match mode_policy.and_then(|p| p.default_decision) {
                Some(decision) => (decision, Source::ModeDefault),
                None => (req.mode.builtin_default_decision(), Source::BuiltinDefault),
            },
        };
    let (mut placement, mut placement_source) =
        match matched_rules.iter().filter_map(|r| r.placement).max() {
            Some(placement) => (placement, Source::Rule),
            None => match mode_policy.and_then(|p| p.default_placement) {
                Some(placement) => (placement, Source::ModeDefault),
                None => (req.mode.builtin_default_placement(), Source::BuiltinDefault),
            },
        };

    if req.action == "git.push_force" {
        decision_source = Source::SafetyFloor;
        placement_source = Source::SafetyFloor;
        decision = decision.max(Decision::Deny);
        placement = placement.max(Placement::Blocked);
        class_notes.push(
            "action git.push_force: safety floor keeps decision deny and placement blocked"
                .to_owned(),
        );
    }

    for class in &classes {
        match class.as_str() {
            "destructive" => {
                if decision < Decision::Deny {
                    decision_source = Source::SafetyFloor;
                }
                if placement < Placement::Blocked {
                    placement_source = Source::SafetyFloor;
                }
                decision = decision.max(Decision::Deny);
                placement = placement.max(Placement::Blocked);
                class_notes.push(
                    "class destructive: safety floor keeps decision deny and placement blocked"
                        .to_owned(),
                );
            }
            "secret_bearing" => {
                if placement < Placement::DedicatedRunRuntime {
                    placement_source = Source::SafetyFloor;
                }
                placement = placement.max(Placement::DedicatedRunRuntime);
                class_notes.push(
                    "class secret_bearing: safety floor keeps placement at least dedicated_run_runtime"
                        .to_owned(),
                );
            }
            _ if !class.is_known() => {
                if decision < Decision::Deny {
                    decision_source = Source::SafetyFloor;
                }
                if placement < Placement::DedicatedRunRuntime {
                    placement_source = Source::SafetyFloor;
                }
                decision = decision.max(Decision::Deny);
                placement = placement.max(Placement::DedicatedRunRuntime);
                class_notes.push(format!(
                    "class {}: unknown safety vocabulary treated as protected/unsafe",
                    class.as_str()
                ));
            }
            _ => {}
        }
    }

    Evaluation {
        mode: req.mode,
        action: req.action.to_owned(),
        classes,
        env: env_reports,
        matched_rules,
        decision,
        decision_source,
        placement,
        placement_source,
        class_notes,
    }
}

// ---------------------------------------------------------------------------
// Report envelopes
// ---------------------------------------------------------------------------

/// Which policy command is being rendered; selects the envelope kind and
/// whether decision and/or placement verdicts appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum View {
    Evaluate,
    Placement,
    Decide,
}

impl View {
    pub fn kind(self) -> &'static str {
        match self {
            View::Evaluate => "nopal.policy_evaluation/v1",
            View::Placement => "nopal.policy_placement/v1",
            View::Decide => "nopal.policy_decision/v1",
        }
    }

    fn includes_decision(self) -> bool {
        matches!(self, View::Evaluate | View::Decide)
    }

    fn includes_placement(self) -> bool {
        matches!(self, View::Placement | View::Decide)
    }
}

/// One envelope per policy command. `ok: false` means the policy module was
/// missing or invalid; verdict fields are then absent and `diagnostics` says
/// why. Exit codes stay config-validity-only; verdicts live in the payload.
#[derive(Debug, Serialize)]
pub struct PolicyReport {
    pub kind: &'static str,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub classes: Option<Vec<ActionClass>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub env: Option<Vec<EnvRefReport>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub decision_source: Option<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement: Option<Placement>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placement_source: Option<Source>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_rules: Option<Vec<MatchedRule>>,
    pub diagnostics: Vec<Diagnostic>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub explanation: Option<Vec<String>>,
}

/// Load, evaluate, and build the report for one policy command.
pub fn run(root: &Path, view: View, req: &EvalRequest) -> io::Result<PolicyReport> {
    let load = load(root)?;
    let has_errors = load
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error);
    let doc = match (&load.doc, has_errors) {
        (Some(doc), false) => doc,
        _ => {
            return Ok(PolicyReport {
                kind: view.kind(),
                ok: false,
                mode: None,
                action: None,
                classes: None,
                env: None,
                decision: None,
                decision_source: None,
                placement: None,
                placement_source: None,
                matched_rules: None,
                diagnostics: load.diagnostics,
                explanation: None,
            });
        }
    };

    let evaluation = evaluate(doc, req);
    let mut diagnostics = load.diagnostics;
    if !req.mode.is_builtin() && !doc.modes.iter().any(|(mode, _)| mode == &req.mode) {
        diagnostics.push(Diagnostic::warning(
            Code::PolicyModeUnknown,
            discover::module_rel_path(Module::Policy),
            format!(
                "unknown mode {:?}; using automation-safe fallback defaults",
                req.mode.as_str()
            ),
        ));
    }
    for class in req.classes.iter().filter(|class| !class.is_known()) {
        diagnostics.push(Diagnostic::warning(
            Code::PolicyClassUnknown,
            discover::module_rel_path(Module::Policy),
            format!(
                "unknown class {:?}; treating as protected/unsafe",
                class.as_str()
            ),
        ));
    }
    diagnostics::sort(&mut diagnostics);
    let mut explanation = evaluation.class_notes.clone();
    if view.includes_decision() {
        explanation.push(explain(
            "decision",
            evaluation.decision.as_str(),
            evaluation.decision_source,
            &evaluation.mode,
            "most restrictive decision from matched rules",
        ));
    }
    if view.includes_placement() {
        explanation.push(explain(
            "placement",
            evaluation.placement.as_str(),
            evaluation.placement_source,
            &evaluation.mode,
            "strongest placement from matched rules",
        ));
    }

    Ok(PolicyReport {
        kind: view.kind(),
        ok: true,
        mode: Some(evaluation.mode),
        action: Some(evaluation.action),
        classes: Some(evaluation.classes),
        env: Some(evaluation.env),
        decision: view.includes_decision().then_some(evaluation.decision),
        decision_source: view
            .includes_decision()
            .then_some(evaluation.decision_source),
        placement: view.includes_placement().then_some(evaluation.placement),
        placement_source: view
            .includes_placement()
            .then_some(evaluation.placement_source),
        matched_rules: Some(evaluation.matched_rules),
        diagnostics,
        explanation: Some(explanation),
    })
}

fn explain(what: &str, verdict: &str, source: Source, mode: &Mode, rule_phrase: &str) -> String {
    match source {
        Source::Rule => format!("{what} {verdict}: {rule_phrase}"),
        Source::ModeDefault => format!(
            "{what} {verdict}: configured default for mode {} (no matched rule sets a {what})",
            mode.as_str()
        ),
        Source::BuiltinDefault => format!(
            "{what} {verdict}: built-in default for mode {} (no matched rule or configured \
             default)",
            mode.as_str()
        ),
        Source::SafetyFloor => format!("{what} {verdict}: protected safety floor"),
    }
}

pub fn report_toon(report: &PolicyReport) -> String {
    let mut doc: Vec<(String, Value)> = vec![
        ("kind".into(), Value::str(report.kind)),
        ("ok".into(), Value::Bool(report.ok)),
    ];
    if let Some(mode) = &report.mode {
        doc.push(("mode".into(), Value::str(mode.as_str())));
    }
    if let Some(action) = &report.action {
        doc.push(("action".into(), Value::str(action.clone())));
    }
    if let Some(classes) = &report.classes {
        doc.push((
            "classes".into(),
            Value::Arr(classes.iter().map(|c| Value::str(c.as_str())).collect()),
        ));
    }
    if let Some(env) = &report.env {
        doc.push(("env".into(), env_table(env)));
    }
    if let Some(decision) = report.decision {
        doc.push(("decision".into(), Value::str(decision.as_str())));
    }
    if let Some(source) = report.decision_source {
        doc.push(("decision_source".into(), Value::str(source.as_str())));
    }
    if let Some(placement) = report.placement {
        doc.push(("placement".into(), Value::str(placement.as_str())));
    }
    if let Some(source) = report.placement_source {
        doc.push(("placement_source".into(), Value::str(source.as_str())));
    }
    if let Some(matched) = &report.matched_rules {
        doc.push(("matched_rules".into(), matched_rules_table(matched)));
    }
    doc.push((
        "diagnostics".into(),
        diagnostics::toon_table(&report.diagnostics),
    ));
    if let Some(explanation) = &report.explanation {
        doc.push((
            "explanation".into(),
            Value::Arr(explanation.iter().map(Value::str).collect()),
        ));
    }
    toon::encode(&doc)
}

fn env_table(env: &[EnvRefReport]) -> Value {
    Value::Table {
        fields: vec!["name".into(), "known".into(), "classes".into()],
        rows: env
            .iter()
            .map(|e| {
                vec![
                    Value::str(e.name.clone()),
                    Value::Bool(e.known),
                    Value::str(join_classes(&e.classes)),
                ]
            })
            .collect(),
    }
}

fn matched_rules_table(matched: &[MatchedRule]) -> Value {
    Value::Table {
        fields: vec![
            "id".into(),
            "via".into(),
            "decision".into(),
            "placement".into(),
            "reason".into(),
        ],
        rows: matched
            .iter()
            .map(|r| {
                vec![
                    Value::str(r.id.clone()),
                    Value::str(r.via.clone()),
                    Value::str(r.decision.map_or("-", Decision::as_str)),
                    Value::str(r.placement.map_or("-", Placement::as_str)),
                    Value::str(r.reason.clone().unwrap_or_else(|| "-".to_owned())),
                ]
            })
            .collect(),
    }
}

/// Cells are scalars; joined with `+` so class lists stay unquoted.
fn join_classes(classes: &[ActionClass]) -> String {
    if classes.is_empty() {
        return "-".to_owned();
    }
    classes
        .iter()
        .map(|c| c.as_str())
        .collect::<Vec<_>>()
        .join("+")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc_from(text: &str) -> (PolicyDoc, Vec<Diagnostic>) {
        let value = config::parse_jsonc(text, "p.jsonc", Code::ModuleParseError).unwrap();
        let (doc, diags) = validate_document(&value, ".nopal/policy.jsonc");
        (doc.unwrap(), diags)
    }

    fn codes(diags: &[Diagnostic]) -> Vec<Code> {
        diags.iter().map(|d| d.code).collect()
    }

    const VALID: &str = r#"{
        "version": "nopal.policy/v1",
        "modes": {
            "rondo_afk": {
                "default_decision": "ask",
                "default_placement": "dedicated_repo_runtime",
                "rules": [
                    { "id": "allow-push", "actions": ["git.push"], "decision": "allow" },
                    { "id": "isolate-secrets", "classes": ["secret_bearing"],
                      "placement": "dedicated_run_runtime", "reason": "keep secrets contained" },
                    { "id": "deny-destructive", "classes": ["destructive"], "decision": "deny" }
                ]
            }
        },
        "env": [
            { "name": "API_TOKEN", "classes": ["secret_bearing"] },
            { "name": "HOME", "classes": [] }
        ]
    }"#;

    #[test]
    fn vocabulary_round_trips() {
        for mode in Mode::ALL {
            assert_eq!(Mode::parse(mode.as_str()), Some(mode));
        }
        for class in ActionClass::ALL {
            assert_eq!(ActionClass::parse(class.as_str()), Some(class));
        }
        for decision in [Decision::Allow, Decision::Ask, Decision::Deny] {
            assert_eq!(Decision::parse(decision.as_str()), Some(decision));
        }
        for placement in [
            Placement::SharedUserRuntime,
            Placement::DedicatedRepoRuntime,
            Placement::DedicatedRunRuntime,
            Placement::Blocked,
        ] {
            assert_eq!(Placement::parse(placement.as_str()), Some(placement));
        }
    }

    #[test]
    fn decision_restrictiveness_and_placement_strength_orderings() {
        assert!(Decision::Deny > Decision::Ask);
        assert!(Decision::Ask > Decision::Allow);
        assert!(Placement::Blocked > Placement::DedicatedRunRuntime);
        assert!(Placement::DedicatedRunRuntime > Placement::DedicatedRepoRuntime);
        assert!(Placement::DedicatedRepoRuntime > Placement::SharedUserRuntime);
    }

    #[test]
    fn valid_document_has_no_diagnostics() {
        let (doc, diags) = doc_from(VALID);
        assert_eq!(diags, vec![]);
        assert_eq!(doc.modes.len(), 1);
        assert_eq!(doc.env.len(), 2);
    }

    #[test]
    fn most_restrictive_decision_wins_across_matched_rules() {
        let (doc, _) = doc_from(VALID);
        // git.push matches allow-push (action); destructive matches deny rule.
        let eval = evaluate(
            &doc,
            &EvalRequest {
                mode: Mode::parse("rondo_afk").unwrap(),
                action: "git.push",
                classes: &[ActionClass::Destructive],
                env: &[],
            },
        );
        assert_eq!(eval.decision, Decision::Deny);
        assert_eq!(eval.decision_source, Source::Rule);
        assert_eq!(eval.matched_rules.len(), 2);
    }

    #[test]
    fn force_push_safety_floor_cannot_be_weakened_by_manual_mode_or_rules() {
        let (doc, _) = doc_from(
            r#"{
              "version": "nopal.policy/v1",
              "modes": { "manual": { "rules": [
                { "id": "attempted-allow", "actions": ["git.push_force"], "decision": "allow" }
              ] } }
            }"#,
        );
        let eval = evaluate(
            &doc,
            &EvalRequest {
                mode: Mode::Manual,
                action: "git.push_force",
                classes: &[ActionClass::GitRemote],
                env: &[],
            },
        );
        assert_eq!(eval.decision, Decision::Deny);
        assert_eq!(eval.placement, Placement::Blocked);
        assert_eq!(eval.decision_source, Source::SafetyFloor);
        assert_eq!(eval.placement_source, Source::SafetyFloor);
    }

    #[test]
    fn classes_matching_is_any_of() {
        let (doc, _) = doc_from(VALID);
        // Rule lists only destructive; action carries read + destructive.
        let eval = evaluate(
            &doc,
            &EvalRequest {
                mode: Mode::parse("rondo_afk").unwrap(),
                action: "fs.rm",
                classes: &[ActionClass::Read, ActionClass::Destructive],
                env: &[],
            },
        );
        assert_eq!(eval.decision, Decision::Deny);
        assert_eq!(eval.matched_rules[0].via, "class destructive");
    }

    #[test]
    fn env_refs_contribute_secret_bearing_and_placement_escalates() {
        let (doc, _) = doc_from(VALID);
        let eval = evaluate(
            &doc,
            &EvalRequest {
                mode: Mode::parse("rondo_afk").unwrap(),
                action: "git.push",
                classes: &[ActionClass::GitRemote],
                env: &["API_TOKEN".to_owned(), "NOT_LISTED".to_owned()],
            },
        );
        assert!(eval.classes.contains(&ActionClass::SecretBearing));
        assert_eq!(eval.placement, Placement::DedicatedRunRuntime);
        assert_eq!(eval.placement_source, Source::Rule);
        assert_eq!(eval.decision, Decision::Allow);
        assert!(eval.env.iter().any(|e| e.name == "NOT_LISTED" && !e.known));
        assert!(
            eval.class_notes
                .iter()
                .any(|n| n.contains("added by env ref API_TOKEN")),
            "notes: {:?}",
            eval.class_notes
        );
    }

    #[test]
    fn mode_default_applies_when_no_rule_matches() {
        let (doc, _) = doc_from(VALID);
        let eval = evaluate(
            &doc,
            &EvalRequest {
                mode: Mode::parse("rondo_afk").unwrap(),
                action: "ticket.fetch",
                classes: &[ActionClass::NetworkRead],
                env: &[],
            },
        );
        assert!(eval.matched_rules.is_empty());
        assert_eq!(eval.decision, Decision::Ask);
        assert_eq!(eval.decision_source, Source::ModeDefault);
        assert_eq!(eval.placement, Placement::DedicatedRepoRuntime);
        assert_eq!(eval.placement_source, Source::ModeDefault);
    }

    #[test]
    fn builtin_defaults_apply_for_unconfigured_modes() {
        let (doc, _) = doc_from(VALID);
        let eval = evaluate(
            &doc,
            &EvalRequest {
                mode: Mode::Ci,
                action: "git.push",
                classes: &[],
                env: &[],
            },
        );
        assert_eq!(eval.decision, Decision::Deny);
        assert_eq!(eval.decision_source, Source::BuiltinDefault);
        assert_eq!(eval.placement, Placement::DedicatedRunRuntime);
        assert_eq!(eval.placement_source, Source::BuiltinDefault);

        let manual = evaluate(
            &doc,
            &EvalRequest {
                mode: Mode::Manual,
                action: "git.push",
                classes: &[],
                env: &[],
            },
        );
        assert_eq!(manual.decision, Decision::Allow);
        assert_eq!(manual.placement, Placement::SharedUserRuntime);
    }

    #[test]
    fn additive_mode_and_class_warn_but_closed_placement_is_error() {
        let (_, diags) = doc_from(
            r#"{
                "version": "nopal.policy/v1",
                "modes": {
                    "yolo": {},
                    "ci": {
                        "default_placement": "moon",
                        "rules": [
                            { "id": "r", "classes": ["warp_core"], "decision": "allow" }
                        ]
                    }
                }
            }"#,
        );
        let codes = codes(&diags);
        assert!(!codes.contains(&Code::PolicyModeUnknown), "{codes:?}");
        assert!(codes.contains(&Code::PolicyPlacementInvalid), "{codes:?}");
        assert!(codes.contains(&Code::PolicyClassUnknown), "{codes:?}");
    }

    #[test]
    fn rule_shape_problems_are_errors() {
        let (_, diags) = doc_from(
            r#"{
                "version": "nopal.policy/v1",
                "modes": {
                    "ci": {
                        "rules": [
                            { "classes": ["read"], "decision": "allow" },
                            { "id": "dup", "classes": ["read"], "decision": "allow" },
                            { "id": "dup", "classes": ["read"], "decision": "deny" },
                            { "id": "no-matcher", "decision": "deny" },
                            { "id": "no-effect", "classes": ["read"] },
                            { "id": "bad-decision", "classes": ["read"], "decision": "maybe" }
                        ]
                    }
                }
            }"#,
        );
        let codes = codes(&diags);
        assert!(codes.contains(&Code::PolicyRuleInvalid), "{codes:?}");
        assert!(codes.contains(&Code::PolicyRuleDuplicateId), "{codes:?}");
        assert!(codes.contains(&Code::PolicyDecisionInvalid), "{codes:?}");
    }

    #[test]
    fn env_refs_require_explicit_classification() {
        let (doc, diags) = doc_from(
            r#"{
                "version": "nopal.policy/v1",
                "modes": {},
                "env": [
                    { "name": "TOKEN" },
                    { "name": "OK", "classes": [] },
                    { "name": "OK", "classes": ["read"] }
                ]
            }"#,
        );
        let codes = codes(&diags);
        assert_eq!(
            codes,
            vec![Code::PolicyEnvInvalid, Code::PolicyEnvInvalid],
            "missing classes + duplicate name"
        );
        assert_eq!(doc.env.len(), 1);
    }

    #[test]
    fn unknown_keys_are_warnings_not_errors() {
        let (_, diags) = doc_from(
            r#"{
                "version": "nopal.policy/v1",
                "modes": { "ci": { "placement": "blocked" } },
                "extra": true
            }"#,
        );
        assert!(diags.iter().all(|d| d.severity == Severity::Warning));
        assert_eq!(
            codes(&diags),
            vec![Code::PolicyKeyUnknown, Code::PolicyKeyUnknown]
        );
    }

    #[test]
    fn missing_version_and_modes_are_errors() {
        let (_, diags) = doc_from("{}");
        assert_eq!(
            codes(&diags),
            vec![Code::VersionUnsupported, Code::PolicyShapeInvalid]
        );
    }

    #[test]
    fn report_toon_is_stable_and_includes_explanations() {
        let (doc, diags) = doc_from(VALID);
        assert_eq!(diags, vec![]);
        let eval_req = EvalRequest {
            mode: Mode::parse("rondo_afk").unwrap(),
            action: "git.push",
            classes: &[ActionClass::GitRemote],
            env: &["API_TOKEN".to_owned()],
        };
        let evaluation = evaluate(&doc, &eval_req);
        assert_eq!(evaluation.decision, Decision::Allow);
        // Render through the same builder twice; byte-identical.
        let report_a = report_for_tests(&doc, &eval_req);
        let report_b = report_for_tests(&doc, &eval_req);
        assert_eq!(report_toon(&report_a), report_toon(&report_b));
        let text = report_toon(&report_a);
        assert!(text.contains("kind: nopal.policy_decision/v1"), "{text}");
        assert!(text.contains("decision: allow"), "{text}");
        assert!(text.contains("placement: dedicated_run_runtime"), "{text}");
        assert!(text.contains("explanation["), "{text}");
    }

    fn report_for_tests(doc: &PolicyDoc, req: &EvalRequest) -> PolicyReport {
        let evaluation = evaluate(doc, req);
        PolicyReport {
            kind: View::Decide.kind(),
            ok: true,
            mode: Some(evaluation.mode),
            action: Some(evaluation.action),
            classes: Some(evaluation.classes),
            env: Some(evaluation.env),
            decision: Some(evaluation.decision),
            decision_source: Some(evaluation.decision_source),
            placement: Some(evaluation.placement),
            placement_source: Some(evaluation.placement_source),
            matched_rules: Some(evaluation.matched_rules),
            diagnostics: Vec::new(),
            explanation: Some(evaluation.class_notes),
        }
    }
}
