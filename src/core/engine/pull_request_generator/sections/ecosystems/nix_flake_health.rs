//! The two one-liners at the end of the flake summary: whether the release notes of the
//! updated inputs mention security, and how healthy the lock is. They are shown even when
//! there is nothing to report, so that their absence can't be mistaken for not having looked.

use std::collections::BTreeSet;
use std::sync::LazyLock;

use chrono::{DateTime, Duration, Utc};
use regex::Regex;

use super::nix_flake::{code, plural, same_repo};
use crate::core::clients::github::GitHub;
use crate::core::engine::ecosystems::nix_flake::internal::helpers::{self, UpstreamComparison};
use crate::core::engine::ecosystems::nix_flake::internal::lock_diff::{ChangeKind, LockDiff};
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

/// How many CVE ids are spelled out.
const CVES_SHOWN: usize = 5;

static CVE_ID: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bCVE-\d{4}-\d{4,}\b").expect("valid pattern"));
static SECURITY_WORD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bsecurity\b").expect("valid pattern"));

/// What was found in the release notes of the updated inputs.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Security {
    pub cves: BTreeSet<String>,
    pub mentions_security: bool,
    /// How many inputs had release notes to read.
    pub inputs_scanned: usize,
}

/// Looks through the release notes of each input (an empty one has none) for CVE ids and the
/// word "security". A plain text search over what was fetched anyway: it finds what the notes
/// say, not what is wrong with the code.
pub fn scan(notes_per_input: &[Vec<String>]) -> Security {
    let mut found = Security::default();

    for notes in notes_per_input.iter().filter(|notes| !notes.is_empty()) {
        found.inputs_scanned += 1;
        for text in notes {
            found.cves.extend(
                CVE_ID
                    .find_iter(text)
                    .map(|cve| cve.as_str().to_uppercase()),
            );
            found.mentions_security |= SECURITY_WORD.is_match(text);
        }
    }

    found
}

fn security_line(security: &Security) -> String {
    let outcome = if !security.cves.is_empty() {
        let mut shown: Vec<String> = security
            .cves
            .iter()
            .take(CVES_SHOWN)
            .map(|cve| code(cve))
            .collect();
        if security.cves.len() > CVES_SHOWN {
            shown.push(format!("{} more", security.cves.len() - CVES_SHOWN));
        }
        format!("release notes mention {}.", shown.join(", "))
    } else if security.mentions_security {
        "release notes mention security, but no CVE ids.".to_string()
    } else if security.inputs_scanned > 0 {
        format!(
            "the release notes of {} don't mention CVEs or security.",
            plural(security.inputs_scanned, "input", "inputs")
        )
    } else {
        "no release notes to read.".to_string()
    };

    format!("- **Security:** {outcome} Not a vulnerability scan.\n")
}

