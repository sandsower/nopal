//! `nopal.project/v1` manifest parsing.
//!
//! Parsing is diagnostic-accumulating rather than fail-fast: a manifest with
//! a bad version and an unknown profile reports both problems in one pass.

use crate::diagnostics::{Code, Diagnostic};
use crate::profile::{self, Module, Profile};

pub const MANIFEST_KIND: &str = "nopal.project/v1";

#[derive(Debug, Clone)]
pub struct Manifest {
    pub project_name: Option<String>,
    pub profile: Option<Profile>,
    pub required_modules: Vec<Module>,
}

/// Dialect: comments and trailing commas are allowed (JSONC niceties);
/// property names must be quoted.
fn parse_options() -> jsonc_parser::ParseOptions {
    jsonc_parser::ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
    }
}

/// Parse JSONC text into a JSON value. `code` selects the diagnostic code the
/// caller wants for parse failures (`manifest_parse_error` vs `module_parse_error`).
///
/// jsonc-parser is deliberately error-recovering: it accepts a missing comma
/// between object properties or array elements without complaint. Accepting
/// input that strict JSONC tooling rejects would make nopal bless config other
/// tools break on, so a token-level strictness pass runs after the lenient
/// parse and turns those omissions into parse errors.
pub fn parse_jsonc(text: &str, path: &str, code: Code) -> Result<serde_json::Value, Diagnostic> {
    let value = match jsonc_parser::parse_to_serde_value(text, &parse_options()) {
        Ok(Some(value)) => value,
        Ok(None) => return Err(Diagnostic::error(code, path, "file contains no JSON value")),
        Err(err) => {
            let (line, column) = line_col(text, err.range().start);
            return Err(
                Diagnostic::error(code, path, err.kind().to_string()).with_position(line, column)
            );
        }
    };

    let tokens = jsonc_parser::parse_to_ast(
        text,
        &jsonc_parser::CollectOptions {
            comments: jsonc_parser::CommentCollectionStrategy::Off,
            tokens: true,
        },
        &parse_options(),
    )
    .ok()
    .and_then(|result| result.tokens)
    .unwrap_or_default();

    if let Some((offset, message)) = strict_comma_violation(&tokens) {
        let (line, column) = line_col(text, offset);
        return Err(Diagnostic::error(code, path, message).with_position(line, column));
    }

    Ok(value)
}

/// Token-level check that sibling object properties and array elements are
/// comma-separated. Returns the byte offset and message of the first
/// violation. Trailing commas are fine; structure errors beyond commas are
/// the parser's job and are ignored here.
fn strict_comma_violation(
    tokens: &[jsonc_parser::tokens::TokenAndRange],
) -> Option<(usize, String)> {
    use jsonc_parser::tokens::Token;

    #[derive(Clone, Copy)]
    enum Frame {
        ObjAwaitKey,
        ObjAwaitColon,
        ObjAwaitValue,
        ObjAwaitComma,
        ArrAwaitValue,
        ArrAwaitComma,
    }

    fn set(stack: &mut [Frame], frame: Frame) {
        let index = stack.len() - 1;
        stack[index] = frame;
    }

    let mut stack: Vec<Frame> = Vec::new();
    for entry in tokens {
        let token = &entry.token;
        let is_scalar = matches!(
            token,
            Token::String(_) | Token::Word(_) | Token::Boolean(_) | Token::Number(_) | Token::Null
        );
        match stack.last().copied() {
            None => match token {
                Token::OpenBrace => stack.push(Frame::ObjAwaitKey),
                Token::OpenBracket => stack.push(Frame::ArrAwaitValue),
                _ => {}
            },
            Some(Frame::ObjAwaitKey) => match token {
                Token::CloseBrace => {
                    stack.pop();
                }
                _ if is_scalar => set(&mut stack, Frame::ObjAwaitColon),
                _ => {}
            },
            Some(Frame::ObjAwaitColon) => {
                if matches!(token, Token::Colon) {
                    set(&mut stack, Frame::ObjAwaitValue);
                }
            }
            Some(Frame::ObjAwaitValue) => match token {
                _ if is_scalar => set(&mut stack, Frame::ObjAwaitComma),
                Token::OpenBrace => {
                    set(&mut stack, Frame::ObjAwaitComma);
                    stack.push(Frame::ObjAwaitKey);
                }
                Token::OpenBracket => {
                    set(&mut stack, Frame::ObjAwaitComma);
                    stack.push(Frame::ArrAwaitValue);
                }
                _ => {}
            },
            Some(Frame::ObjAwaitComma) => match token {
                Token::Comma => set(&mut stack, Frame::ObjAwaitKey),
                Token::CloseBrace => {
                    stack.pop();
                }
                _ => {
                    return Some((
                        entry.range.start,
                        "missing comma between object properties".to_owned(),
                    ));
                }
            },
            Some(Frame::ArrAwaitValue) => match token {
                Token::CloseBracket => {
                    stack.pop();
                }
                _ if is_scalar => set(&mut stack, Frame::ArrAwaitComma),
                Token::OpenBrace => {
                    set(&mut stack, Frame::ArrAwaitComma);
                    stack.push(Frame::ObjAwaitKey);
                }
                Token::OpenBracket => {
                    set(&mut stack, Frame::ArrAwaitComma);
                    stack.push(Frame::ArrAwaitValue);
                }
                _ => {}
            },
            Some(Frame::ArrAwaitComma) => match token {
                Token::Comma => set(&mut stack, Frame::ArrAwaitValue),
                Token::CloseBracket => {
                    stack.pop();
                }
                _ => {
                    return Some((
                        entry.range.start,
                        "missing comma between array elements".to_owned(),
                    ));
                }
            },
        }
    }
    None
}

