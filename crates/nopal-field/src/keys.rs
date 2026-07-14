//! Remappable key action registry.
//!
//! One enum, [`KeyAction`], names every dispatch site in `app.rs` that a
//! `keys` section in the field config (`seat::config::SeatConfig::keys`)
//! is allowed to rebind, plus its default [`KeySpec`]. [`handle_normal_key`]
//! and [`handle_embed_key`] (in `app.rs`) no longer match on literal
//! `KeyCode`s for anything in this table - they call
//! [`KeyRegistry::action_for`] once per key and match on the resolved
//! [`KeyAction`] instead, so a remap changes both dispatch *and* the help
//! overlay / status hint (`ui.rs`) that render [`KeyRegistry::label`] for
//! the same action.
//!
//! [`handle_normal_key`]: crate::app::handle_normal_key
//! [`handle_embed_key`]: crate::app::handle_embed_key
//!
//! Deliberately excluded from this table (see the plan doc, "Not
//! remappable"): digits `1-9` (a fixed jump table), arrow keys, the
//! Ctrl-j/Ctrl-k picker chords, `n`/`N` inside an active scrollback search,
//! every overlay-internal key (the spawn/goto pickers, the context menu,
//! the worktree-name and confirm-kill prompts all stay on literal
//! `KeyCode` matches), and all mouse gestures. Esc is also never part of
//! this table: it is the app's one universal, permanently-hardcoded
//! "cancel/back" convention (row-drag cancel, help dismiss, context-menu
//! dismiss, and - see [`KeyAction::CloseEmbed`]'s doc - closing the
//! embedded panel), so a remap can never take it away.
//!
//! Validation is fail-soft by construction: [`KeyRegistry::build`] never
//! fails outright.
//! An unknown action name or an unparseable key spec is reported and that
//! one action keeps its default; two actions resolving to the same key
//! within the same [`Scope`] revert *both* to their defaults (a scope, not
//! the whole table, is the unit of conflict - the same key can legitimately
//! back different actions in different scopes, e.g. `/` is `filter` in
//! [`Scope::Normal`] and `search` in [`Scope::Embed`]). [`KeyAction::ReleaseInput`]
//! gets no special-cased extra guard beyond membership in that mechanism -
//! see its doc for why the ordinary parse-failure and conflict paths
//! already guarantee it always resolves to something.

use std::collections::{BTreeMap, HashMap};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Every dispatch site `app.rs`'s normal/embed key handlers consult through
/// the registry instead of a literal `KeyCode` pattern. Variant order here
/// only affects [`KeyAction::ALL`]'s iteration order (used for
/// `from_name`); scope membership and default keys are declared
/// separately, in [`NORMAL_ACTIONS`]/[`EMBED_ACTIONS`] and
/// [`KeyAction::default_spec`] below.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyAction {
    Quit,
    Help,
    MoveDown,
    MoveUp,
    SectionNext,
    SectionPrev,
    ActivityNext,
    ActivityPrev,
    Filter,
    SpawnPicker,
    Kill,
    Relaunch,
    /// Jump straight to the ask queue (`a`) - not named in the plan doc's
    /// inventory list, but it is a plain `handle_normal_key` dispatch like
    /// every other sidebar action, so it belongs in the same table.
    AskJump,
    AskApprove,
    AskDeny,
    ShowAll,
    GotoPicker,
    Adopt,
    Reconcile,
    Profiling,
    /// Enter on a seat row: open it live in the embedded panel; on a run
    /// or ask row: the existing detail/context behavior. Only the
    /// top-level dispatch is remappable - Enter's meaning *inside* a
    /// picker/prompt/overlay is untouched (see the module doc).
    OpenView,
    Focus,
    SplitRight,
    SplitBelow,
    BreakToWindow,
    SwapIntoSlot,
    InputFocus,
    /// The sole escape hatch out of held seat input focus. Always resolves
    /// to *some* key - see the module doc's "Validation" paragraph for how
    /// the ordinary fail-soft mechanism already guarantees this without a
    /// separate special case.
    ReleaseInput,
    /// `q` closes the embedded panel and returns to the sidebar. Esc does
    /// the same thing, but is not this action - it is the app's permanent,
    /// non-remappable "leave" key (see the module doc); `close_embed` is
    /// only the *remappable alias* for it.
    CloseEmbed,
    Collapse,
    /// Named `search` (not `embed_search`) in the config, matching the
    /// plan doc's own example.
    EmbedSearch,
}

