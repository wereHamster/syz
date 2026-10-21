//! The tools in the dev shells of a flake, and which of them an update changes.
//!
//! A tool is an input of the shell (`nativeBuildInputs`, `buildInputs` and the propagated ones,
//! which is where `packages` and `inputsFrom` end up as well), known by the name and version of
//! its derivation. The shells are evaluated together with the other outputs of the flake, see
//! [`super::affected_outputs`]; this only compares what that yields.

use std::collections::BTreeMap;

/// The tools of one dev shell: name to version, which is empty for a tool without one.
pub type Tools = BTreeMap<String, String>;

/// The tools of each dev shell, or `None` for a shell that could not be evaluated.
pub type Shells = BTreeMap<String, Option<Tools>>;

/// A tool whose presence or version differs, and the dev shells it is in. `before` is `None`
/// for a tool the update adds, `after` for one it removes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolChange {
    pub name: String,
    pub before: Option<String>,
    pub after: Option<String>,
    pub shells: Vec<String>,
}

/// What an update does to the tools of the dev shells.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DevShellChanges {
    /// The dev shells that were evaluated before and after the update, `default` first.
    pub shells: Vec<String>,
    /// The tools that differ, the ones that are in the most shells first.
    pub changes: Vec<ToolChange>,
    /// How many dev shells could not be evaluated, and are left out of the above.
    pub unevaluated: usize,
}

/// `default` goes first, since it is the one that is meant when no shell is named.
fn shell_order(a: &String, b: &String) -> std::cmp::Ordering {
    (a != "default", a).cmp(&(b != "default", b))
}

