//! Session naming and collision disambiguation.
//!
//! Pure: the caller injects `session_exists`/`known_path` probes
//! (registry- and tmux-backed in the real field) so this stays
//! testable without a tmux server.

/// Resolve the tmux session name to spawn or attach for `path`.
///
/// - Base name: `path`'s basename with `.`/`:` replaced by `_` (tmux
///   session names may not contain these).
/// - No session named that: use the base name.
/// - A session by that name whose recorded path (`known_path`) matches
///   `path`: reuse it (attach case).
/// - Otherwise: disambiguate with `<base>@<parent-dir-basename>`,
///   applying the same two checks, then fall back to a numeric
///   `-2`, `-3`, ... suffix until an unused or matching name is found.
///
/// Paths are compared with trailing `/` trimmed.
pub fn resolve_session_name(
    path: &str,
    session_exists: impl Fn(&str) -> bool,
    known_path: impl Fn(&str) -> Option<String>,
) -> String {
    let base = base_name(path);
    if let Some(name) = try_name(&base, path, &session_exists, &known_path) {
        return name;
    }

    let parent = parent_basename(path);
    let with_parent = if parent.is_empty() {
        base.clone()
    } else {
        format!("{base}@{parent}")
    };
    if let Some(name) = try_name(&with_parent, path, &session_exists, &known_path) {
        return name;
    }

    // Bounded defensively: a real field will never see thousands of
    // same-basename, same-parent collisions, but a pure fn should not
    // be able to loop forever on an adversarial probe pair.
    for n in 2..10_000 {
        let candidate = format!("{with_parent}-{n}");
        if let Some(name) = try_name(&candidate, path, &session_exists, &known_path) {
            return name;
        }
    }
    format!("{with_parent}-9999")
}

/// `Some(name)` when `name` is free to use for `path`: either no
/// session by that name exists, or one does and its known path matches
/// (so using it is a reuse/attach, not a rename).
fn try_name(
    name: &str,
    path: &str,
    session_exists: &impl Fn(&str) -> bool,
    known_path: &impl Fn(&str) -> Option<String>,
) -> Option<String> {
    if !session_exists(name) {
        return Some(name.to_owned());
    }
    if known_path(name).as_deref().map(trim_slash) == Some(trim_slash(path)) {
        return Some(name.to_owned());
    }
    None
}

fn trim_slash(path: &str) -> &str {
    path.trim_end_matches('/')
}

fn base_name(path: &str) -> String {
    let trimmed = trim_slash(path);
    let name = trimmed.rsplit('/').next().unwrap_or(trimmed);
    name.chars()
        .map(|c| if c == '.' || c == ':' { '_' } else { c })
        .collect()
}

/// Basename of `path`'s parent directory, or empty when `path` has no
/// parent component.
fn parent_basename(path: &str) -> String {
    let trimmed = trim_slash(path);
    let mut parts = trimmed.rsplit('/');
    let _name = parts.next();
    parts
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or_default()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn no_sessions(_: &str) -> bool {
        false
    }

    fn no_known(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn uses_base_name_when_no_session_exists() {
        let name = resolve_session_name("/a/teotl/nopal-task-38-x", no_sessions, no_known);
        assert_eq!(name, "nopal-task-38-x");
    }

    #[test]
    fn replaces_dots_and_colons_in_base_name() {
        let name = resolve_session_name("/a/my.repo:thing", no_sessions, no_known);
        assert_eq!(name, "my_repo_thing");
    }

    #[test]
    fn reuses_name_when_known_path_matches() {
        let path = "/a/teotl/nopal-task-38-x";
        let name = resolve_session_name(
            path,
            |n| n == "nopal-task-38-x",
            |n| (n == "nopal-task-38-x").then(|| path.to_owned()),
        );
        assert_eq!(name, "nopal-task-38-x");
    }

    #[test]
    fn reuses_name_when_known_path_matches_modulo_trailing_slash() {
        let name = resolve_session_name(
            "/a/teotl/nopal-task-38-x/",
            |n| n == "nopal-task-38-x",
            |n| (n == "nopal-task-38-x").then(|| "/a/teotl/nopal-task-38-x".to_owned()),
        );
        assert_eq!(name, "nopal-task-38-x");
    }

    #[test]
    fn disambiguates_with_parent_dir_on_collision_with_different_path() {
        let name = resolve_session_name(
            "/other/nopal-task-38-x",
            |n| n == "nopal-task-38-x",
            |n| (n == "nopal-task-38-x").then(|| "/a/teotl/nopal-task-38-x".to_owned()),
        );
        assert_eq!(name, "nopal-task-38-x@other");
    }

    #[test]
    fn reuses_the_at_parent_name_when_its_known_path_matches() {
        let path = "/other/nopal-task-38-x";
        let name = resolve_session_name(
            path,
            |n| n == "nopal-task-38-x" || n == "nopal-task-38-x@other",
            move |n| match n {
                "nopal-task-38-x" => Some("/a/teotl/nopal-task-38-x".to_owned()),
                "nopal-task-38-x@other" => Some(path.to_owned()),
                _ => None,
            },
        );
        assert_eq!(name, "nopal-task-38-x@other");
    }

    #[test]
    fn falls_back_to_numeric_suffix_when_parent_name_also_collides() {
        let name = resolve_session_name(
            "/other/nopal-task-38-x",
            |n| n == "nopal-task-38-x" || n == "nopal-task-38-x@other",
            |n| {
                if n == "nopal-task-38-x" || n == "nopal-task-38-x@other" {
                    Some("/somewhere/else".to_owned())
                } else {
                    None
                }
            },
        );
        assert_eq!(name, "nopal-task-38-x@other-2");
    }

    #[test]
    fn numeric_suffix_climbs_past_multiple_collisions() {
        let taken = [
            "nopal-task-38-x",
            "nopal-task-38-x@other",
            "nopal-task-38-x@other-2",
            "nopal-task-38-x@other-3",
        ];
        let name = resolve_session_name(
            "/other/nopal-task-38-x",
            |n| taken.contains(&n),
            |n| taken.contains(&n).then(|| "/somewhere/else".to_owned()),
        );
        assert_eq!(name, "nopal-task-38-x@other-4");
    }

    #[test]
    fn no_parent_dir_falls_back_to_numeric_suffix_on_base() {
        // A bare basename with no parent component (e.g. a root-level
        // path) - `with_parent` degrades to the base name itself, so a
        // collision goes straight to numeric suffixing.
        let name = resolve_session_name(
            "nopal-task-38-x",
            |n| n == "nopal-task-38-x",
            |n| (n == "nopal-task-38-x").then(|| "/elsewhere".to_owned()),
        );
        assert_eq!(name, "nopal-task-38-x-2");
    }
}