impl KeyAction {
    /// Every action, in declaration order - the table [`Self::from_name`]
    /// scans and the source [`NORMAL_ACTIONS`]/[`EMBED_ACTIONS`] draw from.
    pub const ALL: &'static [KeyAction] = &[
        KeyAction::Quit,
        KeyAction::Help,
        KeyAction::MoveDown,
        KeyAction::MoveUp,
        KeyAction::SectionNext,
        KeyAction::SectionPrev,
        KeyAction::ActivityNext,
        KeyAction::ActivityPrev,
        KeyAction::Filter,
        KeyAction::SpawnPicker,
        KeyAction::Kill,
        KeyAction::Relaunch,
        KeyAction::AskJump,
        KeyAction::AskApprove,
        KeyAction::AskDeny,
        KeyAction::ShowAll,
        KeyAction::GotoPicker,
        KeyAction::Adopt,
        KeyAction::Reconcile,
        KeyAction::Profiling,
        KeyAction::OpenView,
        KeyAction::Focus,
        KeyAction::SplitRight,
        KeyAction::SplitBelow,
        KeyAction::BreakToWindow,
        KeyAction::SwapIntoSlot,
        KeyAction::InputFocus,
        KeyAction::ReleaseInput,
        KeyAction::CloseEmbed,
        KeyAction::Collapse,
        KeyAction::EmbedSearch,
    ];

    /// The `keys` config's name for this action.
    pub fn name(self) -> &'static str {
        match self {
            KeyAction::Quit => "quit",
            KeyAction::Help => "help",
            KeyAction::MoveDown => "move_down",
            KeyAction::MoveUp => "move_up",
            KeyAction::SectionNext => "section_next",
            KeyAction::SectionPrev => "section_prev",
            KeyAction::ActivityNext => "activity_next",
            KeyAction::ActivityPrev => "activity_prev",
            KeyAction::Filter => "filter",
            KeyAction::SpawnPicker => "spawn_picker",
            KeyAction::Kill => "kill",
            KeyAction::Relaunch => "relaunch",
            KeyAction::AskJump => "ask_jump",
            KeyAction::AskApprove => "ask_approve",
            KeyAction::AskDeny => "ask_deny",
            KeyAction::ShowAll => "show_all",
            KeyAction::GotoPicker => "goto_picker",
            KeyAction::Adopt => "adopt",
            KeyAction::Reconcile => "reconcile",
            KeyAction::Profiling => "profiling",
            KeyAction::OpenView => "open",
            KeyAction::Focus => "focus",
            KeyAction::SplitRight => "split_right",
            KeyAction::SplitBelow => "split_below",
            KeyAction::BreakToWindow => "break_to_window",
            KeyAction::SwapIntoSlot => "swap_into_slot",
            KeyAction::InputFocus => "input_focus",
            KeyAction::ReleaseInput => "release_input",
            KeyAction::CloseEmbed => "close_embed",
            KeyAction::Collapse => "collapse",
            KeyAction::EmbedSearch => "search",
        }
    }

    /// Look up an action by its `keys` config name. `None` for anything
    /// unrecognized - the caller reports it as a validation problem and
    /// moves on (see [`KeyRegistry::build`]).
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|a| a.name() == name)
    }

    /// The hardcoded default binding - what every action resolves to with
    /// no `keys` config at all, and what a remap falls back to on any
    /// validation failure.
    pub fn default_spec(self) -> KeySpec {
        match self {
            KeyAction::Quit => KeySpec::bare(KeyCode::Char('q')),
            KeyAction::Help => KeySpec::bare(KeyCode::Char('?')),
            KeyAction::MoveDown => KeySpec::bare(KeyCode::Char('j')),
            KeyAction::MoveUp => KeySpec::bare(KeyCode::Char('k')),
            KeyAction::SectionNext => KeySpec::bare(KeyCode::Tab),
            KeyAction::SectionPrev => KeySpec::bare(KeyCode::BackTab),
            KeyAction::ActivityNext => KeySpec::bare(KeyCode::Tab),
            KeyAction::ActivityPrev => KeySpec::bare(KeyCode::BackTab),
            KeyAction::Filter => KeySpec::bare(KeyCode::Char('/')),
            KeyAction::SpawnPicker => KeySpec::bare(KeyCode::Char('n')),
            KeyAction::Kill => KeySpec::bare(KeyCode::Char('x')),
            KeyAction::Relaunch => KeySpec::bare(KeyCode::Char('s')),
            KeyAction::AskJump => KeySpec::bare(KeyCode::Char('a')),
            KeyAction::AskApprove => KeySpec::bare(KeyCode::Char('y')),
            KeyAction::AskDeny => KeySpec::bare(KeyCode::Char('d')),
            KeyAction::ShowAll => KeySpec::bare(KeyCode::Char('A')),
            KeyAction::GotoPicker => KeySpec::bare(KeyCode::Char('g')),
            KeyAction::Adopt => KeySpec::bare(KeyCode::Char('G')),
            KeyAction::Reconcile => KeySpec::bare(KeyCode::Char('r')),
            KeyAction::Profiling => KeySpec::bare(KeyCode::Char('p')),
            KeyAction::OpenView => KeySpec::bare(KeyCode::Enter),
            KeyAction::Focus => KeySpec::bare(KeyCode::Char('f')),
            KeyAction::SplitRight => KeySpec::bare(KeyCode::Char('|')),
            KeyAction::SplitBelow => KeySpec::bare(KeyCode::Char('-')),
            KeyAction::BreakToWindow => KeySpec::bare(KeyCode::Char('b')),
            KeyAction::SwapIntoSlot => KeySpec::bare(KeyCode::Char('w')),
            KeyAction::InputFocus => KeySpec::bare(KeyCode::Char('i')),
            KeyAction::ReleaseInput => KeySpec {
                code: KeyCode::Char('o'),
                modifiers: KeyModifiers::CONTROL,
            },
            KeyAction::CloseEmbed => KeySpec::bare(KeyCode::Char('q')),
            KeyAction::Collapse => KeySpec::bare(KeyCode::Char('z')),
            KeyAction::EmbedSearch => KeySpec::bare(KeyCode::Char('/')),
        }
    }
}

