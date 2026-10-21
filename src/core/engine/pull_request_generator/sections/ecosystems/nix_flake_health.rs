//! The lock health section of the flake summary: what is wrong with the lock that someone could
//! act on, or `OK`. It is shown even when there is nothing to report, so that its absence can't
//! be mistaken for not having looked.

use chrono::{DateTime, Duration, Utc};

use super::nix_flake::code;
use super::nix_flake::plural;
use crate::core::clients::github::GitHub;
use crate::core::engine::ecosystems::nix_flake::internal::helpers::{self, UpstreamComparison};
use crate::core::engine::ecosystems::nix_flake::internal::lock_health::{
    tracked_inputs, LockHealth, TrackedInput,
};
use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;

/// A newer commit upstream that the lock has missed for this long means that something keeps
/// the input from being updated. Sooner than that, an update is probably on its way.
const BEHIND_AFTER: Duration = Duration::days(7);

/// An upstream that has not moved for this long is a risk to keep an eye on.
const DORMANT_AFTER: Duration = Duration::days(180);

/// How many items of a list are spelled out.
const ITEMS_SHOWN: usize = 3;

/// What is wrong with an input compared to its upstream.
#[derive(Debug, PartialEq, Eq)]
pub enum Stale {
    /// Upstream has had `commits` more commits, the oldest for `days` days.
    Behind { commits: u64, days: i64 },
    /// The lock is at the newest commit, and that is `days` old.
    Dormant { days: i64 },
}

/// `None` when the input is up to date with an upstream that is alive.
fn classify(
    input: &TrackedInput,
    comparison: UpstreamComparison,
    now: DateTime<Utc>,
) -> Option<Stale> {
    match comparison {
        UpstreamComparison::Identical => {
            let committed = DateTime::from_timestamp(input.last_modified?, 0)?;
            let age = now - committed;
            (age >= DORMANT_AFTER).then(|| Stale::Dormant {
                days: age.num_days(),
            })
        }
        UpstreamComparison::Ahead { commits, since } => {
            let age = now - since;
            (age >= BEHIND_AFTER).then(|| Stale::Behind {
                commits,
                days: age.num_days(),
            })
        }
        UpstreamComparison::Unrelated => None,
    }
}

/// The inputs that are behind a live upstream or at a dormant one, by name. Inputs GitHub
/// can't tell about are left out.
async fn stale_inputs(github: Option<&GitHub>, inputs: &[TrackedInput]) -> Vec<(String, Stale)> {
    let Some(github) = github else {
        return Vec::new();
    };
    let now = Utc::now();

    futures::future::join_all(inputs.iter().map(|input| async move {
        let comparison = helpers::compare_with_upstream(
            github,
            &input.owner,
            &input.repo,
            &input.rev,
            input.git_ref.as_deref(),
        )
        .await?;
        Some((input.name.clone(), classify(input, comparison, now)?))
    }))
    .await
    .into_iter()
    .flatten()
    .collect()
}

/// The names in `items`, the first few of them.
fn some_of(items: &[String]) -> String {
    let mut shown: Vec<String> = items.iter().take(ITEMS_SHOWN).cloned().collect();
    if items.len() > ITEMS_SHOWN {
        shown.push(format!("{} more", items.len() - ITEMS_SHOWN));
    }
    shown.join(", ")
}

/// `name`, marked when the update brought it.
fn marked(name: &str, is_new: bool) -> String {
    if is_new {
        format!("{} (new)", code(name))
    } else {
        code(name)
    }
}

