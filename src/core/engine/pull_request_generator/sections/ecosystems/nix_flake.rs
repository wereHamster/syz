use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::ecosystems::nix_flake::internal::helpers;
use crate::core::engine::ecosystems::nix_flake::internal::lock_diff::{
    collapse, ChangeGroup, ChangeKind, InputChange, LockDiff, Pin,
};
use crate::core::engine::pull_request_generator::budget::{fit_items, SectionKind};
use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;
use crate::core::engine::pull_request_generator::sections::{summary, PullRequestSectionGenerator};
use crate::core::engine::repository::{FileModification, FileState, ProjectRepositorySnapshot};

/// None of the other sections produce anything for flake inputs yet.
pub fn sections() -> Vec<Box<dyn PullRequestSectionGenerator>> {
    vec![Box::new(NixFlakeSummarySection)]
}

/// How many places a node is used by are spelled out.
const PLACES_SHOWN: usize = 3;

/// Summarizes what the update did to `flake.lock`, rather than just what it was asked to do:
/// updating one input can move others, and their inputs.
pub struct NixFlakeSummarySection;

#[async_trait]
impl PullRequestSectionGenerator for NixFlakeSummarySection {
    fn name(&self) -> &'static str {
        "nix-flake summary"
    }

    fn kind(&self) -> SectionKind {
        SectionKind::Summary
    }

    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        budget: usize,
    ) -> Result<Option<String>> {
        let Some(diff) = lock_diff(ctx.snapshot, ctx.modifications).await else {
            return summary::generate(ctx, budget, |_, _, _| None).await;
        };

        let commits = commit_counts(ctx, &diff.direct).await;
        Ok(render(&diff, &commits, budget))
    }
}

/// How the lock differs before and after the update. `None` when the update leaves the lock
/// alone, or there is no lock to compare.
async fn lock_diff(
    snapshot: &dyn ProjectRepositorySnapshot,
    modifications: &[FileModification],
) -> Option<LockDiff> {
    let new = modifications
        .iter()
        .find_map(|modification| match &modification.state {
            FileState::Write(content) if modification.path == "flake.lock" => Some(content),
            _ => None,
        })?;
    let old = snapshot.read_file("flake.lock").await.ok()?;

    LockDiff::between(&old, new).filter(|diff| !diff.is_empty())
}

/// How many commits each of the updated `changes` spans, where GitHub can tell.
async fn commit_counts(
    ctx: &PullRequestGenerationContext<'_>,
    changes: &[InputChange],
) -> Vec<Option<u64>> {
    let Some(github) = ctx.github else {
        return vec![None; changes.len()];
    };

    futures::future::join_all(changes.iter().map(|change| async move {
        let ChangeKind::Updated { old, new } = &change.kind else {
            return None;
        };
        let (owner, repo) = same_repo(old, new)?;
        helpers::get_commit_count(github, owner, repo, old.rev()?, new.rev()?).await
    }))
    .await
}

/// The repository two pins share, if they are both github pins of the same one.
fn same_repo<'a>(old: &'a Pin, new: &Pin) -> Option<(&'a str, &'a str)> {
    old.github_repo()
        .filter(|repo| Some(*repo) == new.github_repo())
}

/// Text that is safe between backticks.
fn code(text: &str) -> String {
    format!("`{}`", text.replace(['`', '\n', '\r'], "'"))
}

fn plural(count: usize, one: &str, many: &str) -> String {
    format!("{} {}", count, if count == 1 { one } else { many })
}

/// `[owner/repo](https://github.com/owner/repo)` for a github pin.
fn repo_link(pin: &Pin) -> Option<String> {
    let (owner, repo) = pin.github_repo()?;
    Some(format!(
        "[{owner}/{repo}](https://github.com/{owner}/{repo})"
    ))
}

/// `old → new`, linked to the comparison of the two where there is one, and followed by when
/// they were made.
fn movement(old: &Pin, new: &Pin) -> String {
    let text = format!("{} → {}", code(&old.describe()), code(&new.describe()));

    let mut out = match (same_repo(old, new), old.rev(), new.rev()) {
        (Some((owner, repo)), Some(old_rev), Some(new_rev)) => {
            format!("[{text}](https://github.com/{owner}/{repo}/compare/{old_rev}...{new_rev})")
        }
        _ => text,
    };

    match (old.date(), new.date()) {
        (Some(from), Some(to)) if from == to => out.push_str(&format!(" ({from})")),
        (Some(from), Some(to)) => out.push_str(&format!(" ({from} → {to})")),
        _ => {}
    }

    out
}

