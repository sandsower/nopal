//! `nopal.integrations/v1` module validation.
//!
//! Integrations describe external connection surfaces Nopal can model for
//! Beislið-compatible consumers. Nopal validates provider/config shapes only;
//! callers decide whether and how to execute providers.

use globset::GlobBuilder;
use regex::Regex;

use crate::config;
use crate::diagnostics::{Code, Diagnostic};
use crate::workflow;

pub const INTEGRATIONS_KIND: &str = "nopal.integrations/v1";

const ROUTE_MODES: &[&str] = &["prefer", "require"];
const VISUAL_MODES: &[&str] = &["off", "suggest", "prompt", "auto"];
const SIGNAL_MODES: &[&str] = &["off", "auto"];
const RETENTION: &[&str] = &["local", "discard", "preserve-repo"];
const MODEL_TIERS: &[&str] = &["light", "standard", "heavy", "frontier"];

/// Validate a parsed `.nopal/integrations.jsonc` value.
pub fn validate_document(root: &serde_json::Value, path: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    match root.get("version").and_then(|v| v.as_str()) {
        Some(INTEGRATIONS_KIND) => {}
        Some(other) => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("unsupported integrations version {other:?}; expected {INTEGRATIONS_KIND:?}"),
        )),
        None => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("missing string field \"version\"; expected {INTEGRATIONS_KIND:?}"),
        )),
    }

    if let Some(tracker) = root.get("tracker") {
        validate_tracker(tracker, path, &mut diagnostics);
    }
    if let Some(pr_reviews) = root.get("pr_reviews") {
        validate_pr_reviews(pr_reviews, path, &mut diagnostics);
    }
    if let Some(pi_handoff) = root.get("pi_handoff") {
        validate_pi_handoff(pi_handoff, path, &mut diagnostics);
    }
    if let Some(model_routing) = root.get("model_routing") {
        validate_model_routing(model_routing, path, &mut diagnostics);
    }
    if let Some(visual_surfaces) = root.get("visual_surfaces") {
        validate_visual_surfaces(visual_surfaces, path, &mut diagnostics);
    }
    if let Some(workflow_signals) = root.get("workflow_signals") {
        validate_workflow_signals(workflow_signals, path, &mut diagnostics);
    }
    if let Some(probe_cache) = root.get("probe_cache") {
        validate_probe_cache(probe_cache, path, &mut diagnostics);
    }

    diagnostics
}

pub fn parse_integrations(text: &str, path: &str) -> (Option<serde_json::Value>, Vec<Diagnostic>) {
    let root = match config::parse_jsonc(text, path, Code::ModuleParseError) {
        Ok(value) => value,
        Err(diagnostic) => return (None, vec![diagnostic]),
    };
    let diagnostics = validate_document(&root, path);
    (Some(root), diagnostics)
}

fn validate_tracker(value: &serde_json::Value, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, "tracker must be an object");
        return;
    };
    if let Some(source) = obj.get("ticket_source") {
        validate_ticket_source(source, "tracker.ticket_source", path, diagnostics);
    }
    if let Some(update) = obj.get("ticket_update") {
        validate_ticket_update(update, "tracker.ticket_update", path, diagnostics);
    }
}

fn validate_ticket_source(
    value: &serde_json::Value,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, format!("{ctx} must be an object"));
        return;
    };
    let Some(provider) = obj.get("type").and_then(|v| v.as_str()) else {
        provider_invalid(path, diagnostics, format!("{ctx}: missing provider type"));
        return;
    };
    match provider {
        "mcp" => {
            require_fields(obj, &["tool", "id_pattern"], ctx, path, diagnostics);
            validate_regex_field(obj, "id_pattern", ctx, path, diagnostics);
        }
        "cli" => {
            require_fields(obj, &["command", "id_pattern"], ctx, path, diagnostics);
            validate_regex_field(obj, "id_pattern", ctx, path, diagnostics);
        }
        "file" => {
            require_fields(obj, &["file_glob", "id_pattern"], ctx, path, diagnostics);
            validate_regex_field(obj, "id_pattern", ctx, path, diagnostics);
            validate_glob_field(obj, "file_glob", ctx, path, diagnostics);
        }
        "paste" => {}
        _ => provider_invalid(
            path,
            diagnostics,
            format!("{ctx}: unsupported ticket source provider {provider:?}"),
        ),
    }
}

