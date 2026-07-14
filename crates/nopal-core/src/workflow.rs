//! `nopal.workflow/v1` module validation.
//!
//! The workflow module models Beislið-compatible lifecycle/checkpoint side
//! effects as deterministic config. Nopal validates and explains the shape; it
//! does not execute actions.

use crate::config;
use crate::diagnostics::{Code, Diagnostic};
use crate::policy;

pub const WORKFLOW_KIND: &str = "nopal.workflow/v1";

pub const LIFECYCLE_EVENTS: &[&str] = &[
    "kickoff_start",
    "break_spec_approved",
    "spec_approved",
    "blueprint_approved",
    "kickoff_context_ready",
    "implementation_plan_created",
    "review_feedback_loaded",
    "ready_for_review_pre_submit",
];

const ON_FAILURE: &[&str] = &["prompt", "continue", "abort"];
const APPROVAL: &[&str] = &["auto", "prompt"];

/// Validate a parsed `.nopal/workflow.jsonc` value against the
/// `nopal.workflow/v1` schema.
pub fn validate_document(root: &serde_json::Value, path: &str) -> Vec<Diagnostic> {
    let mut diagnostics = Vec::new();

    match root.get("version").and_then(|v| v.as_str()) {
        Some(WORKFLOW_KIND) => {}
        Some(other) => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("unsupported workflow version {other:?}; expected {WORKFLOW_KIND:?}"),
        )),
        None => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("missing string field \"version\"; expected {WORKFLOW_KIND:?}"),
        )),
    }

    if let Some(lifecycle) = root.get("lifecycle") {
        let Some(obj) = lifecycle.as_object() else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                "\"lifecycle\" must be an object",
            ));
            return diagnostics;
        };
        if let Some(events) = obj.get("events") {
            validate_events(events, path, &mut diagnostics);
        }
    }

    if let Some(establishment) = root.get("establishment") {
        validate_establishment(establishment, path, &mut diagnostics);
    }

    if let Some(handoff) = root.get("handoff") {
        validate_handoff(handoff, path, &mut diagnostics);
    }

    if let Some(babysit) = root.get("babysit") {
        validate_babysit(babysit, path, &mut diagnostics);
    }

    diagnostics
}

pub fn establishment_events(root: &serde_json::Value) -> Vec<&str> {
    root.get("establishment")
        .and_then(|value| value.get("events"))
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .collect()
}

fn validate_establishment(
    value: &serde_json::Value,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(value) = value.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "\"establishment\" must be an object",
        ));
        return;
    };
    let Some(events) = value.get("events").and_then(serde_json::Value::as_array) else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "\"establishment.events\" must be an array of non-empty strings",
        ));
        return;
    };
    let mut seen = std::collections::BTreeSet::new();
    for event in events {
        match event.as_str().filter(|event| !event.trim().is_empty()) {
            Some(event) if !seen.insert(event) => diagnostics.push(Diagnostic::error(
                Code::DuplicateId,
                path,
                format!("duplicate Establishment event {event:?}"),
            )),
            Some(_) => {}
            None => diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                "establishment.events entries must be non-empty strings",
            )),
        }
    }
}

/// `handoff.auto`/`events`/`exclude`: event tokens are open vocabulary (no
/// membership check against `LIFECYCLE_EVENTS`).
fn validate_handoff(value: &serde_json::Value, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(obj) = value.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "\"handoff\" must be an object",
        ));
        return;
    };

    if let Some(auto) = obj.get("auto")
        && !auto.is_boolean()
    {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "\"handoff.auto\" must be a bool",
        ));
    }

    if let Some(events) = obj.get("events") {
        validate_string_array(events, "handoff.events", path, diagnostics);
    }

    if let Some(exclude) = obj.get("exclude") {
        validate_string_array(exclude, "handoff.exclude", path, diagnostics);
    }
}

/// `babysit.token_budget`: an open positive-integer override; unknown keys
/// alongside it are ignored (existing module idiom).
fn validate_babysit(value: &serde_json::Value, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(obj) = value.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "\"babysit\" must be an object",
        ));
        return;
    };

    if let Some(budget) = obj.get("token_budget")
        && budget.as_u64().is_none_or(|value| value == 0)
    {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "\"babysit.token_budget\" must be a positive integer",
        ));
    }
}