/// One bullet: what happened to a node (`~` moved, `+` new, `-` gone), shown the same way for
/// direct and indirect inputs. The marker is code so that it is one character wide everywhere.
fn entry(names: &[String], kind: &ChangeKind, commits: Option<u64>, used_by: &[String]) -> String {
    let (marker, repo, detail) = match kind {
        ChangeKind::Updated { old, new } => ("~", repo_link(new), movement(old, new)),
        ChangeKind::Added(pin) => ("+", repo_link(pin), code(&pin.describe())),
        ChangeKind::Removed(pin) => (
            "-",
            repo_link(pin),
            format!("was {}", code(&pin.describe())),
        ),
    };

    let mut row = format!("- `{marker}` {}", names.join(", "));
    if let Some(repo) = repo {
        row.push_str(&format!(" ({repo})"));
    }
    row.push_str(&format!(": {detail}"));
    if let Some(commits) = commits {
        row.push_str(&format!(
            ", {}",
            plural(commits as usize, "commit", "commits")
        ));
    }

    if !used_by.is_empty() {
        let mut places: Vec<String> = used_by.iter().take(PLACES_SHOWN).map(|p| code(p)).collect();
        if used_by.len() > PLACES_SHOWN {
            places.push(format!("{} more", used_by.len() - PLACES_SHOWN));
        }
        row.push_str(&format!("; used by {}", places.join(", ")));
    }

    row.push('\n');
    row
}

/// One row of the direct inputs.
fn direct_row(change: &InputChange, commits: Option<u64>) -> String {
    entry(&[code(&change.path)], &change.kind, commits, &[])
}

/// One entry of the indirect inputs: a node, or several that moved the same way. It is named
/// after the input, and says where that input is used, since the same node is usually reached
/// from several places.
fn indirect_entry(group: &ChangeGroup) -> String {
    let mut names: Vec<String> = Vec::new();
    let mut used_by: Vec<String> = Vec::new();

    for path in group.paths.iter().chain(&group.also_via) {
        let (parent, name) = path.rsplit_once('/').unwrap_or(("", path));
        let name = code(name);
        if group.paths.contains(path) && !names.contains(&name) {
            names.push(name);
        }
        if !parent.is_empty() && !used_by.iter().any(|p| p == parent) {
            used_by.push(parent.to_string());
        }
    }

    entry(&names, &group.kind, None, &used_by)
}