/// Parse manifest text. Returns whatever could be understood plus every
/// diagnostic found; `manifest` is `None` only when the file did not parse.
pub fn parse_manifest(text: &str, path: &str) -> (Option<Manifest>, Vec<Diagnostic>) {
    let root = match parse_jsonc(text, path, Code::ManifestParseError) {
        Ok(value) => value,
        Err(diagnostic) => return (None, vec![diagnostic]),
    };

    let mut diagnostics = Vec::new();

    match root.get("version").and_then(|v| v.as_str()) {
        Some(MANIFEST_KIND) => {}
        Some(other) => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("unsupported manifest version {other:?}; expected {MANIFEST_KIND:?}"),
        )),
        None => diagnostics.push(Diagnostic::error(
            Code::VersionUnsupported,
            path,
            format!("missing string field \"version\"; expected {MANIFEST_KIND:?}"),
        )),
    }

    let (profile, required_modules) = match root.get("profile").and_then(|v| v.as_str()) {
        Some(name) => {
            let manifest_modules = profile_required_modules(&root, name, path, &mut diagnostics);
            match (profile::builtin_required_modules(name), manifest_modules) {
                (_, Some(modules)) => (Some(Profile::new(name)), modules),
                (Some(modules), None) => (Some(Profile::new(name)), modules.to_vec()),
                (None, None) => {
                    diagnostics.push(Diagnostic::error(
                        Code::ProfileUnknown,
                        path,
                        format!(
                            "unknown profile {name:?}; declare profiles.{name}.required_modules or use a built-in profile ({})",
                            known_profiles()
                        ),
                    ));
                    (None, Vec::new())
                }
            }
        }
        None => {
            diagnostics.push(Diagnostic::error(
                Code::ProfileUnknown,
                path,
                format!(
                    "missing string field \"profile\"; expected a built-in profile ({}) or profiles.<name>.required_modules",
                    known_profiles()
                ),
            ));
            (None, Vec::new())
        }
    };

    let project_name = root
        .get("project")
        .and_then(|p| p.get("name"))
        .and_then(|n| n.as_str())
        .map(str::to_owned);

    (
        Some(Manifest {
            project_name,
            profile,
            required_modules,
        }),
        diagnostics,
    )
}

