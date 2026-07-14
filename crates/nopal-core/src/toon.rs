//! Minimal TOON encoder.
//!
//! This is deliberately not a general TOON implementation. It encodes exactly
//! the subset nopal's output model needs, deterministically:
//!
//! - object entries as `key: value`, nested objects indented two spaces
//! - scalar arrays inline as `key[N]: a,b,c`, nested arrays as indented items
//! - tabular arrays as `key[N]{f1,f2}:` with one comma-joined row per line
//! - strings quoted only when they would be ambiguous (empty, structural
//!   characters, surrounding whitespace, or scalar look-alikes)
//!
//! Key order is insertion order; encoding the same value twice yields
//! byte-identical output.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Str(String),
    Bool(bool),
    Int(i64),
    Number(String),
    /// Nested object; keys render in the order given.
    Obj(Vec<(String, Value)>),
    /// Array of values; scalar-only arrays render inline.
    Arr(Vec<Value>),
    /// Uniform array of objects, rendered in tabular form. Cells are scalar
    /// values so types render faithfully (booleans stay unquoted).
    Table {
        fields: Vec<String>,
        rows: Vec<Vec<Value>>,
    },
}

impl Value {
    pub fn str(s: impl Into<String>) -> Value {
        Value::Str(s.into())
    }
}

/// Convert a JSON value into the structurally equivalent TOON value tree.
pub fn from_json(value: &serde_json::Value) -> Value {
    match value {
        serde_json::Value::Null => Value::Null,
        serde_json::Value::Bool(value) => Value::Bool(*value),
        serde_json::Value::Number(value) => Value::Number(value.to_string()),
        serde_json::Value::String(value) => Value::str(value),
        serde_json::Value::Array(items) => Value::Arr(items.iter().map(from_json).collect()),
        serde_json::Value::Object(object) => Value::Obj(
            object
                .iter()
                .map(|(key, value)| (key.clone(), from_json(value)))
                .collect(),
        ),
    }
}

/// Convert a TOON value tree back to JSON for structural contract checks.
pub fn to_json(value: &Value) -> serde_json::Value {
    match value {
        Value::Null => serde_json::Value::Null,
        Value::Str(value) => serde_json::Value::String(value.clone()),
        Value::Bool(value) => serde_json::Value::Bool(*value),
        Value::Int(value) => serde_json::Value::Number((*value).into()),
        Value::Number(value) => value
            .parse::<serde_json::Number>()
            .map_or(serde_json::Value::Null, serde_json::Value::Number),
        Value::Obj(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(key, value)| (key.clone(), to_json(value)))
                .collect(),
        ),
        Value::Arr(items) => serde_json::Value::Array(items.iter().map(to_json).collect()),
        Value::Table { fields, rows } => serde_json::Value::Array(
            rows.iter()
                .map(|row| {
                    serde_json::Value::Object(
                        fields
                            .iter()
                            .zip(row)
                            .map(|(field, value)| (field.clone(), to_json(value)))
                            .collect(),
                    )
                })
                .collect(),
        ),
    }
}

/// Encode a top-level object (TOON documents are objects).
pub fn encode(entries: &[(String, Value)]) -> String {
    let mut out = String::new();
    encode_obj(entries, 0, &mut out);
    out
}

fn encode_obj(entries: &[(String, Value)], indent: usize, out: &mut String) {
    for (key, value) in entries {
        let pad = "  ".repeat(indent);
        let key = render_key(key);
        match value {
            Value::Null | Value::Str(_) | Value::Bool(_) | Value::Int(_) | Value::Number(_) => {
                out.push_str(&format!("{pad}{key}: {}\n", scalar(value)));
            }
            Value::Obj(children) => {
                out.push_str(&format!("{pad}{key}:\n"));
                encode_obj(children, indent + 1, out);
            }
            Value::Arr(items) => {
                if items.is_empty() {
                    out.push_str(&format!("{pad}{key}[0]:\n"));
                } else if items.iter().all(is_scalar) {
                    let rendered: Vec<String> = items.iter().map(scalar).collect();
                    out.push_str(&format!(
                        "{pad}{key}[{}]: {}\n",
                        rendered.len(),
                        rendered.join(",")
                    ));
                } else {
                    out.push_str(&format!("{pad}{key}[{}]:\n", items.len()));
                    encode_array_items(items, indent + 1, out);
                }
            }
            Value::Table { fields, rows } => {
                let fields = render_fields(fields);
                out.push_str(&format!("{pad}{key}[{}]{{{}}}:\n", rows.len(), fields));
                let row_pad = "  ".repeat(indent + 1);
                for row in rows {
                    let cells: Vec<String> = row.iter().map(scalar).collect();
                    out.push_str(&format!("{row_pad}{}\n", cells.join(",")));
                }
            }
        }
    }
}