fn validate_string_array(
    value: &serde_json::Value,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(items) = value.as_array() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("\"{ctx}\" must be an array of non-empty strings"),
        ));
        return;
    };
    for item in items {
        if !non_empty_str(Some(item)) {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{ctx}: entries must be non-empty strings"),
            ));
        }
    }
}

/// Parse and validate workflow module text.
pub fn parse_workflow(text: &str, path: &str) -> (Option<serde_json::Value>, Vec<Diagnostic>) {
    let root = match config::parse_jsonc(text, path, Code::ModuleParseError) {
        Ok(value) => value,
        Err(diagnostic) => return (None, vec![diagnostic]),
    };
    let diagnostics = validate_document(&root, path);
    (Some(root), diagnostics)
}

fn validate_events(events: &serde_json::Value, path: &str, diagnostics: &mut Vec<Diagnostic>) {
    let Some(events) = events.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            "\"lifecycle.events\" must be an object",
        ));
        return;
    };

    for (event, body) in events {
        if !LIFECYCLE_EVENTS.contains(&event.as_str()) {
            diagnostics.push(Diagnostic::error(
                Code::WorkflowEventUnknown,
                path,
                format!(
                    "unknown lifecycle event {event:?}; expected one of {}",
                    quoted(LIFECYCLE_EVENTS)
                ),
            ));
        }

        let ctx = format!("lifecycle.events.{event}");
        let Some(body) = body.as_object() else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{ctx} must be an object"),
            ));
            continue;
        };
        let Some(actions) = body.get("actions") else {
            continue;
        };
        let Some(actions) = actions.as_array() else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{ctx}.actions must be an array"),
            ));
            continue;
        };
        let mut seen_action_ids: Vec<String> = Vec::new();
        for (index, action) in actions.iter().enumerate() {
            let action_ctx = format!("{ctx}.actions[{index}]");
            if let Some(id) = action.get("id").and_then(|v| v.as_str())
                && !id.trim().is_empty()
            {
                if seen_action_ids.iter().any(|seen| seen == id) {
                    diagnostics.push(Diagnostic::error(
                        Code::DuplicateId,
                        path,
                        format!("{action_ctx}: duplicate action id {id:?} in event {event:?}"),
                    ));
                } else {
                    seen_action_ids.push(id.to_owned());
                }
            }
            validate_action(event, action, &action_ctx, path, diagnostics);
        }
    }
}

fn validate_action(
    event: &str,
    action: &serde_json::Value,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(obj) = action.as_object() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx} must be an object"),
        ));
        return;
    };

    if !non_empty_str(obj.get("id")) {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx}: requires stable non-empty string field \"id\""),
        ));
    }

    let Some(action_type) = obj.get("type").and_then(|v| v.as_str()) else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx}: requires non-empty string field \"type\""),
        ));
        return;
    };

    if !allowed_action_types(event).contains(&action_type) {
        diagnostics.push(Diagnostic::error(
            Code::WorkflowActionTypeUnknown,
            path,
            format!(
                "{ctx}: action type {action_type:?} is not supported for event {event:?}; expected one of {}",
                quoted(allowed_action_types(event))
            ),
        ));
    }

    match action_type {
        "cli" => validate_cli_action(obj, ctx, path, diagnostics),
        "artifact" => validate_artifact_action(obj, ctx, path, diagnostics),
        "tracker" => validate_tracker_action(obj, ctx, path, diagnostics),
        _ => {}
    }

    if let Some(on_failure) = obj.get("on_failure") {
        validate_enum(
            on_failure,
            ON_FAILURE,
            &format!("{ctx}.on_failure"),
            path,
            diagnostics,
        );
    }
}

fn allowed_action_types(event: &str) -> &'static [&'static str] {
    match event {
        "kickoff_start" => &["cli"],
        "break_spec_approved" | "blueprint_approved" => &["artifact", "cli"],
        "spec_approved" => &["artifact", "cli", "tracker"],
        "kickoff_context_ready"
        | "implementation_plan_created"
        | "review_feedback_loaded"
        | "ready_for_review_pre_submit" => &["artifact"],
        _ => &["artifact", "cli", "tracker"],
    }
}

