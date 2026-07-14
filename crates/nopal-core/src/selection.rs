//! Deterministic gate selection over changed files.
//!
//! Selection walks selectors in declaration order, pulls their gate sets in
//! reference order, and dedups gates by first selection. The stage filter is
//! applied last so a gate a selector wanted at the wrong stage is reported
//! as `stage_mismatch` rather than silently vanishing. When no selectors are
//! configured, every gate at the requested stage is default-selected. Every
//! outcome carries its explanation; nopal never executes what it selects.

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::Serialize;

use crate::gates::{Gate, GateStage, GatesConfig};

/// Why a gate did not make the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// A selector (or default selection) wanted it, but at another stage.
    StageMismatch,
    /// Selectors are configured and none that matched references it.
    NotSelected,
}

impl SkipReason {
    pub fn as_str(self) -> &'static str {
        match self {
            SkipReason::StageMismatch => "stage_mismatch",
            SkipReason::NotSelected => "not_selected",
        }
    }
}

/// How a gate was pulled into consideration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Via {
    /// No selectors configured; every stage-matching gate applies.
    Default,
    /// First selector/set pair that referenced the gate.
    Selector { selector: String, set: String },
}

impl Via {
    /// One-line display form for tables.
    pub fn display(&self) -> String {
        match self {
            Via::Default => "default".to_owned(),
            Via::Selector { selector, set } => format!("selector:{selector} set:{set}"),
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct SelectedGate {
    pub id: String,
    pub stage: GateStage,
    pub run: crate::gates::Run,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autofix: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parallel_safe: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mutates: Option<bool>,
    pub via: Via,
}

#[derive(Debug, Clone, Serialize)]
pub struct SkippedGate {
    pub id: String,
    pub stage: GateStage,
    pub reason: SkipReason,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub via: Option<Via>,
}

/// Per-selector match evidence: which changed files pulled it in.
#[derive(Debug, Clone, Serialize)]
pub struct SelectorMatch {
    pub name: String,
    pub matched: bool,
    pub matched_files: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Selection {
    pub stage: GateStage,
    /// Sorted, deduplicated input; selection is order-insensitive.
    pub changed_files: Vec<String>,
    pub selectors: Vec<SelectorMatch>,
    pub selected: Vec<SelectedGate>,
    pub skipped: Vec<SkippedGate>,
}

/// Select gates for `stage` given `changed_files`. Call only on a config
/// that validated without errors; glob patterns that fail to compile here
/// select nothing for that selector (validation is the place that reports
/// them).
pub fn select(config: &GatesConfig, stage: GateStage, changed_files: &[String]) -> Selection {
    let mut files: Vec<String> = changed_files.to_vec();
    files.sort();
    files.dedup();

    // Gate id -> first Via that pulled it in, in selection order.
    let mut pulled: Vec<(String, Via)> = Vec::new();
    let mut selectors = Vec::new();

    if config.selectors.is_empty() {
        for gate in &config.gates {
            pulled.push((gate.id.clone(), Via::Default));
        }
    } else {
        for selector in &config.selectors {
            let matched_files = matching_files(&files, &selector.paths, &selector.exclude);
            let matched = !matched_files.is_empty();
            if matched {
                for set_name in &selector.gate_sets {
                    let Some(set) = config.gate_sets.get(set_name) else {
                        continue;
                    };
                    for id in &set.gates {
                        if !pulled.iter().any(|(pulled_id, _)| pulled_id == id) {
                            pulled.push((
                                id.clone(),
                                Via::Selector {
                                    selector: selector.name.clone(),
                                    set: set_name.clone(),
                                },
                            ));
                        }
                    }
                }
            }
            selectors.push(SelectorMatch {
                name: selector.name.clone(),
                matched,
                matched_files,
            });
        }
    }

    let mut selected = Vec::new();
    let mut skipped = Vec::new();

    // Selected gates in selection order; stage filter last so wrong-stage
    // pulls are visible.
    for (id, via) in &pulled {
        let Some(gate) = find_gate(config, id) else {
            continue;
        };
        if gate.stage == stage {
            selected.push(SelectedGate {
                id: gate.id.clone(),
                stage: gate.stage.clone(),
                run: gate.run.clone(),
                cwd: gate.cwd.clone(),
                autofix: gate.autofix.clone(),
                parallel_safe: gate.parallel_safe,
                mutates: gate.mutates,
                via: via.clone(),
            });
        } else {
            skipped.push(SkippedGate {
                id: gate.id.clone(),
                stage: gate.stage.clone(),
                reason: SkipReason::StageMismatch,
                via: Some(via.clone()),
            });
        }
    }

    // Never-pulled gates in declaration order, after the pulled ones.
    for gate in &config.gates {
        if !pulled.iter().any(|(id, _)| id == &gate.id) {
            skipped.push(SkippedGate {
                id: gate.id.clone(),
                stage: gate.stage.clone(),
                reason: SkipReason::NotSelected,
                via: None,
            });
        }
    }

    Selection {
        stage,
        changed_files: files,
        selectors,
        selected,
        skipped,
    }
}

fn find_gate<'a>(config: &'a GatesConfig, id: &str) -> Option<&'a Gate> {
    config.gates.iter().find(|gate| gate.id == id)
}

/// Changed files matching any `paths` glob and no `exclude` glob, in the
/// (sorted) input order.
fn matching_files(files: &[String], paths: &[String], exclude: &[String]) -> Vec<String> {
    let Some(include) = glob_set(paths) else {
        return Vec::new();
    };
    let exclude = if exclude.is_empty() {
        None
    } else {
        glob_set(exclude)
    };
    files
        .iter()
        .filter(|file| {
            include.is_match(file.as_str())
                && exclude
                    .as_ref()
                    .is_none_or(|set| !set.is_match(file.as_str()))
        })
        .cloned()
        .collect()
}

/// Compile globs with literal path separators: `*` stays inside one path
/// segment, `**` crosses directories (beislid's selector semantics). Shared
/// with review_policy.rs so risk-path glob matching stays behaviorally
/// identical to gate selector matching.
pub(crate) fn glob_set(patterns: &[String]) -> Option<GlobSet> {
    let mut builder = GlobSetBuilder::new();
    for pattern in patterns {
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .build()
            .ok()?;
        builder.add(glob);
    }
    builder.build().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gates::parse_gates;

    const CONFIG: &str = r#"{
        "version": "nopal.gates/v1",
        "gates": [
            { "id": "fmt", "stage": "pre_pr", "command": "cargo fmt --all --check" },
            { "id": "clippy", "stage": "pre_pr", "command": "cargo clippy" },
            { "id": "docs", "stage": "pre_pr", "command": "lint docs" },
            { "id": "bench", "stage": "continuous", "command": "cargo bench" }
        ],
        "gate_sets": {
            "rust": { "gates": ["fmt", "clippy", "bench"] },
            "docs": { "gates": ["docs"] }
        },
        "selectors": [
            { "name": "rust-files", "paths": ["**/*.rs"], "exclude": ["target/**"],
              "gate_sets": ["rust"] },
            { "name": "doc-files", "paths": ["docs/**", "*.md"], "gate_sets": ["docs"] }
        ]
    }"#;

    fn config(text: &str) -> GatesConfig {
        let (parsed, diagnostics) = parse_gates(text, "gates.jsonc");
        assert_eq!(diagnostics, vec![], "fixture must validate clean");
        parsed.expect("fixture parses")
    }

    fn files(names: &[&str]) -> Vec<String> {
        names.iter().map(|n| (*n).to_owned()).collect()
    }

    fn selected_ids(selection: &Selection) -> Vec<&str> {
        selection.selected.iter().map(|g| g.id.as_str()).collect()
    }

    #[test]
    fn selector_pulls_matching_sets_in_order() {
        let selection = select(
            &config(CONFIG),
            GateStage::PrePr,
            &files(&["src/lib.rs", "README.md"]),
        );
        assert_eq!(selected_ids(&selection), vec!["fmt", "clippy", "docs"]);
        assert_eq!(
            selection.selected[0].via,
            Via::Selector {
                selector: "rust-files".into(),
                set: "rust".into()
            }
        );
        // bench was pulled by the rust selector but declares another stage.
        assert_eq!(selection.skipped.len(), 1);
        assert_eq!(selection.skipped[0].id, "bench");
        assert_eq!(selection.skipped[0].reason, SkipReason::StageMismatch);
    }

    #[test]
    fn unmatched_selector_gates_are_not_selected() {
        let selection = select(&config(CONFIG), GateStage::PrePr, &files(&["src/lib.rs"]));
        assert_eq!(selected_ids(&selection), vec!["fmt", "clippy"]);
        let docs = selection
            .skipped
            .iter()
            .find(|s| s.id == "docs")
            .expect("docs is reported");
        assert_eq!(docs.reason, SkipReason::NotSelected);
        assert!(docs.via.is_none());
        let doc_selector = &selection.selectors[1];
        assert!(!doc_selector.matched);
        assert_eq!(doc_selector.matched_files, Vec::<String>::new());
    }

    #[test]
    fn exclude_globs_remove_matches() {
        let selection = select(
            &config(CONFIG),
            GateStage::PrePr,
            &files(&["target/generated.rs"]),
        );
        assert_eq!(selected_ids(&selection), Vec::<&str>::new());
        assert!(!selection.selectors[0].matched);
    }

    #[test]
    fn single_star_does_not_cross_directories() {
        // `*.md` must not match `docs/guide/x.md`; `docs/**` does.
        let selection = select(
            &config(CONFIG),
            GateStage::PrePr,
            &files(&["notes/deep/x.md"]),
        );
        assert_eq!(selected_ids(&selection), Vec::<&str>::new());
    }

    #[test]
    fn changed_files_are_sorted_and_deduped() {
        let selection = select(
            &config(CONFIG),
            GateStage::PrePr,
            &files(&["b.rs", "a.rs", "b.rs"]),
        );
        assert_eq!(selection.changed_files, files(&["a.rs", "b.rs"]));
        assert_eq!(
            selection.selectors[0].matched_files,
            files(&["a.rs", "b.rs"])
        );
    }

    #[test]
    fn selection_is_deterministic_regardless_of_input_order() {
        let forward = select(
            &config(CONFIG),
            GateStage::PrePr,
            &files(&["src/lib.rs", "README.md"]),
        );
        let reverse = select(
            &config(CONFIG),
            GateStage::PrePr,
            &files(&["README.md", "src/lib.rs"]),
        );
        assert_eq!(selected_ids(&forward), selected_ids(&reverse));
        assert_eq!(forward.changed_files, reverse.changed_files);
    }

    #[test]
    fn no_selectors_default_selects_stage_matching_gates() {
        let text = r#"{
            "version": "nopal.gates/v1",
            "gates": [
                { "id": "fmt", "stage": "pre_pr", "command": "x" },
                { "id": "bench", "stage": "continuous", "command": "y" }
            ]
        }"#;
        let selection = select(&config(text), GateStage::PrePr, &[]);
        assert_eq!(selected_ids(&selection), vec!["fmt"]);
        assert_eq!(selection.selected[0].via, Via::Default);
        assert_eq!(selection.skipped[0].id, "bench");
        assert_eq!(selection.skipped[0].reason, SkipReason::StageMismatch);
    }

    #[test]
    fn first_selection_wins_for_gates_in_multiple_sets() {
        let text = r#"{
            "version": "nopal.gates/v1",
            "gates": [ { "id": "fmt", "stage": "pre_pr", "command": "x" } ],
            "gate_sets": {
                "a": { "gates": ["fmt"] },
                "b": { "gates": ["fmt"] }
            },
            "selectors": [
                { "name": "first", "paths": ["**"], "gate_sets": ["a"] },
                { "name": "second", "paths": ["**"], "gate_sets": ["b"] }
            ]
        }"#;
        let selection = select(&config(text), GateStage::PrePr, &files(&["x.rs"]));
        assert_eq!(selection.selected.len(), 1);
        assert_eq!(
            selection.selected[0].via,
            Via::Selector {
                selector: "first".into(),
                set: "a".into()
            }
        );
    }
}