fn validate_ticket_update(
    value: &serde_json::Value,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, format!("{ctx} must be an object"));
        return;
    };
    let Some(provider) = obj.get("type").and_then(|v| v.as_str()) else {
        provider_invalid(path, diagnostics, format!("{ctx}: missing provider type"));
        return;
    };
    match provider {
        "mcp" => require_any_field(obj, &["comment_tool", "issue_tool"], ctx, path, diagnostics),
        "cli" => require_any_field(
            obj,
            &["comment_command", "issue_command"],
            ctx,
            path,
            diagnostics,
        ),
        _ => provider_invalid(
            path,
            diagnostics,
            format!("{ctx}: unsupported ticket update provider {provider:?}"),
        ),
    }
}

fn validate_pr_reviews(value: &serde_json::Value, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, "pr_reviews must be an object");
        return;
    };
    if let Some(source) = obj.get("source") {
        let Some(source) = source.as_object() else {
            field_invalid(path, diagnostics, "pr_reviews.source must be an object");
            return;
        };
        match source.get("type").and_then(|v| v.as_str()) {
            Some("cli") => require_fields(
                source,
                &["summary_command"],
                "pr_reviews.source",
                path,
                diagnostics,
            ),
            Some("paste") => {}
            Some(other) => provider_invalid(
                path,
                diagnostics,
                format!("pr_reviews.source: unsupported provider {other:?}"),
            ),
            None => provider_invalid(
                path,
                diagnostics,
                "pr_reviews.source: missing provider type",
            ),
        }
    }
    if let Some(update) = obj.get("update") {
        let Some(update) = update.as_object() else {
            field_invalid(path, diagnostics, "pr_reviews.update must be an object");
            return;
        };
        match update.get("type").and_then(|v| v.as_str()) {
            Some("cli") => require_fields(
                update,
                &["reply_command"],
                "pr_reviews.update",
                path,
                diagnostics,
            ),
            Some("manual") => {}
            Some(other) => provider_invalid(
                path,
                diagnostics,
                format!("pr_reviews.update: unsupported provider {other:?}"),
            ),
            None => provider_invalid(
                path,
                diagnostics,
                "pr_reviews.update: missing provider type",
            ),
        }
    }
}

fn validate_pi_handoff(value: &serde_json::Value, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, "pi_handoff must be an object");
        return;
    };
    if let Some(events) = obj.get("events") {
        match events.as_str() {
            Some("all") => {}
            Some(_) => field_invalid(
                path,
                diagnostics,
                "pi_handoff.events must be \"all\" or an array of lifecycle event strings",
            ),
            None => validate_lifecycle_event_array(events, "pi_handoff.events", path, diagnostics),
        }
    }
    if let Some(exclude) = obj.get("exclude") {
        validate_lifecycle_event_array(exclude, "pi_handoff.exclude", path, diagnostics);
    }
}

fn validate_lifecycle_event_array(
    value: &serde_json::Value,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(items) = value.as_array() else {
        field_invalid(
            path,
            diagnostics,
            format!("{ctx} must be an array of lifecycle event strings"),
        );
        return;
    };
    for item in items {
        match item.as_str() {
            Some(event) if workflow::LIFECYCLE_EVENTS.contains(&event) => {}
            Some(event) => diagnostics.push(Diagnostic::error(
                Code::WorkflowEventUnknown,
                path,
                format!(
                    "{ctx}: unknown lifecycle event {event:?}; expected one of {}",
                    quoted(workflow::LIFECYCLE_EVENTS)
                ),
            )),
            None => field_invalid(
                path,
                diagnostics,
                format!("{ctx} must be an array of lifecycle event strings"),
            ),
        }
    }
}