/// What is wrong with an input compared to its upstream.
#[derive(Debug, PartialEq, Eq)]
pub enum Stale {
    /// Upstream has had `commits` more commits for `days` days.
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

/// The release notes of the direct inputs the update moved, one list per input.
async fn release_notes(github: Option<&GitHub>, diff: &LockDiff) -> Vec<Vec<String>> {
    let Some(github) = github else {
        return Vec::new();
    };

    futures::future::join_all(diff.direct.iter().map(|change| async move {
        let ChangeKind::Updated { old, new } = &change.kind else {
            return None;
        };
        let (owner, repo) = same_repo(old, new)?;
        helpers::get_release_notes_between(
            github,
            owner,
            repo,
            old.last_modified?,
            new.last_modified?,
        )
        .await
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

/// The first line of the lock health: what is actionable, and `OK` when nothing is. What the
/// update changed is marked, where the lock before it could be compared.
fn health_line(
    health: &LockHealth,
    previous: Option<&LockHealth>,
    stale: &[(String, Stale)],
) -> String {
    let mut parts: Vec<String> = Vec::new();

    let followable: Vec<String> = health
        .followable()
        .map(|path| match previous {
            Some(previous) if !previous.followable().any(|before| before == path) => {
                format!("{} (new)", code(path))
            }
            _ => code(path),
        })
        .collect();
    if !followable.is_empty() {
        parts.push(format!(
            "{} a `follows` could remove ({})",
            plural(followable.len(), "duplicate", "duplicates"),
            some_of(&followable)
        ));
    }

    if !health.not_following.is_empty() {
        let names: Vec<String> = health
            .not_following
            .iter()
            .map(|name| match previous {
                Some(previous) if !previous.not_following.contains(name) => {
                    format!("{} (new)", code(name))
                }
                _ => code(name),
            })
            .collect();
        parts.push(format!(
            "{} not following the `nixpkgs` of the flake ({})",
            plural(names.len(), "input", "inputs"),
            some_of(&names)
        ));
    }

    let behind: Vec<String> = stale
        .iter()
        .filter_map(|(name, stale)| match stale {
            Stale::Behind { commits, days } => Some(format!(
                "{} {}, the oldest {} days old",
                code(name),
                plural(*commits as usize, "commit", "commits"),
                days
            )),
            Stale::Dormant { .. } => None,
        })
        .collect();
    if !behind.is_empty() {
        parts.push(format!(
            "{} behind upstream ({})",
            plural(behind.len(), "input", "inputs"),
            some_of(&behind)
        ));
    }

    let dormant: Vec<String> = stale
        .iter()
        .filter_map(|(name, stale)| match stale {
            Stale::Dormant { days } => Some(format!(
                "{} {}",
                code(name),
                plural((*days / 30) as usize, "month", "months")
            )),
            Stale::Behind { .. } => None,
        })
        .collect();
    if !dormant.is_empty() {
        parts.push(format!(
            "{} at a dormant upstream ({})",
            plural(dormant.len(), "input", "inputs"),
            some_of(&dormant)
        ));
    }

    if parts.is_empty() {
        "- **Lock health:** OK\n".to_string()
    } else {
        format!("- **Lock health:** {}.\n", parts.join("; "))
    }
}

/// Every repository that is locked more than once, for whoever wants to look closer.
fn duplicates_details(health: &LockHealth, previous: Option<&LockHealth>) -> String {
    if health.duplicates.is_empty() {
        return String::new();
    }

    let mut out = String::from(
        "<details>\n<summary>Repositories locked at more than one revision</summary>\n\n",
    );

    for duplicate in &health.duplicates {
        let paths: Vec<String> = duplicate.paths.iter().map(|path| code(path)).collect();
        out.push_str(&format!(
            "- {}: {} ({})",
            code(&duplicate.repo),
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

    out.push_str("\n</details>\n\n");
    out
}

/// The one-liners, and what can be expanded below them.
pub struct Footer {
    pub lines: String,
    pub details: String,
}

impl Footer {
    fn new(
        health: &LockHealth,
        previous: Option<&LockHealth>,
        stale: &[(String, Stale)],
        security: &Security,
    ) -> Self {
        Self {
            lines: format!(
                "{}{}\n",
                security_line(security),
                health_line(health, previous, stale)
            ),
            details: duplicates_details(health, previous),
        }
    }

    /// The footer with the details if they leave `limit` bytes sufficient, else without them.
    /// Empty if not even the lines fit.
    pub fn within(&self, limit: usize) -> String {
        let full = format!("{}{}", self.lines, self.details);
        [full, self.lines.clone()]
            .into_iter()
            .find(|footer| footer.len() <= limit)
            .unwrap_or_default()
    }
}

/// Looks at the lock after the update, and at what GitHub says about the inputs. `None` if the
/// lock can't be read.
pub async fn footer(
    ctx: &PullRequestGenerationContext<'_>,
    old: &str,
    new: &str,
    diff: &LockDiff,
) -> Option<Footer> {
    let health = LockHealth::of(new)?;
    let previous = LockHealth::of(old);
    let tracked = tracked_inputs(new).unwrap_or_default();

    let (stale, notes) = futures::future::join(
        stale_inputs(ctx.github, &tracked),
        release_notes(ctx.github, diff),
    )
    .await;

    Some(Footer::new(
        &health,
        previous.as_ref(),
        &stale,
        &scan(&notes),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notes(texts: &[&str]) -> Vec<String> {
        texts.iter().map(|text| text.to_string()).collect()
    }

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
    fn scan_finds_cve_ids_in_any_case_once() {
        let found = scan(&[
            notes(&["Fixes CVE-2025-1234 and cve-2025-1234, also CVE-2024-98765."]),
            notes(&["CVE-2025-1234 again"]),
        ]);

        assert_eq!(
            found.cves.iter().map(String::as_str).collect::<Vec<_>>(),
            ["CVE-2024-98765", "CVE-2025-1234"]
        );
        assert_eq!(found.inputs_scanned, 2);
    }

    #[test]
    fn scan_ignores_things_that_only_look_like_cve_ids() {
        let found = scan(&[notes(&[
            "CVE-25-1, CVE-2025-12, XCVE-2025-1234, CVE-2025-1234x",
        ])]);

        assert!(found.cves.is_empty(), "{found:?}");
    }

    #[test]
    fn scan_matches_the_word_security_only() {
        assert!(scan(&[notes(&["A Security fix."])]).mentions_security);
        assert!(!scan(&[notes(&["insecurity, securityfoo"])]).mentions_security);
    }

    #[test]
    fn scan_counts_only_inputs_that_have_notes() {
        let found = scan(&[notes(&["a"]), Vec::new(), notes(&["b", "c"])]);

        assert_eq!(found.inputs_scanned, 2);
    }

    #[test]
    fn the_security_line_never_calls_anything_safe_or_vulnerable() {
        let cases = [
            Security::default(),
            scan(&[notes(&["nothing to see"])]),
            scan(&[notes(&["security release"])]),
            scan(&[notes(&["CVE-2025-1234"])]),
        ];

        for case in cases {
            let line = security_line(&case);
            assert!(line.ends_with("Not a vulnerability scan.\n"), "{line}");
            assert!(
                !line.contains("safe") && !line.contains("vulnerable"),
                "{line}"
            );
        }
    }

    #[test]
    fn the_security_line_says_what_was_found() {
        assert_eq!(
            security_line(&Security::default()),
            "- **Security:** no release notes to read. Not a vulnerability scan.\n"
        );
        assert_eq!(
            security_line(&scan(&[notes(&["nothing"]), notes(&["still nothing"])])),
            "- **Security:** the release notes of 2 inputs don't mention CVEs or security. Not a vulnerability scan.\n"
        );
        assert_eq!(
            security_line(&scan(&[notes(&["fixes CVE-2025-1234"])])),
            "- **Security:** release notes mention `CVE-2025-1234`. Not a vulnerability scan.\n"
        );
    }

    #[test]
    fn many_cves_are_cut_to_a_few() {
        let text: Vec<String> = (1000..1010).map(|n| format!("CVE-2025-{n}")).collect();
        let line = security_line(&scan(&[vec![text.join(" ")]]));

        assert!(line.contains("`CVE-2025-1004`, 5 more."), "{line}");
        assert!(!line.contains("CVE-2025-1005"), "{line}");
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

    #[test]
    fn a_lock_without_problems_reads_ok() {
        let clean = health(
            &github_node("nixpkgs", "NixOS", "nixpkgs", 'a', ""),
            r#""nixpkgs": "nixpkgs""#,
        );

        assert_eq!(
            health_line(&clean, Some(&clean), &[]),
            "- **Lock health:** OK\n"
        );
        assert_eq!(duplicates_details(&clean, Some(&clean)), "");
    }

    #[test]
    fn the_health_line_lists_what_can_be_fixed() {
        let health = lock_with_duplicates();

        assert_eq!(
            health_line(&health, None, &[]),
            "- **Lock health:** 1 duplicate a `follows` could remove (`tool/systems`); \
             1 input not following the `nixpkgs` of the flake (`tool`).\n"
        );
    }

    #[test]
    fn the_health_line_marks_what_the_update_brought() {
        let after = lock_with_duplicates();
        let before = health(
            &github_node("nixpkgs", "NixOS", "nixpkgs", 'a', ""),
            r#""nixpkgs": "nixpkgs""#,
        );
        let line = health_line(&after, Some(&before), &[]);

        assert!(line.contains("(`tool/systems` (new))"), "{line}");
        assert!(line.contains("(`tool` (new))"), "{line}");
        assert!(!health_line(&after, Some(&after), &[]).contains("(new)"));
    }

    #[test]
    fn the_health_line_reports_stale_inputs_apart() {
        let clean = health(
            &github_node("nixpkgs", "NixOS", "nixpkgs", 'a', ""),
            r#""nixpkgs": "nixpkgs""#,
        );
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
            health_line(&clean, None, &stale),
            "- **Lock health:** 1 input behind upstream (`old-tool` 1 commit, the oldest 40 days old); \
             2 inputs at a dormant upstream (`quiet` 7 months, `quieter` 13 months).\n"
        );
    }

    #[test]
    fn the_details_say_how_the_update_changed_the_duplicates() {
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
        let details = duplicates_details(&after, Some(&before));

        assert!(details.starts_with("<details>\n<summary>"), "{details}");
        assert!(details.ends_with("</details>\n\n"), "{details}");
        assert!(
            details.contains("- `NixOS/nixpkgs`: 2 revisions (`nixpkgs`, `tool/nixpkgs`)\n"),
            "{details}"
        );
        assert!(
            details.contains(
                "- `nix-systems/default`: 2 revisions (`systems`, `tool/systems`) (+1 since before)\n"
            ),
            "{details}"
        );
    }

    #[test]
    fn the_footer_drops_the_details_before_the_lines() {
        let footer = Footer::new(&lock_with_duplicates(), None, &[], &Security::default());
        let full = footer.within(usize::MAX);

        assert!(full.ends_with("</details>\n\n"), "{full}");
        assert_eq!(footer.within(full.len() - 1), footer.lines);
        assert_eq!(footer.within(footer.lines.len() - 1), "");
        assert!(footer.lines.ends_with("\n\n"));
    }
}