fn validate_cli_action(
    obj: &serde_json::Map<String, serde_json::Value>,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if !non_empty_str(obj.get("command")) {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx}: cli actions require non-empty string field \"command\""),
        ));
    }
    match obj.get("approval") {
        Some(value) => validate_enum(
            value,
            APPROVAL,
            &format!("{ctx}.approval"),
            path,
            diagnostics,
        ),
        None => diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx}: cli actions require \"approval\""),
        )),
    }
    if let Some(classes) = obj.get("classes") {
        validate_classes(classes, &format!("{ctx}.classes"), path, diagnostics);
    }
}

fn validate_artifact_action(
    obj: &serde_json::Map<String, serde_json::Value>,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    if let Some(approval) = obj.get("approval") {
        validate_enum(
            approval,
            APPROVAL,
            &format!("{ctx}.approval"),
            path,
            diagnostics,
        );
    }
    if let Some(value) = obj.get("path") {
        let Some(text) = value.as_str().filter(|s| !s.trim().is_empty()) else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{ctx}.path must be a non-empty string when present"),
            ));
            return;
        };
        if text.starts_with('/') || text.contains("..") || !text.ends_with(".md") {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{ctx}.path must be a relative repo-local .md path with no '..' segments"),
            ));
        }
    }
}

fn validate_tracker_action(
    obj: &serde_json::Map<String, serde_json::Value>,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    match obj.get("approval") {
        Some(value) => validate_enum(
            value,
            APPROVAL,
            &format!("{ctx}.approval"),
            path,
            diagnostics,
        ),
        None => diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx}: tracker actions require \"approval\""),
        )),
    }
}

fn validate_classes(
    value: &serde_json::Value,
    ctx: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let Some(items) = value.as_array() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx} must be an array of action-policy classes"),
        ));
        return;
    };
    for item in items {
        // Classes are open vocabulary: unknown tokens pass through as data and
        // degrade conservatively at policy evaluation time. Only entries that
        // cannot be a class at all (non-strings, empty strings) are invalid.
        if item.as_str().and_then(policy::ActionClass::parse).is_none() {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!("{ctx}: invalid action-policy class {item}"),
            ));
        }
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
        _ => diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("{ctx} must be one of {}", quoted(allowed)),
        )),
    }
}