/// Which dispatcher consults an action - both the conflict-detection unit
/// (two actions in the same scope must not resolve to the same key) and
/// the grouping [`KeyRegistry::action_for`] searches at runtime.
///
/// [`KeyAction::ReleaseInput`] is a [`Scope::Embed`] member for validation
/// purposes only: its real dispatch site (`handle_embed_key`'s
/// input-focus-held branch) is checked in isolation, before anything else
/// in that scope ever runs, so it can never actually collide at runtime.
/// Grouping it with [`Scope::Embed`] anyway means a config that
/// accidentally reuses another embed action's key for `release_input`
/// (e.g. mapping it to `input_focus`'s `i`) is still caught and reverted
/// like any other conflict, rather than silently shadowing the escape
/// hatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    /// `handle_normal_key`: sidebar-only, no embed open.
    Normal,
    /// `handle_embed_key`: an embed is open, seat input focus is not held,
    /// and scrollback search is not active.
    Embed,
    /// Durable Plot activity stage, whether or not a Session embed is live.
    Stage,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Scope::Normal => "normal",
            Scope::Embed => "embed",
            Scope::Stage => "stage",
        }
    }

    fn actions(self) -> &'static [KeyAction] {
        match self {
            Scope::Normal => NORMAL_ACTIONS,
            Scope::Embed => EMBED_ACTIONS,
            Scope::Stage => STAGE_ACTIONS,
        }
    }
}

/// [`Scope::Normal`]'s members, in `handle_normal_key`'s own match order.
const NORMAL_ACTIONS: &[KeyAction] = &[
    KeyAction::Quit,
    KeyAction::Help,
    KeyAction::MoveDown,
    KeyAction::MoveUp,
    KeyAction::SectionNext,
    KeyAction::SectionPrev,
    KeyAction::Filter,
    KeyAction::SpawnPicker,
    KeyAction::Kill,
    KeyAction::Relaunch,
    KeyAction::AskJump,
    KeyAction::ShowAll,
    KeyAction::GotoPicker,
    KeyAction::Adopt,
    KeyAction::Reconcile,
    KeyAction::Profiling,
    KeyAction::AskApprove,
    KeyAction::AskDeny,
    KeyAction::OpenView,
    KeyAction::Focus,
    KeyAction::SplitRight,
    KeyAction::SplitBelow,
    KeyAction::BreakToWindow,
    KeyAction::SwapIntoSlot,
];