/// How the tools of the dev shells differ between `old` and `new`.
///
/// A shell that only one of them has counts as having no tools in the other.
pub fn compare(old: &Shells, new: &Shells) -> DevShellChanges {
    let mut names: Vec<&String> = old.keys().chain(new.keys()).collect();
    names.sort_by(|a, b| shell_order(a, b));
    names.dedup();

    let mut result = DevShellChanges::default();
    let mut by_change: BTreeMap<(String, Option<String>, Option<String>), Vec<String>> =
        BTreeMap::new();
    let none = Tools::new();

    for name in names {
        let (before, after) = match (old.get(name), new.get(name)) {
            (Some(None), _) | (_, Some(None)) => {
                result.unevaluated += 1;
                continue;
            }
            (before, after) => (
                before.and_then(Option::as_ref).unwrap_or(&none),
                after.and_then(Option::as_ref).unwrap_or(&none),
            ),
        };
        result.shells.push(name.clone());

        for tool in before.keys().chain(after.keys()) {
            let (b, a) = (before.get(tool), after.get(tool));
            if b != a {
                by_change
                    .entry((tool.clone(), b.cloned(), a.cloned()))
                    .or_default()
                    .push(name.clone());
            }
        }
    }

    result.changes = by_change
        .into_iter()
        .map(|((name, before, after), mut shells)| {
            shells.dedup();
            ToolChange {
                name,
                before,
                after,
                shells,
            }
        })
        .collect();
    result.changes.sort_by(|a, b| {
        b.shells
            .len()
            .cmp(&a.shells.len())
            .then(a.name.cmp(&b.name))
    });

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shell(tools: &[(&str, &str)]) -> Option<Tools> {
        Some(
            tools
                .iter()
                .map(|(name, version)| (name.to_string(), version.to_string()))
                .collect(),
        )
    }

    fn shells(shells: Vec<(&str, Option<Tools>)>) -> Shells {
        shells
            .into_iter()
            .map(|(name, tools)| (name.to_string(), tools))
            .collect()
    }

    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn a_tool_is_reported_once_with_all_the_shells_it_changes_in() {
        let old = shells(vec![
            ("agent", shell(&[("cargo", "1.0"), ("biome", "2.0")])),
            ("default", shell(&[("cargo", "1.0")])),
            ("workflow", shell(&[("cargo", "1.0")])),
        ]);
        let new = shells(vec![
            ("agent", shell(&[("cargo", "1.1"), ("biome", "2.1")])),
            ("default", shell(&[("cargo", "1.1")])),
            ("workflow", shell(&[("cargo", "1.1")])),
        ]);

        let result = compare(&old, &new);

        assert_eq!(result.shells, strings(&["default", "agent", "workflow"]));
        assert_eq!(result.changes.len(), 2);
        assert_eq!(result.changes[0].name, "cargo");
        assert_eq!(result.changes[0].before.as_deref(), Some("1.0"));
        assert_eq!(result.changes[0].after.as_deref(), Some("1.1"));
        assert_eq!(
            result.changes[0].shells,
            strings(&["default", "agent", "workflow"])
        );
        assert_eq!(result.changes[1].name, "biome");
        assert_eq!(result.changes[1].shells, strings(&["agent"]));
    }

    #[test]
    fn tools_that_stay_the_same_are_not_reported() {
        let both = shells(vec![(
            "default",
            shell(&[("cargo", "1.0"), ("turso", "0.2")]),
        )]);

        let result = compare(&both, &both);

        assert!(result.changes.is_empty());
        assert_eq!(result.shells, strings(&["default"]));
    }

    #[test]
    fn added_and_removed_tools_have_one_side_missing() {
        let old = shells(vec![("default", shell(&[("jq", "1.7")]))]);
        let new = shells(vec![("default", shell(&[("biome", "2.1")]))]);

        let result = compare(&old, &new);

        assert_eq!(result.changes.len(), 2);
        assert_eq!(result.changes[0].name, "biome");
        assert_eq!(result.changes[0].before, None);
        assert_eq!(result.changes[0].after.as_deref(), Some("2.1"));
        assert_eq!(result.changes[1].name, "jq");
        assert_eq!(result.changes[1].before.as_deref(), Some("1.7"));
        assert_eq!(result.changes[1].after, None);
    }

    #[test]
    fn most_widespread_tools_come_first_then_by_name() {
        let old = shells(vec![
            ("a", shell(&[("zsh", "1"), ("curl", "1"), ("awk", "1")])),
            ("b", shell(&[("zsh", "1"), ("curl", "1")])),
            ("c", shell(&[("zsh", "1")])),
        ]);
        let new = shells(vec![
            ("a", shell(&[("zsh", "2"), ("curl", "2"), ("awk", "2")])),
            ("b", shell(&[("zsh", "2"), ("curl", "2")])),
            ("c", shell(&[("zsh", "2")])),
        ]);

        let result = compare(&old, &new);

        let order: Vec<&str> = result.changes.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(order, ["zsh", "curl", "awk"]);
    }

    #[test]
    fn a_shell_that_cannot_be_evaluated_is_counted_and_left_out() {
        let old = shells(vec![
            ("default", shell(&[("cargo", "1.0")])),
            ("agent", None),
        ]);
        let new = shells(vec![
            ("default", shell(&[("cargo", "1.1")])),
            ("agent", shell(&[("cargo", "1.1")])),
        ]);

        let result = compare(&old, &new);

        assert_eq!(result.unevaluated, 1);
        assert_eq!(result.shells, strings(&["default"]));
        assert_eq!(result.changes[0].shells, strings(&["default"]));
    }

    #[test]
    fn a_new_shell_brings_all_its_tools_as_new() {
        let old = shells(vec![("default", shell(&[("cargo", "1.0")]))]);
        let new = shells(vec![
            ("default", shell(&[("cargo", "1.0")])),
            ("agent", shell(&[("cargo", "1.0"), ("biome", "2.1")])),
        ]);

        let result = compare(&old, &new);

        assert_eq!(result.shells, strings(&["default", "agent"]));
        assert_eq!(result.changes.len(), 2);
        assert_eq!(result.changes[0].name, "biome");
        assert_eq!(result.changes[0].before, None);
        assert_eq!(result.changes[1].name, "cargo");
        assert_eq!(result.changes[1].shells, strings(&["agent"]));
    }
}