/// What is actionable, one finding to an item. Empty when nothing is. What the update brought
/// is marked, where the lock before it could be compared.
fn findings(
    health: &LockHealth,
    previous: Option<&LockHealth>,
    stale: &[(String, Stale)],
) -> Vec<String> {
    let mut items: Vec<String> = Vec::new();

    let followable: Vec<String> = health
        .followable()
        .map(|path| {
            let is_new = previous.is_some_and(|previous| !previous.followable().any(|p| p == path));
            marked(path, is_new)
        })
        .collect();
    if !followable.is_empty() {
        items.push(format!(
            "{} a `follows` could remove: {}",
            plural(followable.len(), "duplicate", "duplicates"),
            some_of(&followable)
        ));
    }

    let not_following: Vec<String> = health
        .not_following
        .iter()
        .map(|name| {
            let is_new = previous.is_some_and(|previous| !previous.not_following.contains(name));
            marked(name, is_new)
        })
        .collect();
    if !not_following.is_empty() {
        items.push(format!(
            "{} not following the `nixpkgs` of the flake: {}",
            plural(not_following.len(), "input", "inputs"),
            some_of(&not_following)
        ));
    }

    let behind: Vec<String> = stale
        .iter()
        .filter_map(|(name, stale)| match stale {
            Stale::Behind { commits, days } => Some(format!(
                "{} ({}, the oldest {} days old)",
                code(name),
                plural(*commits as usize, "commit", "commits"),
                days
            )),
            Stale::Dormant { .. } => None,
        })
        .collect();
    if !behind.is_empty() {
        items.push(format!(
            "{} behind upstream: {}",
            plural(behind.len(), "input", "inputs"),
            some_of(&behind)
        ));
    }

    let dormant: Vec<String> = stale
        .iter()
        .filter_map(|(name, stale)| match stale {
            Stale::Dormant { days } => Some(format!(
                "{} ({})",
                code(name),
                plural((*days / 30) as usize, "month", "months")
            )),
            Stale::Behind { .. } => None,
        })
        .collect();
    if !dormant.is_empty() {
        items.push(format!(
            "{} at a dormant upstream: {}",
            plural(dormant.len(), "input", "inputs"),
            some_of(&dormant)
        ));
    }

    items
}

/// Every repository that is locked more than once, as a paragraph and a list.
fn duplicates_list(health: &LockHealth, previous: Option<&LockHealth>) -> String {
    if health.duplicates.is_empty() {
        return String::new();
    }

    let mut out = String::from("Repositories locked at more than one revision:\n\n");

    for duplicate in &health.duplicates {
        let paths: Vec<String> = duplicate.paths.iter().map(|path| code(path)).collect();
        out.push_str(&format!(
            "- {}: {} ({})",
            // Only github repositories are grouped, and their names are checked in the lock.
            format!("[{0}](https://github.com/{0})", duplicate.repo),
            plural(duplicate.revisions, "revision", "revisions"),
            some_of(&paths)
        ));

        if let Some(previous) = previous {
            let before = previous
                .duplicates
                .iter()
                .find(|before| before.repo == duplicate.repo)
                .map_or(1, |before| before.revisions);
            if before != duplicate.revisions {
                out.push_str(&format!(
                    " ({}{} since before)",
                    if duplicate.revisions > before {
                        "+"
                    } else {
                        "−"
                    },
                    duplicate.revisions.abs_diff(before)
                ));
            }
        }
        out.push('\n');
    }

    out.push('\n');
    out
}

/// The lock health section, and the list of duplicates that goes below its findings.
pub struct HealthSection {
    pub body: String,
    pub duplicates: String,
}

impl HealthSection {
    fn new(health: &LockHealth, previous: Option<&LockHealth>, stale: &[(String, Stale)]) -> Self {
        let items = findings(health, previous, stale);
        let body = if items.is_empty() {
            "**Lock health**\n\nOK\n\n".to_string()
        } else {
            let list: String = items.iter().map(|item| format!("- {item}\n")).collect();
            format!("**Lock health**\n\n{list}\n")
        };

        Self {
            body,
            duplicates: duplicates_list(health, previous),
        }
    }

    /// The section with the list of duplicates if it fits in `limit` bytes, else without it. Empty if
    /// not even the findings fit.
    pub fn within(&self, limit: usize) -> String {
        let full = format!("{}{}", self.body, self.duplicates);
        [full, self.body.clone()]
            .into_iter()
            .find(|section| section.len() <= limit)
            .unwrap_or_default()
    }
}