fn validate_model_routing(
    value: &serde_json::Value,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, "model_routing must be an object");
        return;
    };
    if let Some(defaults) = obj.get("defaults") {
        validate_route(defaults, "model_routing.defaults", false, path, diagnostics);
    }
    if let Some(overrides) = obj.get("overrides") {
        let Some(items) = overrides.as_array() else {
            field_invalid(
                path,
                diagnostics,
                "model_routing.overrides must be an array",
            );
            return;
        };
        for (index, route) in items.iter().enumerate() {
            validate_route(
                route,
                &format!("model_routing.overrides[{index}]"),
                true,
                path,
                diagnostics,
            );
        }
    }
    if let Some(tiers) = obj.get("tiers") {
        let Some(tiers) = tiers.as_object() else {
            field_invalid(path, diagnostics, "model_routing.tiers must be an object");
            return;
        };
        for (tier, candidates) in tiers {
            if !MODEL_TIERS.contains(&tier.as_str()) {
                field_invalid(
                    path,
                    diagnostics,
                    format!(
                        "model_routing.tiers: unknown tier {tier:?}; expected one of {}",
                        quoted(MODEL_TIERS)
                    ),
                );
            }
            if !non_empty_string_array(candidates) {
                field_invalid(
                    path,
                    diagnostics,
                    format!("model_routing.tiers.{tier} must be a non-empty array of strings"),
                );
            }
        }
    }
    if let Some(mode) = obj.get("tier_mode") {
        validate_enum(
            mode,
            ROUTE_MODES,
            "model_routing.tier_mode",
            path,
            diagnostics,
        );
    }
}

fn validate_route(
    value: &serde_json::Value,
    ctx: &str,
    require_skills: bool,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, format!("{ctx} must be an object"));
        return;
    };
    let has_model = non_empty_str(obj.get("model"));
    let has_models = obj.get("models").is_some_and(non_empty_string_array);
    if has_model == has_models {
        field_invalid(
            path,
            diagnostics,
            format!("{ctx} must set exactly one of model or models"),
        );
    }
    if require_skills && !obj.get("skills").is_some_and(non_empty_string_array) {
        field_invalid(
            path,
            diagnostics,
            format!("{ctx}.skills must be a non-empty array of strings"),
        );
    }
    if let Some(mode) = obj.get("mode") {
        validate_enum(mode, ROUTE_MODES, &format!("{ctx}.mode"), path, diagnostics);
    }
}

fn validate_visual_surfaces(
    value: &serde_json::Value,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, "visual_surfaces must be an object");
        return;
    };
    match obj.get("provider").and_then(|v| v.as_str()) {
        Some("lavish-axi") | None => {}
        Some(other) => provider_invalid(
            path,
            diagnostics,
            format!("visual_surfaces.provider {other:?} is unsupported"),
        ),
    }
    if let Some(mode) = obj.get("mode") {
        validate_enum(
            mode,
            VISUAL_MODES,
            "visual_surfaces.mode",
            path,
            diagnostics,
        );
    }
    if let Some(retention) = obj.get("artifact_retention") {
        validate_enum(
            retention,
            RETENTION,
            "visual_surfaces.artifact_retention",
            path,
            diagnostics,
        );
    }
    if let Some(workflows) = obj.get("workflows") {
        validate_string_map_enum(
            workflows,
            VISUAL_MODES,
            "visual_surfaces.workflows",
            path,
            diagnostics,
        );
    }
}

fn validate_workflow_signals(
    value: &serde_json::Value,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, "workflow_signals must be an object");
        return;
    };
    if let Some(mode) = obj.get("mode") {
        validate_enum(
            mode,
            SIGNAL_MODES,
            "workflow_signals.mode",
            path,
            diagnostics,
        );
    }
    if let Some(sinks) = obj.get("sinks") {
        let Some(sinks) = sinks.as_array() else {
            field_invalid(path, diagnostics, "workflow_signals.sinks must be an array");
            return;
        };
        for (index, sink) in sinks.iter().enumerate() {
            match sink.get("type").and_then(|v| v.as_str()) {
                Some("tmux-glance") => {}
                Some(other) => provider_invalid(
                    path,
                    diagnostics,
                    format!("workflow_signals.sinks[{index}]: unsupported sink {other:?}"),
                ),
                None => provider_invalid(
                    path,
                    diagnostics,
                    format!("workflow_signals.sinks[{index}]: missing sink type"),
                ),
            }
        }
    }
    if let Some(skills) = obj.get("skills") {
        validate_string_map_enum(
            skills,
            SIGNAL_MODES,
            "workflow_signals.skills",
            path,
            diagnostics,
        );
    }
}