/// [`Scope::Embed`]'s members, in `handle_embed_key`'s own match order
/// (plus [`KeyAction::ReleaseInput`] - see the doc on [`Scope`]).
const EMBED_ACTIONS: &[KeyAction] = &[
    KeyAction::Help,
    KeyAction::OpenView,
    KeyAction::InputFocus,
    KeyAction::CloseEmbed,
    KeyAction::Focus,
    KeyAction::SplitRight,
    KeyAction::SplitBelow,
    KeyAction::SwapIntoSlot,
    KeyAction::BreakToWindow,
    KeyAction::MoveDown,
    KeyAction::MoveUp,
    KeyAction::SectionNext,
    KeyAction::SectionPrev,
    KeyAction::Collapse,
    KeyAction::EmbedSearch,
    KeyAction::ReleaseInput,
];

const STAGE_ACTIONS: &[KeyAction] = &[
    KeyAction::Help,
    KeyAction::OpenView,
    KeyAction::InputFocus,
    KeyAction::CloseEmbed,
    KeyAction::Focus,
    KeyAction::SplitRight,
    KeyAction::SplitBelow,
    KeyAction::SwapIntoSlot,
    KeyAction::BreakToWindow,
    KeyAction::MoveDown,
    KeyAction::MoveUp,
    KeyAction::ActivityNext,
    KeyAction::ActivityPrev,
    KeyAction::Collapse,
    KeyAction::EmbedSearch,
    KeyAction::ReleaseInput,
];

/// One parsed key binding: a `KeyCode` plus the modifiers that must be
/// present. Two specs compare equal (for conflict detection) only when
/// both fields match exactly; [`Self::matches`] (runtime dispatch against
/// a live [`KeyEvent`]) is deliberately looser - see its doc.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeySpec {
    pub code: KeyCode,
    pub modifiers: KeyModifiers,
}

impl KeySpec {
    fn bare(code: KeyCode) -> Self {
        Self {
            code,
            modifiers: KeyModifiers::NONE,
        }
    }

    /// True when `key` triggers this binding. A bare spec (no modifiers)
    /// matches on `code` alone, ignoring whatever modifier bits crossterm
    /// attaches - the same behavior every hardcoded `KeyCode::Char(_)` arm
    /// this replaces already had (none of them checked `key.modifiers`). A
    /// `ctrl-`-prefixed spec requires the `CONTROL` bit to be present via
    /// `contains`, not exact equality, matching the pre-registry `Ctrl-o`/
    /// `Ctrl-c` checks this table supersedes.
    pub fn matches(&self, key: &KeyEvent) -> bool {
        if self.code != key.code {
            return false;
        }
        if self.modifiers.contains(KeyModifiers::CONTROL) {
            key.modifiers.contains(KeyModifiers::CONTROL)
        } else {
            true
        }
    }

    /// Human-readable label for the help overlay and status hints.
    pub fn label(&self) -> String {
        if self.modifiers.contains(KeyModifiers::CONTROL)
            && let KeyCode::Char(c) = self.code
        {
            return format!("Ctrl-{c}");
        }
        match self.code {
            KeyCode::Tab => "tab".to_owned(),
            KeyCode::BackTab => "shift+tab".to_owned(),
            KeyCode::Enter => "enter".to_owned(),
            KeyCode::Esc => "esc".to_owned(),
            KeyCode::Char(' ') => "space".to_owned(),
            KeyCode::Char(c) => c.to_string(),
            other => format!("{other:?}"),
        }
    }
}