fn profile_required_modules(
    root: &serde_json::Value,
    profile_name: &str,
    path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) -> Option<Vec<Module>> {
    let profile_value = root.get("profiles")?.get(profile_name)?;
    let Some(items) = profile_value.get("required_modules") else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("profiles.{profile_name}.required_modules must be an array of module names"),
        ));
        return Some(Vec::new());
    };
    let Some(items) = items.as_array() else {
        diagnostics.push(Diagnostic::error(
            Code::FieldInvalid,
            path,
            format!("profiles.{profile_name}.required_modules must be an array of module names"),
        ));
        return Some(Vec::new());
    };

    let mut modules = Vec::new();
    for item in items {
        let Some(name) = item.as_str() else {
            diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!(
                    "profiles.{profile_name}.required_modules contains unknown module {:?}; expected one of {}",
                    item,
                    Module::known_names()
                ),
            ));
            continue;
        };
        if let Some(migration) = removed_module_migration(name) {
            diagnostics.push(Diagnostic::error(
                Code::ProductSurfaceRemoved,
                path,
                format!(
                    "profiles.{profile_name}.required_modules still names removed v0.2 module {name:?}; {migration}"
                ),
            ));
            continue;
        }
        match Module::parse(name) {
            Some(module) if !modules.contains(&module) => modules.push(module),
            Some(_) => {}
            None => diagnostics.push(Diagnostic::error(
                Code::FieldInvalid,
                path,
                format!(
                    "profiles.{profile_name}.required_modules contains unknown module {:?}; expected one of {}",
                    item,
                    Module::known_names()
                ),
            )),
        }
    }
    Some(modules)
}

pub(crate) fn removed_module_migration(name: &str) -> Option<&'static str> {
    match name {
        "field" | "plot" | "session" => {
            Some("remove the module and use bare `nopal` for an enforced Pi session")
        }
        "rondo" | "memento" | "herdr" => Some(
            "remove the module; optional external profiles are not part of the v0.3 distribution",
        ),
        "integrations" => Some(
            "keep tracker, review, and lifecycle integration meaning in `.beislid/workflow.md`",
        ),
        _ => None,
    }
}

fn known_profiles() -> String {
    "\"minimal\", \"portable\"".to_owned()
}