/// The summary of `diff` no longer than `budget`. `commits` has the commit count of each of
/// the direct changes, where known.
///
/// The direct inputs come first. When there are too many entries, the last ones are dropped,
/// and the count of the indirect inputs stays.
fn render(diff: &LockDiff, commits: &[Option<u64>], budget: usize) -> Option<String> {
    let omitted = |n: usize| format!("- … and {} more\n", n);

    let direct_budget = if diff.indirect.is_empty() {
        budget
    } else {
        budget * 2 / 3
    };
    let direct = if diff.direct.is_empty() {
        "**Direct inputs**\n\nNo direct inputs changed.\n\n".to_string()
    } else {
        let rows: Vec<String> = diff
            .direct
            .iter()
            .enumerate()
            .map(|(i, change)| direct_row(change, commits.get(i).copied().flatten()))
            .collect();
        fit_items("**Direct inputs**\n\n", &rows, "\n", direct_budget, omitted)?
    };

    let count = |is: fn(&ChangeKind) -> bool| diff.indirect.iter().filter(|c| is(&c.kind)).count();
    let updated = count(|kind| matches!(kind, ChangeKind::Updated { .. }));
    let added = count(|kind| matches!(kind, ChangeKind::Added(_)));
    let removed = count(|kind| matches!(kind, ChangeKind::Removed(_)));

    let indirect = if diff.indirect.is_empty() {
        "**Indirect inputs**\n\nNo indirect inputs changed.\n\n".to_string()
    } else {
        let summary = format!(
            "{} changed, {} added, {} removed.",
            plural(updated, "indirect input", "indirect inputs"),
            added,
            removed
        );
        let entries: Vec<String> = collapse(&diff.indirect)
            .iter()
            .map(indirect_entry)
            .collect();
        let remaining = budget.saturating_sub(direct.len());

        fit_items(
            &format!("**Indirect inputs**\n\n{summary}\n\n"),
            &entries,
            "\n",
            remaining,
            omitted,
        )
        .or_else(|| {
            Some(format!("**Indirect inputs**\n\n{summary}\n\n")).filter(|s| s.len() <= remaining)
        })
        .unwrap_or_default()
    };

    Some(direct + &indirect)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::repository::test_support::MockSnapshot;

    const REV_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const REV_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const REV_C: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const REV_D: &str = "dddddddddddddddddddddddddddddddddddddddd";

    fn github(owner: &str, repo: &str, rev: &str, modified: i64) -> String {
        format!(
            r#"{{ "type": "github", "owner": "{owner}", "repo": "{repo}", "rev": "{rev}", "narHash": "sha256-{rev}", "lastModified": {modified} }}"#
        )
    }

    fn node(locked: String, inputs: &str) -> String {
        format!(r#"{{ "locked": {locked}, "inputs": {{ {inputs} }} }}"#)
    }

    /// A lock with a `nixpkgs` and a `llm-agents` that has `systems` and `treefmt-nix` of its
    /// own, pinned to the given revisions.
    fn lock(nixpkgs: &str, systems: &str, treefmt: Option<&str>) -> String {
        let treefmt_node = match treefmt {
            Some(rev) => format!(
                r#""treefmt-nix": {},"#,
                node(github("numtide", "treefmt-nix", rev, 1_760_000_000), "")
            ),
            None => String::new(),
        };
        let treefmt_input = if treefmt.is_some() {
            r#", "treefmt-nix": "treefmt-nix""#
        } else {
            ""
        };

        format!(
            r#"{{ "nodes": {{
                "nixpkgs": {nixpkgs_node},
                "llm-agents": {llm_agents_node},
                "systems": {systems_node},
                {treefmt_node}
                "root": {{ "inputs": {{ "nixpkgs": "nixpkgs", "llm-agents": "llm-agents" }} }}
            }}, "root": "root" }}"#,
            nixpkgs_node = node(github("NixOS", "nixpkgs", nixpkgs, 1_760_000_000), ""),
            llm_agents_node = node(
                github("numtide", "llm-agents.nix", REV_D, 1_760_000_000),
                &format!(r#""systems": "systems"{treefmt_input}"#),
            ),
            systems_node = node(github("nix-systems", "default", systems, 1_700_000_000), ""),
        )
    }

    fn modification(content: String) -> Vec<FileModification> {
        vec![FileModification {
            path: "flake.lock".to_string(),
            state: FileState::Write(content),
        }]
    }

    async fn summarize(old: String, new: String, budget: usize) -> Option<String> {
        let snapshot = MockSnapshot::with_file("flake.lock", &old);
        let diff = lock_diff(&snapshot, &modification(new)).await?;
        let commits = vec![None; diff.direct.len()];
        render(&diff, &commits, budget)
    }

    #[tokio::test]
    async fn lists_direct_and_indirect_inputs() {
        let summary = summarize(
            lock(REV_A, REV_A, None),
            lock(REV_B, REV_B, Some(REV_C)),
            5_000,
        )
        .await
        .unwrap();

        assert_eq!(
            summary,
            "**Direct inputs**\n\n\
             - `~` `nixpkgs` ([NixOS/nixpkgs](https://github.com/NixOS/nixpkgs)): \
             [`aaaaaaa` → `bbbbbbb`](https://github.com/NixOS/nixpkgs/compare/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa...bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb) (2025-10-09)\n\
             \n\
             **Indirect inputs**\n\n\
             1 indirect input changed, 1 added, 0 removed.\n\n\
             - `~` `systems` ([nix-systems/default](https://github.com/nix-systems/default)): \
             [`aaaaaaa` → `bbbbbbb`](https://github.com/nix-systems/default/compare/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa...bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb) (2023-11-14); used by `llm-agents`\n\
             - `+` `treefmt-nix` ([numtide/treefmt-nix](https://github.com/numtide/treefmt-nix)): `ccccccc`; used by `llm-agents`\n\
             \n"
        );
    }

    /// A `systems` that every input has its own copy of, and one that is reached from another
    /// input too, and a removed one.
    fn shared_lock(rev: &str, with_flake_utils: bool) -> String {
        let systems = |rev: &str| node(github("nix-systems", "default", rev, 1_700_000_000), "");
        let flake_utils = if with_flake_utils {
            format!(
                r#""flake-utils": {},"#,
                node(github("numtide", "flake-utils", REV_C, 1_700_000_000), "")
            )
        } else {
            String::new()
        };
        let fu_input = if with_flake_utils {
            r#", "flake-utils": "flake-utils""#
        } else {
            ""
        };

        format!(
            r#"{{ "nodes": {{
                "a": {a}, "b": {b}, "systems": {s1}, "systems_2": {s2}, {flake_utils}
                "root": {{ "inputs": {{ "a": "a", "b": "b" }} }}
            }}, "root": "root" }}"#,
            a = node(
                github("o", "a", REV_D, 1_760_000_000),
                &format!(r#""systems": "systems"{fu_input}"#)
            ),
            b = node(
                github("o", "b", REV_D, 1_760_000_000),
                &format!(r#""systems": "systems_2"{fu_input}"#)
            ),
            s1 = systems(rev),
            s2 = systems(rev),
        )
    }

    #[tokio::test]
    async fn names_a_node_by_its_input_and_says_where_it_is_used() {
        let summary = summarize(shared_lock(REV_A, true), shared_lock(REV_B, false), 5_000)
            .await
            .unwrap();

        assert!(
            summary.contains(
                "- `~` `systems` ([nix-systems/default](https://github.com/nix-systems/default)): "
            ),
            "{summary}"
        );
        assert!(summary.contains("; used by `a`, `b`\n"), "{summary}");
        assert!(
            summary.contains(
                "- `-` `flake-utils` ([numtide/flake-utils](https://github.com/numtide/flake-utils)): was `ccccccc`; used by `a`, `b`\n"
            ),
            "{summary}"
        );
        assert!(
            summary.contains("2 indirect inputs changed, 0 added, 1 removed."),
            "{summary}"
        );
    }

    #[tokio::test]
    async fn says_so_when_no_direct_inputs_changed() {
        let summary = summarize(lock(REV_A, REV_A, None), lock(REV_A, REV_B, None), 5_000)
            .await
            .unwrap();

        assert!(
            summary.starts_with("**Direct inputs**\n\nNo direct inputs changed.\n\n"),
            "{summary}"
        );
        assert!(summary.contains("1 indirect input changed, 0 added, 0 removed."));
    }

    #[tokio::test]
    async fn says_so_when_no_indirect_inputs_changed() {
        let summary = summarize(lock(REV_A, REV_A, None), lock(REV_B, REV_A, None), 5_000)
            .await
            .unwrap();

        assert!(
            summary.ends_with("**Indirect inputs**\n\nNo indirect inputs changed.\n\n"),
            "{summary}"
        );
    }

    #[tokio::test]
    async fn is_none_when_the_lock_does_not_change() {
        let content = lock(REV_A, REV_A, None);
        let snapshot = MockSnapshot::with_file("flake.lock", &content);

        assert!(lock_diff(&snapshot, &modification(content)).await.is_none());
        assert!(lock_diff(&snapshot, &[]).await.is_none());
    }

    #[tokio::test]
    async fn is_none_without_a_lock_to_compare_with() {
        let snapshot = MockSnapshot::empty();

        assert!(
            lock_diff(&snapshot, &modification(lock(REV_A, REV_A, None)))
                .await
                .is_none()
        );
    }

    #[tokio::test]
    async fn the_count_of_indirect_inputs_survives_a_tight_budget() {
        let old = lock(REV_A, REV_A, None);
        let new = lock(REV_B, REV_B, Some(REV_C));
        let full = summarize(old.clone(), new.clone(), 5_000).await.unwrap();

        for budget in 0..full.len() {
            if let Some(summary) = summarize(old.clone(), new.clone(), budget).await {
                assert!(summary.len() <= budget, "{budget}: {summary}");
            }
        }

        let tight = summarize(old, new, full.len() - 60).await.unwrap();
        assert!(
            tight.contains("1 indirect input changed, 1 added, 0 removed."),
            "{tight}"
        );
        assert!(tight.contains("… and 1 more"), "{tight}");
    }
}