/// Looks at the lock after the update, and at what GitHub says about the inputs. `None` if the
/// lock can't be read.
pub async fn section(
    ctx: &PullRequestGenerationContext<'_>,
    old: &str,
    new: &str,
) -> Option<HealthSection> {
    let health = LockHealth::of(new)?;
    let previous = LockHealth::of(old);
    let tracked = tracked_inputs(new).unwrap_or_default();
    let stale = stale_inputs(ctx.github, &tracked).await;

    Some(HealthSection::new(&health, previous.as_ref(), &stale))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(last_modified: Option<i64>) -> TrackedInput {
        TrackedInput {
            name: "tool".to_string(),
            owner: "acme".to_string(),
            repo: "tool".to_string(),
            git_ref: None,
            rev: "a".repeat(40),
            last_modified,
        }
    }

    fn at(date: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(&format!("{date}T00:00:00Z"))
            .unwrap()
            .with_timezone(&Utc)
    }

    #[test]
    fn an_input_at_the_newest_commit_is_dormant_when_that_commit_is_old() {
        let committed = at("2026-03-01").timestamp();

        assert_eq!(
            classify(
                &input(Some(committed)),
                UpstreamComparison::Identical,
                at("2026-10-01")
            ),
            Some(Stale::Dormant { days: 214 })
        );
        assert_eq!(
            classify(
                &input(Some(committed)),
                UpstreamComparison::Identical,
                at("2026-04-01")
            ),
            None
        );
        assert_eq!(
            classify(
                &input(None),
                UpstreamComparison::Identical,
                at("2026-10-01")
            ),
            None
        );
    }

    #[test]
    fn an_input_is_behind_when_upstream_moved_on_a_while_ago() {
        let comparison = |since| UpstreamComparison::Ahead { commits: 4, since };

        assert_eq!(
            classify(&input(None), comparison(at("2026-09-01")), at("2026-10-01")),
            Some(Stale::Behind {
                commits: 4,
                days: 30
            })
        );
        assert_eq!(
            classify(&input(None), comparison(at("2026-09-30")), at("2026-10-01")),
            None
        );
        assert_eq!(
            classify(
                &input(None),
                UpstreamComparison::Unrelated,
                at("2026-10-01")
            ),
            None
        );
    }

    fn health(nodes: &str, root_inputs: &str) -> LockHealth {
        LockHealth::of(&format!(
            r#"{{ "nodes": {{ {nodes}, "root": {{ "inputs": {{ {root_inputs} }} }} }}, "root": "root" }}"#
        ))
        .unwrap()
    }

    fn github_node(id: &str, owner: &str, repo: &str, revision: char, inputs: &str) -> String {
        format!(
            r#""{id}": {{ "locked": {{ "type": "github", "owner": "{owner}", "repo": "{repo}", "rev": "{}" }}, "inputs": {{ {inputs} }} }}"#,
            revision.to_string().repeat(40)
        )
    }

    /// A flake whose `tool` brings a `nixpkgs` and a `systems` of its own.
    fn lock_with_duplicates() -> LockHealth {
        health(
            &[
                github_node("nixpkgs", "NixOS", "nixpkgs", 'a', ""),
                github_node("nixpkgs_2", "NixOS", "nixpkgs", 'b', ""),
                github_node("systems", "nix-systems", "default", 'c', ""),
                github_node("systems_2", "nix-systems", "default", 'd', ""),
                github_node(
                    "tool",
                    "acme",
                    "tool",
                    'e',
                    r#""nixpkgs": "nixpkgs_2", "systems": "systems_2""#,
                ),
            ]
            .join(", "),
            r#""nixpkgs": "nixpkgs", "systems": "systems", "tool": "tool""#,
        )
    }

    fn clean_lock() -> LockHealth {
        health(
            &github_node("nixpkgs", "NixOS", "nixpkgs", 'a', ""),
            r#""nixpkgs": "nixpkgs""#,
        )
    }

    #[test]
    fn a_lock_without_problems_reads_ok() {
        let clean = clean_lock();
        let section = HealthSection::new(&clean, Some(&clean), &[]);

        assert_eq!(section.body, "**Lock health**\n\nOK\n\n");
        assert_eq!(section.duplicates, "");
    }

    #[test]
    fn lists_what_can_be_fixed() {
        let section = HealthSection::new(&lock_with_duplicates(), None, &[]);

        assert_eq!(
            section.body,
            "**Lock health**\n\n\
             - 1 duplicate a `follows` could remove: `tool/systems`\n\
             - 1 input not following the `nixpkgs` of the flake: `tool`\n\n"
        );
    }

    #[test]
    fn marks_what_the_update_brought() {
        let after = lock_with_duplicates();
        let marked = HealthSection::new(&after, Some(&clean_lock()), &[]).body;

        assert!(marked.contains(": `tool/systems` (new)\n"), "{marked}");
        assert!(marked.contains(": `tool` (new)\n"), "{marked}");
        assert!(!HealthSection::new(&after, Some(&after), &[])
            .body
            .contains("(new)"));
    }

    #[test]
    fn reports_stale_inputs_apart() {
        let stale = vec![
            (
                "old-tool".to_string(),
                Stale::Behind {
                    commits: 1,
                    days: 40,
                },
            ),
            ("quiet".to_string(), Stale::Dormant { days: 215 }),
            ("quieter".to_string(), Stale::Dormant { days: 400 }),
        ];

        assert_eq!(
            HealthSection::new(&clean_lock(), None, &stale).body,
            "**Lock health**\n\n\
             - 1 input behind upstream: `old-tool` (1 commit, the oldest 40 days old)\n\
             - 2 inputs at a dormant upstream: `quiet` (7 months), `quieter` (13 months)\n\n"
        );
    }

    #[test]
    fn cuts_long_lists_to_a_few() {
        let stale: Vec<(String, Stale)> = (0..5)
            .map(|n| (format!("quiet-{n}"), Stale::Dormant { days: 200 }))
            .collect();
        let body = HealthSection::new(&clean_lock(), None, &stale).body;

        assert!(body.contains("5 inputs at a dormant upstream: "), "{body}");
        assert!(body.contains("`quiet-2` (6 months), 2 more\n"), "{body}");
    }

    #[test]
    fn the_list_says_how_the_update_changed_the_duplicates() {
        let after = lock_with_duplicates();
        let before = health(
            &[
                github_node("nixpkgs", "NixOS", "nixpkgs", 'a', ""),
                github_node("nixpkgs_2", "NixOS", "nixpkgs", 'b', ""),
                github_node("tool", "acme", "tool", 'e', r#""nixpkgs": "nixpkgs_2""#),
            ]
            .join(", "),
            r#""nixpkgs": "nixpkgs", "tool": "tool""#,
        );
        let list = duplicates_list(&after, Some(&before));

        assert!(
            list.starts_with("Repositories locked at more than one revision:\n\n- "),
            "{list}"
        );
        assert!(list.ends_with(")\n\n"), "{list}");
        assert!(
            list.contains("- [NixOS/nixpkgs](https://github.com/NixOS/nixpkgs): 2 revisions (`nixpkgs`, `tool/nixpkgs`)\n"),
            "{list}"
        );
        assert!(
            list.contains(
                "- [nix-systems/default](https://github.com/nix-systems/default): 2 revisions (`systems`, `tool/systems`) (+1 since before)\n"
            ),
            "{list}"
        );
    }

    #[test]
    fn the_section_drops_the_duplicates_before_the_findings() {
        let section = HealthSection::new(&lock_with_duplicates(), None, &[]);
        let full = section.within(usize::MAX);

        assert!(full.ends_with(")\n\n"), "{full}");
        assert_eq!(section.within(full.len() - 1), section.body);
        assert_eq!(section.within(section.body.len() - 1), "");
    }
}
