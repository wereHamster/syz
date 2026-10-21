use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::ecosystems::nix_flake::internal::affected_outputs::{
    self, store_name, AffectedOutputs, ChangedOutput,
};
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
    vec![
        Box::new(NixFlakeSummarySection),
        Box::new(NixFlakeAffectedOutputsSection),
    ]
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

/// Says which outputs of the flake the update changes, and which it leaves alone.
pub struct NixFlakeAffectedOutputsSection;

#[async_trait]
impl PullRequestSectionGenerator for NixFlakeAffectedOutputsSection {
    fn name(&self) -> &'static str {
        "nix-flake affected outputs"
    }

    fn kind(&self) -> SectionKind {
        SectionKind::Impact
    }

    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        budget: usize,
    ) -> Result<Option<String>> {
        let (Some(github), Some(work_dir)) = (ctx.github, ctx.work_dir) else {
            return Ok(None);
        };

        let affected =
            affected_outputs::analyze(github, ctx.snapshot, work_dir, ctx.modifications).await?;
        Ok(affected.and_then(|affected| render_outputs(&affected, budget)))
    }
}

/// Output names are `category.name`, or just `formatter`.
fn category_of(name: &str) -> (&str, Option<&str>) {
    match name.split_once('.') {
        Some((category, rest)) => (category, Some(rest)),
        None => (name, None),
    }
}

/// What an output's change comes to, where that says something: a new or removed output, or a
/// different name of the derivation (usually a version).
fn change_detail(change: &ChangedOutput, with_names: bool) -> Option<String> {
    match (&change.before, &change.after) {
        (None, _) => Some("(new)".to_string()),
        (_, None) => Some("(removed)".to_string()),
        (Some(before), Some(after)) if with_names => {
            let (before, after) = (store_name(before)?, store_name(after)?);
            (before != after).then(|| format!("{} → {}", code(before), code(after)))
        }
        _ => None,
    }
}

/// The changed outputs as nested bullets: a bullet per category, a bullet per output below it.
/// Each element is a whole number of lines, and a category's first row carries the category's
/// bullet, so that dropping rows from the end never leaves a category empty.
fn change_rows(changed: &[ChangedOutput], with_names: bool) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    let mut category: Option<&str> = None;

    for change in changed {
        let (this_category, name) = category_of(&change.name);
        let detail = change_detail(change, with_names);

        let Some(name) = name else {
            // An output without a name of its own, such as the formatter.
            let mut row = format!("- {}", code(this_category));
            if let Some(detail) = detail {
                row.push_str(&format!(": {detail}"));
            }
            row.push('\n');
            rows.push(row);
            category = None;
            continue;
        };

        let mut row = String::new();
        if category != Some(this_category) {
            row.push_str(&format!("- {}\n", code(this_category)));
            category = Some(this_category);
        }
        row.push_str(&format!("  - {}", code(name)));
        if let Some(detail) = detail {
            row.push_str(&format!(": {detail}"));
        }
        row.push('\n');
        rows.push(row);
    }

    rows
}

/// The changed outputs no longer than `limit`. When they don't fit, the names of the
/// derivations go first, then the outputs at the end.
fn render_changes(changed: &[ChangedOutput], limit: usize) -> Option<String> {
    let omitted = |n: usize| format!("- … and {} more\n", n);

    let full = change_rows(changed, true);
    let rows = if full.iter().map(String::len).sum::<usize>() < limit {
        full
    } else {
        change_rows(changed, false)
    };

    let fitted = fit_items("", &rows, "\n", limit, omitted)?;

    // Only the note that everything was left out says nothing.
    (fitted != format!("{}\n", omitted(rows.len()))).then_some(fitted)
}

/// Explains why the outputs below it can't be attributed to the update.
const ANY_LOCK_EDIT: &str = "These outputs are built from the whole flake source, which includes `flake.lock`, so they change on any edit of the lock, and what this update does to them can't be told:";

/// The outputs that change with any lock edit, with the explanation, no longer than `limit`.
fn render_any_lock_edit(names: &[String], limit: usize) -> Option<String> {
    let items: Vec<String> = names
        .iter()
        .map(|name| format!("- {}\n", code(name)))
        .collect();
    fit_items(&format!("{ANY_LOCK_EDIT}\n\n"), &items, "\n", limit, |n| {
        format!("- … and {n} more\n")
    })
}

/// Whether `name` is an output that is evaluated for one system.
fn is_per_system(name: &str) -> bool {
    ["devShells", "packages", "checks", "apps", "formatter"].contains(&category_of(name).0)
}