fn scalar(value: &Value) -> String {
    match value {
        Value::Null => "null".to_owned(),
        Value::Str(s) => quote_if_needed(s),
        Value::Bool(b) => b.to_string(),
        Value::Int(i) => i.to_string(),
        Value::Number(number) => number
            .parse::<serde_json::Number>()
            .map_or_else(|_| json_quote(number), |_| number.clone()),
        // Nested containers inside scalar positions are a programming error in
        // our own output model; render a quoted placeholder rather than panic.
        Value::Obj(_) | Value::Arr(_) | Value::Table { .. } => "\"<non-scalar>\"".to_owned(),
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(
        value,
        Value::Null | Value::Str(_) | Value::Bool(_) | Value::Int(_) | Value::Number(_)
    )
}

fn encode_array_items(items: &[Value], indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    for item in items {
        match item {
            Value::Obj(entries) => {
                encode_array_object(entries, indent, out);
            }
            Value::Arr(children) => {
                out.push_str(&format!("{pad}-[{}]:\n", children.len()));
                encode_array_items(children, indent + 1, out);
            }
            Value::Table { fields, rows } => {
                let fields = render_fields(fields);
                out.push_str(&format!("{pad}-[{}]{{{}}}:\n", rows.len(), fields));
                let row_pad = "  ".repeat(indent + 1);
                for row in rows {
                    let cells: Vec<String> = row.iter().map(scalar).collect();
                    out.push_str(&format!("{row_pad}{}\n", cells.join(",")));
                }
            }
            scalar_value => out.push_str(&format!("{pad}- {}\n", scalar(scalar_value))),
        }
    }
}

fn encode_array_object(entries: &[(String, Value)], indent: usize, out: &mut String) {
    let pad = "  ".repeat(indent);
    let Some(((key, value), rest)) = entries.split_first() else {
        out.push_str(&format!("{pad}- {{}}\n"));
        return;
    };
    let key = render_key(key);

    match value {
        value if is_scalar(value) => {
            out.push_str(&format!("{pad}- {key}: {}\n", scalar(value)));
        }
        Value::Obj(children) => {
            out.push_str(&format!("{pad}- {key}:\n"));
            encode_obj(children, indent + 2, out);
        }
        Value::Arr(children) => {
            out.push_str(&format!("{pad}- {key}[{}]:\n", children.len()));
            encode_array_items(children, indent + 2, out);
        }
        Value::Table { fields, rows } => {
            let fields = render_fields(fields);
            out.push_str(&format!("{pad}- {key}[{}]{{{}}}:\n", rows.len(), fields));
            let row_pad = "  ".repeat(indent + 2);
            for row in rows {
                let cells: Vec<String> = row.iter().map(scalar).collect();
                out.push_str(&format!("{row_pad}{}\n", cells.join(",")));
            }
        }
        Value::Null | Value::Str(_) | Value::Bool(_) | Value::Int(_) | Value::Number(_) => {}
    }
    encode_obj(rest, indent + 1, out);
}

fn quote_if_needed(s: &str) -> String {
    if needs_quoting(s) {
        json_quote(s)
    } else {
        s.to_owned()
    }
}

fn render_key(key: &str) -> String {
    quote_if_needed(key)
}

fn render_fields(fields: &[String]) -> String {
    fields
        .iter()
        .map(|field| render_key(field))
        .collect::<Vec<_>>()
        .join(",")
}

fn json_quote(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
}

fn needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    if s.starts_with(char::is_whitespace) || s.ends_with(char::is_whitespace) {
        return true;
    }
    if s.contains([',', ':', '"', '\n', '[', ']', '{', '}']) {
        return true;
    }
    if s.chars().any(char::is_control) {
        return true;
    }
    // Scalar look-alikes must be quoted so a reader can round-trip types.
    if s == "true" || s == "false" || s == "null" {
        return true;
    }
    s.parse::<f64>().is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn obj(entries: &[(&str, Value)]) -> Vec<(String, Value)> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), v.clone()))
            .collect()
    }

    #[test]
    fn scalars_render_as_key_value_lines() {
        let doc = obj(&[
            ("kind", Value::str("nopal.status/v1")),
            ("ready", Value::Bool(true)),
            ("count", Value::Int(3)),
        ]);
        assert_eq!(
            encode(&doc),
            "kind: nopal.status/v1\nready: true\ncount: 3\n"
        );
    }

    #[test]
    fn nested_objects_indent_two_spaces() {
        let doc = obj(&[("project", Value::Obj(obj(&[("name", Value::str("x"))])))]);
        assert_eq!(encode(&doc), "project:\n  name: x\n");
    }

    #[test]
    fn scalar_arrays_render_inline_with_length() {
        let doc = obj(&[(
            "help",
            Value::Arr(vec![Value::str("run nopal validate"), Value::str("done")]),
        )]);
        assert_eq!(encode(&doc), "help[2]: run nopal validate,done\n");
    }

    #[test]
    fn empty_arrays_render_zero_length_and_no_items() {
        let doc = obj(&[("help", Value::Arr(vec![]))]);
        assert_eq!(encode(&doc), "help[0]:\n");
    }

    #[test]
    fn tables_render_header_then_rows() {
        let doc = obj(&[(
            "modules",
            Value::Table {
                fields: vec!["name".into(), "state".into()],
                rows: vec![
                    vec![Value::str("gates"), Value::str("ok")],
                    vec![Value::str("policy"), Value::Bool(true)],
                ],
            },
        )]);
        assert_eq!(
            encode(&doc),
            "modules[2]{name,state}:\n  gates,ok\n  policy,true\n"
        );
    }

    #[test]
    fn ambiguous_strings_are_quoted() {
        for (input, expected) in [
            ("", "\"\""),
            ("a,b", "\"a,b\""),
            ("a: b", "\"a: b\""),
            (" padded", "\" padded\""),
            ("true", "\"true\""),
            ("42", "\"42\""),
            ("line\nbreak", "\"line\\nbreak\""),
            ("plain text", "plain text"),
        ] {
            let doc = obj(&[("v", Value::str(input))]);
            assert_eq!(encode(&doc), format!("v: {expected}\n"), "input: {input:?}");
        }
    }

    #[test]
    fn controls_and_structural_keys_use_json_compatible_escaping_everywhere() {
        let controls = "carriage\rreturn\ttab\0nul\u{1b}esc\u{8}backspace\u{c}formfeed";
        let manifest_path = "C:\\repo\\slice\nmanifest.json";
        let doc = vec![
            ("plain".to_owned(), Value::str(controls)),
            ("unsafe:key".to_owned(), Value::str(manifest_path)),
            ("line\nkey".to_owned(), Value::Bool(true)),
            (
                "nested".to_owned(),
                Value::Arr(vec![Value::Obj(vec![(
                    "inner:key".to_owned(),
                    Value::str("line\nvalue"),
                )])]),
            ),
            (
                "table".to_owned(),
                Value::Table {
                    fields: vec!["field:one".to_owned(), "field\ntwo".to_owned()],
                    rows: vec![vec![Value::str(controls), Value::str(manifest_path)]],
                },
            ),
        ];

        let rendered = encode(&doc);

        assert!(
            rendered.contains(
                r#"plain: "carriage\rreturn\ttab\u0000nul\u001besc\bbackspace\fformfeed""#
            )
        );
        assert!(rendered.contains(r#""unsafe:key": "C:\\repo\\slice\nmanifest.json""#));
        assert!(rendered.contains(r#""line\nkey": true"#));
        assert!(rendered.contains(r#"- "inner:key": "line\nvalue""#));
        assert!(rendered.contains(r#"table[1]{"field:one","field\ntwo"}:"#));
        for control in ['\r', '\t', '\0', '\u{1b}', '\u{8}', '\u{c}'] {
            assert!(
                !rendered.contains(control),
                "raw control {control:?} leaked into TOON: {rendered:?}"
            );
        }
        assert!(!rendered.contains("slice\nmanifest"));
        assert!(!rendered.contains("line\nkey"));
        assert!(!rendered.contains("line\nvalue"));
    }

    #[test]
    fn encoding_is_deterministic() {
        let doc = obj(&[("b", Value::str("second")), ("a", Value::str("first"))]);
        assert_eq!(encode(&doc), encode(&doc));
        // Insertion order is preserved, not sorted.
        assert_eq!(encode(&doc), "b: second\na: first\n");
    }

    #[test]
    fn json_conversion_preserves_nested_objects_arrays_numbers_and_nulls() {
        let json = serde_json::json!({
            "event": {"type": "run.completed", "attempt": 2},
            "items": [null, true, 1.5, {"uri": "rondo-run://run/artifacts/report.json"}]
        });

        let value = from_json(&json);

        assert_eq!(to_json(&value), json);
        let Value::Obj(entries) = value else {
            panic!("JSON object should convert to a TOON object");
        };
        let rendered = encode(&entries);
        assert!(!rendered.contains("<non-scalar>"));
        assert!(rendered.contains("type: run.completed"));
        assert!(rendered.contains("- null"));
        assert!(rendered.contains("uri: \"rondo-run://run/artifacts/report.json\""));
    }

    #[test]
    fn malformed_number_text_cannot_inject_toon_structure() {
        let doc = obj(&[("count", Value::Number("1\nforged: true".to_owned()))]);

        let rendered = encode(&doc);

        assert_eq!(rendered, "count: \"1\\nforged: true\"\n");
    }
}