fn non_empty_str(value: Option<&serde_json::Value>) -> bool {
    value
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty())
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
        let (_, diags) = parse_workflow(r#"{ "version": "nopal.workflow/v1" }"#, "w.jsonc");
        assert_eq!(diags, vec![]);
    }

    #[test]
    fn establishment_events_are_explicit_and_deduplicated() {
        let text = r#"{
            "version": "nopal.workflow/v1",
            "establishment": {"events": ["kickoff_context_ready"]}
        }"#;
        let (value, diagnostics) = parse_workflow(text, "w.jsonc");
        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(
            establishment_events(value.as_ref().unwrap()),
            vec!["kickoff_context_ready"]
        );

        let duplicate = serde_json::json!({
            "version": "nopal.workflow/v1",
            "establishment": {"events": ["ready", "ready"]}
        });
        assert!(
            validate_document(&duplicate, "w.jsonc")
                .iter()
                .any(|diagnostic| diagnostic.code == Code::DuplicateId)
        );
    }

    #[test]
    fn unsupported_event_and_action_type_are_errors() {
        let text = r#"{
            "version": "nopal.workflow/v1",
            "lifecycle": { "events": {
                "kickoff_start": { "actions": [
                    { "id": "bad", "type": "artifact", "path": "plans/x.md" }
                ] },
                "future": { "actions": [] }
            } }
        }"#;
        let (_, diags) = parse_workflow(text, "w.jsonc");
        let codes = codes(&diags);
        assert!(codes.contains(&Code::WorkflowEventUnknown));
        assert!(codes.contains(&Code::WorkflowActionTypeUnknown));
    }

    #[test]
    fn duplicate_action_ids_within_event_are_errors() {
        let text = r#"{
            "version": "nopal.workflow/v1",
            "lifecycle": { "events": {
                "blueprint_approved": { "actions": [
                    { "id": "write-design", "type": "artifact", "path": "plans/a.md" },
                    { "id": "write-design", "type": "artifact", "path": "plans/b.md" }
                ] }
            } }
        }"#;
        let (_, diags) = parse_workflow(text, "w.jsonc");
        assert_eq!(codes(&diags), vec![Code::DuplicateId]);
    }

    #[test]
    fn cli_actions_validate_command_approval_classes_and_on_failure() {
        let text = r#"{
            "version": "nopal.workflow/v1",
            "lifecycle": { "events": {
                "kickoff_start": { "actions": [
                    {
                        "id": "bad-cli",
                        "type": "cli",
                        "command": "",
                        "approval": "maybe",
                        "classes": ["warp"],
                        "on_failure": "explode"
                    }
                ] }
            } }
        }"#;
        let (_, diags) = parse_workflow(text, "w.jsonc");
        assert_eq!(
            codes(&diags),
            vec![Code::FieldInvalid, Code::FieldInvalid, Code::FieldInvalid]
        );
    }

    #[test]
    fn tracker_actions_require_valid_approval() {
        let missing = r#"{
            "version": "nopal.workflow/v1",
            "lifecycle": { "events": {
                "spec_approved": { "actions": [
                    { "id": "post-spec", "type": "tracker" }
                ] }
            } }
        }"#;
        let (_, diags) = parse_workflow(missing, "w.jsonc");
        assert_eq!(codes(&diags), vec![Code::FieldInvalid]);

        let invalid = r#"{
            "version": "nopal.workflow/v1",
            "lifecycle": { "events": {
                "spec_approved": { "actions": [
                    { "id": "post-spec", "type": "tracker", "approval": "sometimes" }
                ] }
            } }
        }"#;
        let (_, diags) = parse_workflow(invalid, "w.jsonc");
        assert_eq!(codes(&diags), vec![Code::FieldInvalid]);
    }

    #[test]
    fn valid_handoff_and_babysit_sections_have_no_diagnostics() {
        let text = r#"{
            "version": "nopal.workflow/v1",
            "handoff": {
                "auto": true,
                "events": ["kickoff_context_ready"],
                "exclude": ["spec_approved"]
            },
            "babysit": { "token_budget": 400000 }
        }"#;
        let (_, diags) = parse_workflow(text, "w.jsonc");
        assert_eq!(diags, vec![]);
    }

    #[test]
    fn handoff_event_tokens_are_open_vocabulary() {
        // Not a member of LIFECYCLE_EVENTS: still valid, data not ABI.
        let text = r#"{
            "version": "nopal.workflow/v1",
            "handoff": { "events": ["some_future_event"], "exclude": ["another_future_event"] }
        }"#;
        let (_, diags) = parse_workflow(text, "w.jsonc");
        assert_eq!(diags, vec![]);
    }

    #[test]
    fn handoff_field_shape_errors_are_reported() {
        let text = r#"{
            "version": "nopal.workflow/v1",
            "handoff": { "auto": "yes", "events": ["ok", ""], "exclude": [1] }
        }"#;
        let (_, diags) = parse_workflow(text, "w.jsonc");
        assert_eq!(
            codes(&diags),
            vec![Code::FieldInvalid, Code::FieldInvalid, Code::FieldInvalid]
        );
    }

    #[test]
    fn handoff_must_be_an_object() {
        let text = r#"{ "version": "nopal.workflow/v1", "handoff": "auto" }"#;
        let (_, diags) = parse_workflow(text, "w.jsonc");
        assert_eq!(codes(&diags), vec![Code::FieldInvalid]);
    }

    #[test]
    fn babysit_token_budget_must_be_a_positive_integer() {
        for bad in ["0", "-1", "1.5", "\"400000\""] {
            let text = format!(
                r#"{{ "version": "nopal.workflow/v1", "babysit": {{ "token_budget": {bad} }} }}"#
            );
            let (_, diags) = parse_workflow(&text, "w.jsonc");
            assert_eq!(codes(&diags), vec![Code::FieldInvalid], "bad value: {bad}");
        }
    }

    #[test]
    fn unknown_keys_inside_handoff_and_babysit_are_ignored() {
        let text = r#"{
            "version": "nopal.workflow/v1",
            "handoff": { "auto": false, "future_field": 1 },
            "babysit": { "token_budget": 1, "future_field": "x" }
        }"#;
        let (_, diags) = parse_workflow(text, "w.jsonc");
        assert_eq!(diags, vec![]);
    }
}