/// 1-based line/column for a byte offset into `text`.
fn line_col(text: &str, offset: usize) -> (usize, usize) {
    let clamped = offset.min(text.len());
    let before = &text[..clamped];
    let line = before.bytes().filter(|&b| b == b'\n').count() + 1;
    let column = before.rfind('\n').map_or(before.chars().count() + 1, |nl| {
        before[nl + 1..].chars().count() + 1
    });
    (line, column)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_manifest_parses_clean() {
        let text = r#"{
            // comment allowed
            "version": "nopal.project/v1",
            "project": { "name": "x" },
            "profile": "minimal",
        }"#;
        let (manifest, diagnostics) = parse_manifest(text, ".nopal/nopal.jsonc");
        assert_eq!(diagnostics, vec![]);
        let manifest = manifest.unwrap();
        assert_eq!(manifest.project_name.as_deref(), Some("x"));
        assert_eq!(
            manifest.profile.as_ref().map(Profile::as_str),
            Some("minimal")
        );
        assert_eq!(manifest.required_modules, vec![]);
    }

    #[test]
    fn bad_version_and_profile_both_reported() {
        let text = r#"{ "version": "nopal.project/v2", "profile": "mega" }"#;
        let (manifest, diagnostics) = parse_manifest(text, ".nopal/nopal.jsonc");
        assert!(manifest.is_some());
        let codes: Vec<_> = diagnostics.iter().map(|d| d.code).collect();
        assert_eq!(codes, vec![Code::VersionUnsupported, Code::ProfileUnknown]);
    }

    #[test]
    fn manifest_defined_profile_parses_required_modules() {
        let text = r#"{
            "version": "nopal.project/v1",
            "profile": "custom",
            "profiles": { "custom": { "required_modules": ["gates", "guidance"] } }
        }"#;
        let (manifest, diagnostics) = parse_manifest(text, ".nopal/nopal.jsonc");
        assert_eq!(diagnostics, vec![]);
        let manifest = manifest.unwrap();
        assert_eq!(
            manifest.profile.as_ref().map(Profile::as_str),
            Some("custom")
        );
        assert_eq!(
            manifest.required_modules,
            vec![Module::Gates, Module::Guidance]
        );
    }

    #[test]
    fn manifest_profile_accepts_roots_as_a_first_class_module() {
        let text = r#"{
            "version": "nopal.project/v1",
            "profile": "custom",
            "profiles": { "custom": { "required_modules": ["roots"] } }
        }"#;

        let (manifest, diagnostics) = parse_manifest(text, "nopal.jsonc");

        assert!(diagnostics.is_empty(), "{diagnostics:?}");
        assert_eq!(manifest.unwrap().required_modules, vec![Module::Roots]);
    }

    #[test]
    fn removed_required_module_has_an_actionable_migration_diagnostic() {
        let text = r#"{
            "version": "nopal.project/v1",
            "profile": "custom",
            "profiles": { "custom": { "required_modules": ["field", "integrations"] } }
        }"#;
        let (_, diagnostics) = parse_manifest(text, ".nopal/nopal.jsonc");
        assert_eq!(
            diagnostics
                .iter()
                .map(|diagnostic| diagnostic.code)
                .collect::<Vec<_>>(),
            vec![Code::ProductSurfaceRemoved, Code::ProductSurfaceRemoved]
        );
        assert!(diagnostics[0].message.contains("bare `nopal`"));
        assert!(diagnostics[1].message.contains(".beislid/workflow.md"));
    }

    #[test]
    fn unknown_required_module_is_field_invalid() {
        let text = r#"{
            "version": "nopal.project/v1",
            "profile": "custom",
            "profiles": { "custom": { "required_modules": ["agents"] } }
        }"#;
        let (_, diagnostics) = parse_manifest(text, ".nopal/nopal.jsonc");
        assert_eq!(
            diagnostics.iter().map(|d| d.code).collect::<Vec<_>>(),
            vec![Code::FieldInvalid]
        );
    }

    #[test]
    fn missing_comma_between_properties_is_a_parse_error() {
        let text = "{\n  \"a\": 1\n  \"b\": 2\n}";
        let err = parse_jsonc(text, "x.jsonc", Code::ManifestParseError).unwrap_err();
        assert_eq!(err.code, Code::ManifestParseError);
        assert!(err.message.contains("missing comma"), "{}", err.message);
        assert_eq!(err.position.unwrap().line, 3);
    }

    #[test]
    fn missing_comma_between_array_elements_is_a_parse_error() {
        let text = "{ \"a\": [1\n 2] }";
        let err = parse_jsonc(text, "x.jsonc", Code::ManifestParseError).unwrap_err();
        assert!(err.message.contains("missing comma"), "{}", err.message);
        assert_eq!(err.position.unwrap().line, 2);
    }

    #[test]
    fn missing_comma_before_nested_container_is_a_parse_error() {
        let text = "{ \"a\": 1 \"b\": { \"c\": 2 } }";
        assert!(parse_jsonc(text, "x.jsonc", Code::ManifestParseError).is_err());
    }

    #[test]
    fn unquoted_property_names_are_a_parse_error() {
        let text = "{ version: \"nopal.project/v1\" }";
        assert!(parse_jsonc(text, "x.jsonc", Code::ManifestParseError).is_err());
    }

    #[test]
    fn comments_and_trailing_commas_remain_allowed() {
        let text = "{\n  // jsonc niceties stay\n  \"a\": [1, 2,],\n}";
        assert!(parse_jsonc(text, "x.jsonc", Code::ManifestParseError).is_ok());
    }

    #[test]
    fn parse_error_carries_position() {
        let text = "{\n  \"version\" \"oops\"\n}";
        let (manifest, diagnostics) = parse_manifest(text, ".nopal/nopal.jsonc");
        assert!(manifest.is_none());
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].code, Code::ManifestParseError);
        let position = diagnostics[0].position.unwrap();
        assert_eq!(position.line, 2);
    }
}