fn validate_probe_cache(value: &serde_json::Value, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, "probe_cache must be an object");
        return;
    };
    if let Some(ttl) = obj.get("ttl_hours") {
        match ttl.as_i64() {
            Some(n) if n > 0 => {}
            _ => field_invalid(
                path,
                diagnostics,
                "probe_cache.ttl_hours must be a positive integer",
            ),
        }
    }
}

fn require_fields(
    obj: &serde_json::Map<String, serde_json::Value>,
    fields: &[&str],
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for field in fields {
        if !non_empty_str(obj.get(*field)) {
            field_invalid(
                path,
                diagnostics,
                format!("{ctx}: requires non-empty string field {field:?}"),
            );
        }
    }
}

fn validate_regex_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(pattern) = obj.get(field).and_then(|v| v.as_str()) else {
        return;
    };
    if Regex::new(pattern).is_err() {
        provider_invalid(
            path,
            diagnostics,
            format!("{ctx}.{field} must be a valid regular expression"),
        );
    }
}

fn validate_glob_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    field: &str,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(pattern) = obj.get(field).and_then(|v| v.as_str()) else {
        return;
    };
    if GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .is_err()
    {
        provider_invalid(
            path,
            diagnostics,
            format!("{ctx}.{field} must be a valid git-style glob"),
        );
    }
}

fn require_any_field(
    obj: &serde_json::Map<String, serde_json::Value>,
    fields: &[&str],
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !fields.iter().any(|field| non_empty_str(obj.get(*field))) {
        field_invalid(
            path,
            diagnostics,
            format!("{ctx}: requires at least one of {}", quoted(fields)),
        );
    }
}

fn validate_string_map_enum(
    value: &serde_json::Value,
    allowed: &[&str],
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(obj) = value.as_object() else {
        field_invalid(path, diagnostics, format!("{ctx} must be an object"));
        return;
    };
    for (key, value) in obj {
        validate_enum(value, allowed, &format!("{ctx}.{key}"), path, diagnostics);
    }
}

fn validate_enum(
    value: &serde_json::Value,
    allowed: &[&str],
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match value.as_str() {
        Some(text) if allowed.contains(&text) => {}
        _ => field_invalid(
            path,
            diagnostics,
            format!("{ctx} must be one of {}", quoted(allowed)),
        ),
    }
}

fn non_empty_str(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
}

fn non_empty_string_array(value: &serde_json::Value) -> bool {
    value.as_array().is_some_and(|items| {
        !items.is_empty() && items.iter().all(|item| non_empty_str(Some(item)))
    })
}

fn field_invalid(path: &str, diagnostics: &mut Vec<Diagnostic>, message: impl Into<String>) {
    diagnostics.push(Diagnostic::error(Code::FieldInvalid, path, message));
}

fn provider_invalid(path: &str, diagnostics: &mut Vec<Diagnostic>, message: impl Into<String>) {
    diagnostics.push(Diagnostic::error(
        Code::IntegrationProviderInvalid,
        path,
        message,
    ));
}