/// Parse one `keys` config value into a [`KeySpec`]. Accepted forms: a
/// single visible character (`g`, `G`, `|`, `?`, case preserved - `A` and
/// `a` are different bindings, matching the pre-registry hardcoded arms),
/// a named key (`tab`, `backtab`, `enter`, `esc`, `space`, case-
/// insensitive), or `ctrl-<char>` (the character is lowercased, matching
/// how crossterm reports a control chord - `Ctrl-O` and `ctrl-o` parse the
/// same). Anything else is a parse error; the caller keeps the action's
/// default and reports the string (see [`KeyRegistry::build`]).
pub fn parse_key_spec(raw: &str) -> Result<KeySpec, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err("empty key spec".to_owned());
    }
    let lower = trimmed.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("ctrl-") {
        let mut chars = rest.chars();
        return match (chars.next(), chars.next()) {
            (Some(c), None) => Ok(KeySpec {
                code: KeyCode::Char(c),
                modifiers: KeyModifiers::CONTROL,
            }),
            _ => Err(format!("invalid ctrl chord {trimmed:?}")),
        };
    }
    match lower.as_str() {
        "tab" => return Ok(KeySpec::bare(KeyCode::Tab)),
        "backtab" => return Ok(KeySpec::bare(KeyCode::BackTab)),
        "enter" => return Ok(KeySpec::bare(KeyCode::Enter)),
        "esc" | "escape" => return Ok(KeySpec::bare(KeyCode::Esc)),
        "space" => return Ok(KeySpec::bare(KeyCode::Char(' '))),
        _ => {}
    }
    let mut chars = trimmed.chars();
    match (chars.next(), chars.next()) {
        (Some(c), None) => Ok(KeySpec::bare(KeyCode::Char(c))),
        _ => Err(format!("unrecognized key spec {trimmed:?}")),
    }
}

/// The resolved action->key table, built once at startup
/// ([`KeyRegistry::build`]) from the field config's `keys` section. An
/// empty/absent config produces [`KeyRegistry::defaults`] - every action at
/// its hardcoded default - so dispatch is byte-identical to a build with no
/// registry at all.
#[derive(Debug, Clone, Default)]
pub struct KeyRegistry {
    overrides: HashMap<KeyAction, KeySpec>,
}

impl KeyRegistry {
    /// The all-default registry: no `keys` config, or none of it survived
    /// validation.
    pub fn defaults() -> Self {
        Self::default()
    }

    /// Parse and validate a `keys` config section into a registry plus a
    /// fail-soft problem report. Never fails outright: every problem here
    /// means "this one action kept its default," never "the field
    /// refuses to start." See the module doc for the full validation
    /// rules.
    pub fn build(raw: &BTreeMap<String, String>) -> (Self, Vec<String>) {
        let mut overrides = HashMap::new();
        let mut problems = Vec::new();
        for (name, spec_str) in raw {
            match KeyAction::from_name(name) {
                None => problems.push(format!("keys: unknown action {name:?}; ignored")),
                Some(action) => match parse_key_spec(spec_str) {
                    Ok(spec) => {
                        overrides.insert(action, spec);
                    }
                    Err(err) => problems.push(format!(
                        "keys: {name}: {err}; kept default {}",
                        action.default_spec().label()
                    )),
                },
            }
        }
        let mut registry = Self { overrides };
        for scope in [Scope::Normal, Scope::Embed, Scope::Stage] {
            registry.resolve_scope_conflicts(scope, &mut problems);
        }
        (registry, problems)
    }

    /// Detect and revert same-key collisions within one scope. Iterates
    /// the scope's fixed action order (not a hash-map order) so problem
    /// messages are deterministic across runs, which the unit tests below
    /// rely on.
    fn resolve_scope_conflicts(&mut self, scope: Scope, problems: &mut Vec<String>) {
        let mut groups: Vec<(KeySpec, Vec<KeyAction>)> = Vec::new();
        for &action in scope.actions() {
            let spec = self.effective(action);
            match groups.iter_mut().find(|(s, _)| *s == spec) {
                Some((_, actions)) => actions.push(action),
                None => groups.push((spec, vec![action])),
            }
        }
        for (spec, actions) in groups {
            if actions.len() < 2 {
                continue;
            }
            for &action in &actions {
                self.overrides.remove(&action);
            }
            let names: Vec<&str> = actions.iter().map(|a| a.name()).collect();
            problems.push(format!(
                "keys: {} scope: {} all map to {}; kept defaults",
                scope.label(),
                names.join("/"),
                spec.label(),
            ));
        }
    }