/// The section for `affected`, no longer than `budget`. `None` if nothing was evaluated, or
/// there is no room to say what changed.
///
/// What the update leaves alone isn't listed. The outputs that change with any lock edit come
/// after the ones that change, since they say less, and so do those that could not be
/// evaluated, which are only counted.
fn render_outputs(affected: &AffectedOutputs, budget: usize) -> Option<String> {
    if affected.is_empty() {
        return None;
    }

    let every_name = || {
        affected
            .changed
            .iter()
            .map(|change| &change.name)
            .chain(&affected.changes_on_any_lock_edit)
            .chain(&affected.unchanged)
            .chain(&affected.unevaluated)
    };
    let mut head = String::from("# Affected outputs\n\n");
    if every_name().any(|name| is_per_system(name)) {
        head.push_str(&format!(
            "Per-system outputs are evaluated for {}.\n\n",
            code(&affected.system)
        ));
    }

    let unevaluated = match affected.unevaluated.len() {
        0 => String::new(),
        1 => "1 output could not be evaluated and is not listed.\n\n".to_string(),
        n => format!("{n} outputs could not be evaluated and are not listed.\n\n"),
    };

    let mut remaining = budget.checked_sub(head.len())?;
    let unevaluated = if unevaluated.len() <= remaining {
        remaining -= unevaluated.len();
        unevaluated
    } else {
        String::new()
    };

    // What changes is what the section is about, so the rest gets a share of what is left.
    let any_edit_limit = if affected.changed.is_empty() {
        remaining
    } else {
        remaining / 3
    };
    let any_edit = if affected.changes_on_any_lock_edit.is_empty() {
        String::new()
    } else {
        render_any_lock_edit(&affected.changes_on_any_lock_edit, any_edit_limit).unwrap_or_default()
    };
    remaining -= any_edit.len();

    let changes = if !affected.changed.is_empty() {
        render_changes(&affected.changed, remaining)?
    } else if affected.changes_on_any_lock_edit.is_empty() {
        "No output is affected by this update.\n\n".to_string()
    } else {
        String::new()
    };

    Some(head + &changes + &any_edit + &unevaluated).filter(|section| section.len() <= budget)
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
    fn strings(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    const HASH: &str = "c3yh3hnrjnfp3a7kb9h5q8x1ylmcr2kh";

    fn drv(name: &str) -> String {
        format!("/nix/store/{HASH}-{name}.drv")
    }

    fn changed(name: &str, before: Option<&str>, after: Option<&str>) -> ChangedOutput {
        ChangedOutput {
            name: name.to_string(),
            before: before.map(drv),
            after: after.map(drv),
        }
    }

    fn affected(
        changed: Vec<ChangedOutput>,
        any_edit: &[&str],
        unchanged: &[&str],
        unevaluated: &[&str],
    ) -> AffectedOutputs {
        AffectedOutputs {
            system: "x86_64-linux".to_string(),
            changed,
            changes_on_any_lock_edit: strings(any_edit),
            unchanged: strings(unchanged),
            unevaluated: strings(unevaluated),
        }
    }

    fn example() -> AffectedOutputs {
        affected(
            vec![
                changed("devShells.default", Some("shell"), Some("shell")),
                changed(
                    "nixosConfigurations.host-a",
                    Some("nixos-system-host-a-25.11.20261001.a1b2c3d"),
                    Some("nixos-system-host-a-25.11.20261015.e4f5a6b"),
                ),
                changed(
                    "nixosConfigurations.host-b",
                    Some("nixos-system-host-b-25.11.20261001.a1b2c3d"),
                    Some("nixos-system-host-b-25.11.20261015.e4f5a6b"),
                ),
                changed("packages.docs", Some("docs"), Some("docs")),
                changed("packages.extras", None, Some("extras")),
                changed("packages.tool", Some("tool-1.4.0"), Some("tool-1.5.0")),
            ],
            &["packages.default"],
            &["nixosConfigurations.host-c"],
            &["nixosConfigurations.host-d", "packages.broken", "apps.run"],
        )
    }

    #[test]
    fn lists_the_changes_by_category_with_what_is_known_about_each() {
        let section = render_outputs(&example(), 2_500).unwrap();

        assert_eq!(
            section,
            format!(
                "# Affected outputs\n\n\
                 Per-system outputs are evaluated for `x86_64-linux`.\n\n\
                 - `devShells`\n  - `default`\n\
                 - `nixosConfigurations`\n\
                 \x20 - `host-a`: `nixos-system-host-a-25.11.20261001.a1b2c3d` → `nixos-system-host-a-25.11.20261015.e4f5a6b`\n\
                 \x20 - `host-b`: `nixos-system-host-b-25.11.20261001.a1b2c3d` → `nixos-system-host-b-25.11.20261015.e4f5a6b`\n\
                 - `packages`\n  - `docs`\n  - `extras`: (new)\n  - `tool`: `tool-1.4.0` → `tool-1.5.0`\n\n\
                 {ANY_LOCK_EDIT}\n\n- `packages.default`\n\n\
                 3 outputs could not be evaluated and are not listed.\n\n"
            )
        );
    }

    #[test]
    fn outputs_without_a_name_of_their_own_are_a_single_bullet() {
        let section = render_outputs(
            &affected(
                vec![changed("formatter", Some("fmt-1"), Some("fmt-2"))],
                &[],
                &[],
                &[],
            ),
            2_500,
        )
        .unwrap();

        assert_eq!(
            section,
            "# Affected outputs\n\n\
             Per-system outputs are evaluated for `x86_64-linux`.\n\n\
             - `formatter`: `fmt-1` → `fmt-2`\n\n"
        );
    }

    #[test]
    fn says_so_when_the_update_affects_no_output() {
        let section = render_outputs(
            &affected(
                vec![],
                &[],
                &["nixosConfigurations.host-a", "packages.tool"],
                &[],
            ),
            2_500,
        )
        .unwrap();

        assert_eq!(
            section,
            "# Affected outputs\n\n\
             Per-system outputs are evaluated for `x86_64-linux`.\n\n\
             No output is affected by this update.\n\n"
        );
    }

    #[test]
    fn leaves_out_the_system_when_no_output_depends_on_it() {
        let section = render_outputs(
            &affected(
                vec![changed("nixosConfigurations.host-a", Some("a"), Some("b"))],
                &[],
                &[],
                &["nixosConfigurations.host-b"],
            ),
            2_500,
        )
        .unwrap();

        assert_eq!(
            section,
            "# Affected outputs\n\n\
             - `nixosConfigurations`\n  - `host-a`: `a` → `b`\n\n\
             1 output could not be evaluated and is not listed.\n\n"
        );
    }

    #[test]
    fn explains_outputs_that_change_with_any_lock_edit_even_when_nothing_else_changes() {
        let section =
            render_outputs(&affected(vec![], &["packages.default"], &[], &[]), 2_500).unwrap();

        assert_eq!(
            section,
            format!(
                "# Affected outputs\n\n\
                 Per-system outputs are evaluated for `x86_64-linux`.\n\n\
                 {ANY_LOCK_EDIT}\n\n- `packages.default`\n\n"
            )
        );
    }

    #[test]
    fn is_omitted_when_nothing_was_evaluated() {
        assert_eq!(
            render_outputs(&affected(vec![], &[], &[], &[]), 2_500),
            None
        );
        assert_eq!(
            render_outputs(&affected(vec![], &[], &[], &["packages.tool"]), 2_500),
            None
        );
    }

    #[test]
    fn stays_within_the_budget_and_cuts_detail_before_outputs() {
        let many: Vec<ChangedOutput> = (0..200)
            .map(|i| {
                changed(
                    &format!("packages.tool-{i}"),
                    Some(&format!("tool-{i}-1.0")),
                    Some(&format!("tool-{i}-2.0")),
                )
            })
            .collect();
        let lock_edit: Vec<String> = (0..200).map(|i| format!("checks.test-{i}")).collect();
        let lock_edit: Vec<&str> = lock_edit.iter().map(String::as_str).collect();

        for budget in [0, 40, 100, 300, 600, 1_500, 2_500] {
            let affected = affected(many.clone(), &lock_edit, &[], &["apps.run"]);
            if let Some(section) = render_outputs(&affected, budget) {
                assert!(section.len() <= budget, "{budget}: {section}");
                assert!(section.contains("`tool-0`"), "{budget}: {section}");
            }
        }

        let section = render_outputs(&affected(many.clone(), &[], &[], &[]), 2_500).unwrap();
        assert!(section.contains("- … and "), "{section}");

        let section = render_outputs(&affected(many[..3].to_vec(), &[], &[], &[]), 400).unwrap();
        assert!(
            section.contains("`tool-2`: `tool-2-1.0` → `tool-2-2.0`"),
            "{section}"
        );
        let tight = render_outputs(&affected(many[..3].to_vec(), &[], &[], &[]), 150).unwrap();
        assert!(tight.contains("`tool-2`\n"), "{tight}");
        assert!(!tight.contains('→'), "{tight}");
    }
}