fn quoted(items: &[&str]) -> String {
    items
        .iter()
        .map(|item| format!("{item:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn codes(diags: &[Diagnostic]) -> Vec<Code> {
        diags.iter().map(|d| d.code).collect()
    }

    #[test]
    fn valid_minimal_document_has_no_diagnostics() {
        let (_, diags) = parse_integrations(r#"{ "version": "nopal.integrations/v1" }"#, "i.jsonc");
        assert_eq!(diags, vec![]);
    }

    #[test]
    fn bad_providers_are_reported() {
        let text = r#"{
            "version": "nopal.integrations/v1",
            "tracker": { "ticket_source": { "type": "graphql" } },
            "visual_surfaces": { "provider": "browser" },
            "workflow_signals": { "sinks": [ { "type": "webhook" } ] }
        }"#;
        let (_, diags) = parse_integrations(text, "i.jsonc");
        assert!(
            codes(&diags)
                .iter()
                .all(|code| *code == Code::IntegrationProviderInvalid)
        );
    }

    #[test]
    fn pi_handoff_event_names_use_workflow_event_vocabulary() {
        let text = r#"{
            "version": "nopal.integrations/v1",
            "pi_handoff": {
                "events": ["kickoff_context_ready", "surprise"],
                "exclude": ["ready_for_review_pre_submit", "future"]
            }
        }"#;
        let (_, diags) = parse_integrations(text, "i.jsonc");
        assert_eq!(
            codes(&diags),
            vec![Code::WorkflowEventUnknown, Code::WorkflowEventUnknown]
        );
    }

    #[test]
    fn ticket_source_pattern_fields_must_compile() {
        let text = r#"{
            "version": "nopal.integrations/v1",
            "tracker": {
                "ticket_source": {
                    "type": "file",
                    "id_pattern": "(",
                    "file_glob": "["
                }
            }
        }"#;
        let (_, diags) = parse_integrations(text, "i.jsonc");
        assert_eq!(
            codes(&diags),
            vec![
                Code::IntegrationProviderInvalid,
                Code::IntegrationProviderInvalid
            ]
        );
    }

    #[test]
    fn model_routing_validates_routes_modes_and_tiers() {
        let valid = r#"{
            "version": "nopal.integrations/v1",
            "model_routing": {
                "defaults": { "models": ["sonnet"], "mode": "prefer" },
                "overrides": [
                    { "skills": ["blueprint"], "model": "opus", "mode": "require" }
                ],
                "tiers": { "light": ["haiku"], "heavy": ["opus", "sonnet"] },
                "tier_mode": "prefer"
            }
        }"#;
        let (_, diags) = parse_integrations(valid, "i.jsonc");
        assert_eq!(diags, vec![]);

        let invalid = r#"{
            "version": "nopal.integrations/v1",
            "model_routing": {
                "defaults": { "model": "opus", "models": ["sonnet"], "mode": "maybe" },
                "overrides": [ { "model": "opus" } ],
                "tiers": { "warp": [], "light": [] },
                "tier_mode": "never"
            }
        }"#;
        let (_, diags) = parse_integrations(invalid, "i.jsonc");
        assert!(
            codes(&diags).iter().all(|code| *code == Code::FieldInvalid),
            "{diags:?}"
        );
        assert!(diags.len() >= 6, "{diags:?}");
    }

    #[test]
    fn visual_surfaces_workflow_signals_and_probe_cache_are_validated() {
        let valid = r#"{
            "version": "nopal.integrations/v1",
            "visual_surfaces": {
                "provider": "lavish-axi",
                "mode": "prompt",
                "artifact_retention": "local",
                "workflows": { "blueprint": "suggest" }
            },
            "workflow_signals": {
                "mode": "auto",
                "sinks": [ { "type": "tmux-glance" } ],
                "skills": { "ready-for-review": "auto" }
            },
            "probe_cache": { "ttl_hours": 24 }
        }"#;
        let (_, diags) = parse_integrations(valid, "i.jsonc");
        assert_eq!(diags, vec![]);

        let invalid = r#"{
            "version": "nopal.integrations/v1",
            "visual_surfaces": {
                "provider": "browser",
                "mode": "always",
                "artifact_retention": "forever",
                "workflows": { "blueprint": "sometimes" }
            },
            "workflow_signals": {
                "mode": "loud",
                "sinks": [ { "type": "webhook" } ],
                "skills": { "ready-for-review": "sometimes" }
            },
            "probe_cache": { "ttl_hours": 0 }
        }"#;
        let (_, diags) = parse_integrations(invalid, "i.jsonc");
        let codes = codes(&diags);
        assert!(
            codes.contains(&Code::IntegrationProviderInvalid),
            "{codes:?}"
        );
        assert!(codes.contains(&Code::FieldInvalid), "{codes:?}");
        assert!(diags.len() >= 8, "{diags:?}");
    }
}