    /// The key this action currently resolves to - its override if one
    /// survived validation, its hardcoded default otherwise.
    pub fn effective(&self, action: KeyAction) -> KeySpec {
        self.overrides
            .get(&action)
            .copied()
            .unwrap_or_else(|| action.default_spec())
    }

    /// The human-readable label `ui.rs` renders for this action's
    /// effective key.
    pub fn label(&self, action: KeyAction) -> String {
        self.effective(action).label()
    }

    /// Resolve a pressed key to the action it triggers within `scope`, if
    /// any - the single lookup both `handle_normal_key` and
    /// `handle_embed_key` consult instead of scattering `KeyCode` matches
    /// across their arms.
    pub fn action_for(&self, scope: Scope, key: &KeyEvent) -> Option<KeyAction> {
        scope
            .actions()
            .iter()
            .copied()
            .find(|&action| self.effective(action).matches(key))
    }
}

/// Collapse a validation problem list into one status-line-sized message:
/// the first problem verbatim, plus a count of the rest if there were more
/// than one. `None` for an empty list (nothing to report).
pub fn summarize_problems(problems: &[String]) -> Option<String> {
    match problems {
        [] => None,
        [only] => Some(only.clone()),
        [first, rest @ ..] => Some(format!(
            "{first} (+{} more keybinding issue{})",
            rest.len(),
            if rest.len() == 1 { "" } else { "s" }
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEventKind, KeyEventState};

    fn key(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn plain(code: KeyCode) -> KeyEvent {
        key(code, KeyModifiers::NONE)
    }

    fn raw(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    // --- key-spec parsing ---

    #[test]
    fn parses_single_visible_chars_preserving_case() {
        assert_eq!(
            parse_key_spec("g").unwrap(),
            KeySpec::bare(KeyCode::Char('g'))
        );
        assert_eq!(
            parse_key_spec("G").unwrap(),
            KeySpec::bare(KeyCode::Char('G'))
        );
        assert_eq!(
            parse_key_spec("|").unwrap(),
            KeySpec::bare(KeyCode::Char('|'))
        );
    }

    #[test]
    fn parses_named_keys_case_insensitively() {
        assert_eq!(parse_key_spec("Tab").unwrap(), KeySpec::bare(KeyCode::Tab));
        assert_eq!(
            parse_key_spec("BACKTAB").unwrap(),
            KeySpec::bare(KeyCode::BackTab)
        );
        assert_eq!(
            parse_key_spec("Enter").unwrap(),
            KeySpec::bare(KeyCode::Enter)
        );
        assert_eq!(parse_key_spec("Esc").unwrap(), KeySpec::bare(KeyCode::Esc));
        assert_eq!(
            parse_key_spec("space").unwrap(),
            KeySpec::bare(KeyCode::Char(' '))
        );
    }

    #[test]
    fn parses_ctrl_chords_lowercasing_the_char() {
        assert_eq!(
            parse_key_spec("ctrl-o").unwrap(),
            KeySpec {
                code: KeyCode::Char('o'),
                modifiers: KeyModifiers::CONTROL,
            }
        );
        assert_eq!(
            parse_key_spec("Ctrl-O").unwrap(),
            KeySpec {
                code: KeyCode::Char('o'),
                modifiers: KeyModifiers::CONTROL,
            }
        );
    }

    #[test]
    fn rejects_empty_and_multi_char_and_bad_ctrl_specs() {
        assert!(parse_key_spec("").is_err());
        assert!(parse_key_spec("   ").is_err());
        assert!(parse_key_spec("gg").is_err());
        assert!(parse_key_spec("ctrl-").is_err());
        assert!(parse_key_spec("ctrl-oo").is_err());
    }

    // --- KeyRegistry::build: defaults, overrides, unknown/unparseable ---

    #[test]
    fn no_config_yields_pure_defaults() {
        let (registry, problems) = KeyRegistry::build(&BTreeMap::new());
        assert!(problems.is_empty());
        for &action in KeyAction::ALL {
            assert_eq!(registry.effective(action), action.default_spec());
        }
    }

    #[test]
    fn valid_override_takes_effect_with_no_problems() {
        let (registry, problems) = KeyRegistry::build(&raw(&[("goto_picker", "o")]));
        assert!(problems.is_empty());
        assert_eq!(
            registry.effective(KeyAction::GotoPicker),
            KeySpec::bare(KeyCode::Char('o'))
        );
        // Untouched actions stay default.
        assert_eq!(
            registry.effective(KeyAction::Quit),
            KeyAction::Quit.default_spec()
        );
    }

    #[test]
    fn unknown_action_name_is_reported_and_ignored() {
        let (registry, problems) = KeyRegistry::build(&raw(&[("teleport", "t")]));
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("unknown action"));
        assert!(problems[0].contains("teleport"));
        // Nothing else was disturbed.
        for &action in KeyAction::ALL {
            assert_eq!(registry.effective(action), action.default_spec());
        }
    }

    #[test]
    fn unparseable_spec_is_reported_and_keeps_the_default() {
        let (registry, problems) = KeyRegistry::build(&raw(&[("goto_picker", "ctrl-oo")]));
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("goto_picker"));
        assert_eq!(
            registry.effective(KeyAction::GotoPicker),
            KeyAction::GotoPicker.default_spec()
        );
    }

    // --- conflict detection, per scope ---

    #[test]
    fn same_scope_conflict_reverts_both_actions_to_default() {
        // `kill` (default x) remapped onto `relaunch`'s default (s)
        // collides with `relaunch` itself - both stay Normal-scope
        // members, so both must revert.
        let (registry, problems) = KeyRegistry::build(&raw(&[("kill", "s")]));
        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("normal scope"));
        assert_eq!(
            registry.effective(KeyAction::Kill),
            KeyAction::Kill.default_spec()
        );
        assert_eq!(
            registry.effective(KeyAction::Relaunch),
            KeyAction::Relaunch.default_spec()
        );
    }

    #[test]
    fn cross_scope_same_key_is_not_a_conflict() {
        // `/` already legitimately backs `filter` (normal) and `search`
        // (embed) by default - different scopes, no problem reported.
        let (registry, problems) = KeyRegistry::build(&BTreeMap::new());
        assert!(problems.is_empty());
        assert_eq!(
            registry.effective(KeyAction::Filter),
            KeySpec::bare(KeyCode::Char('/'))
        );
        assert_eq!(
            registry.effective(KeyAction::EmbedSearch),
            KeySpec::bare(KeyCode::Char('/'))
        );
    }

    #[test]
    fn defaults_have_no_within_scope_conflicts() {
        // Regression guard: if a future action's default ever collides
        // with another one in the same scope, `build` would silently
        // revert both on every single launch with no config at all.
        for scope in [Scope::Normal, Scope::Embed, Scope::Stage] {
            let mut seen: Vec<KeySpec> = Vec::new();
            for &action in scope.actions() {
                let spec = action.default_spec();
                assert!(
                    !seen.contains(&spec),
                    "default conflict in {scope:?} scope on {}",
                    spec.label()
                );
                seen.push(spec);
            }
        }
    }

    // --- release_input's restoration guarantee ---

    #[test]
    fn release_input_defaults_to_ctrl_o() {
        let (registry, _) = KeyRegistry::build(&BTreeMap::new());
        assert_eq!(
            registry.effective(KeyAction::ReleaseInput),
            KeyAction::ReleaseInput.default_spec()
        );
    }

    #[test]
    fn release_input_falls_back_to_default_on_unparseable_spec() {
        let (registry, problems) = KeyRegistry::build(&raw(&[("release_input", "not-a-key")]));
        assert_eq!(problems.len(), 1);
        assert_eq!(
            registry.effective(KeyAction::ReleaseInput),
            KeyAction::ReleaseInput.default_spec()
        );
    }

    #[test]
    fn release_input_falls_back_to_default_on_conflict() {
        // Remapping it onto `input_focus`'s default (`i`) collides inside
        // Scope::Embed - both revert.
        let (registry, problems) = KeyRegistry::build(&raw(&[("release_input", "i")]));
        assert_eq!(problems.len(), 1);
        assert_eq!(
            registry.effective(KeyAction::ReleaseInput),
            KeyAction::ReleaseInput.default_spec()
        );
        assert_eq!(
            registry.effective(KeyAction::InputFocus),
            KeyAction::InputFocus.default_spec()
        );
    }

    #[test]
    fn release_input_honors_a_clean_remap() {
        // A remap that parses and does not collide with anything in its
        // scope is a deliberate operator choice and is honored - the
        // guarantee is "always resolves to something," not "always ctrl-o".
        let (registry, problems) = KeyRegistry::build(&raw(&[("release_input", "ctrl-x")]));
        assert!(problems.is_empty());
        assert_eq!(
            registry.effective(KeyAction::ReleaseInput),
            KeySpec {
                code: KeyCode::Char('x'),
                modifiers: KeyModifiers::CONTROL,
            }
        );
    }

    // --- action_for / matches ---

    #[test]
    fn action_for_resolves_remapped_and_default_keys() {
        let (registry, _) = KeyRegistry::build(&raw(&[("goto_picker", "o")]));
        assert_eq!(
            registry.action_for(Scope::Normal, &plain(KeyCode::Char('o'))),
            Some(KeyAction::GotoPicker)
        );
        // The old default key no longer resolves to anything.
        assert_eq!(
            registry.action_for(Scope::Normal, &plain(KeyCode::Char('g'))),
            None
        );
        // An untouched action's default still resolves.
        assert_eq!(
            registry.action_for(Scope::Normal, &plain(KeyCode::Char('q'))),
            Some(KeyAction::Quit)
        );
    }

    #[test]
    fn action_for_respects_scope() {
        let (registry, _) = KeyRegistry::build(&BTreeMap::new());
        // `collapse` (z) only exists in Scope::Embed.
        assert_eq!(
            registry.action_for(Scope::Normal, &plain(KeyCode::Char('z'))),
            None
        );
        assert_eq!(
            registry.action_for(Scope::Embed, &plain(KeyCode::Char('z'))),
            Some(KeyAction::Collapse)
        );
    }

    #[test]
    fn stage_tabs_cycle_activity_without_changing_normal_section_bindings() {
        let registry = KeyRegistry::defaults();
        let tab = plain(KeyCode::Tab);
        let backtab = key(KeyCode::BackTab, KeyModifiers::SHIFT);

        assert_eq!(
            registry.action_for(Scope::Normal, &tab),
            Some(KeyAction::SectionNext)
        );
        assert_eq!(
            registry.action_for(Scope::Normal, &backtab),
            Some(KeyAction::SectionPrev)
        );
        assert_eq!(
            registry.action_for(Scope::Stage, &tab),
            Some(KeyAction::ActivityNext)
        );
        assert_eq!(
            registry.action_for(Scope::Stage, &backtab),
            Some(KeyAction::ActivityPrev)
        );
    }

    #[test]
    fn ctrl_spec_matches_ignore_incidental_extra_modifier_bits() {
        let spec = KeySpec {
            code: KeyCode::Char('o'),
            modifiers: KeyModifiers::CONTROL,
        };
        assert!(spec.matches(&key(
            KeyCode::Char('o'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT
        )));
        assert!(!spec.matches(&plain(KeyCode::Char('o'))));
    }

    // --- labels ---

    #[test]
    fn label_renders_ctrl_chords_named_keys_and_bare_chars() {
        assert_eq!(
            KeySpec {
                code: KeyCode::Char('o'),
                modifiers: KeyModifiers::CONTROL,
            }
            .label(),
            "Ctrl-o"
        );
        assert_eq!(KeySpec::bare(KeyCode::Tab).label(), "tab");
        assert_eq!(KeySpec::bare(KeyCode::BackTab).label(), "shift+tab");
        assert_eq!(KeySpec::bare(KeyCode::Char('|')).label(), "|");
    }

    // --- summarize_problems ---

    #[test]
    fn summarize_problems_collapses_the_list() {
        assert_eq!(summarize_problems(&[]), None);
        assert_eq!(
            summarize_problems(&["only".to_owned()]),
            Some("only".to_owned())
        );
        assert_eq!(
            summarize_problems(&["first".to_owned(), "second".to_owned()]),
            Some("first (+1 more keybinding issue)".to_owned())
        );
        assert_eq!(
            summarize_problems(&["first".to_owned(), "second".to_owned(), "third".to_owned()]),
            Some("first (+2 more keybinding issues)".to_owned())
        );
    }
}
