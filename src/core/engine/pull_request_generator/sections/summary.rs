//! The layout the summaries of the ecosystems have in common.
//!
//! An ecosystem's summary section calls [`generate`] with what is specific to it.

use anyhow::Result;

use crate::core::engine::pull_request_generator::budget::fit_items;
use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;
use crate::core::engine::UpdateTarget;

/// Where to see what changed between two versions of a package: name, old and new version.
pub type DiffUrl = fn(&str, &str, &str) -> Option<String>;

/// The source repository of the target, from the registry if the target doesn't carry it.
async fn repo_url(ctx: &PullRequestGenerationContext<'_>, target: &UpdateTarget) -> String {
    if let Some(url) = target.package_info.repo_url.as_deref() {
        if !url.is_empty() {
            return url.to_string();
        }
    }

    match ctx
        .registry_router
        .fetch_package_info(ctx.ecosystem, &target.name)
        .await
    {
        Ok(info) => info.repo_url.unwrap_or_default(),
        Err(_) => String::new(),
    }
}

/// `[text](url)`, or just `text` when there is no url.
fn link(text: &str, url: Option<&str>) -> String {
    match url {
        Some(url) if !url.is_empty() => format!("[{}]({})", text, url),
        _ => text.to_string(),
    }
}

fn is_major_bump(target: &UpdateTarget) -> bool {
    semver::Version::parse(target.current_version.version.trim_start_matches('v'))
        .ok()
        .zip(semver::Version::parse(target.target_version.version.trim_start_matches('v')).ok())
        .map(|(old, new)| old.major != new.major)
        .unwrap_or(false)
}

/// Builds the summary of the targets in ctx, no longer than `budget`.
///
/// When there are more packages than fit, the last ones are dropped from the list.
pub async fn generate(
    ctx: &PullRequestGenerationContext<'_>,
    budget: usize,
    diff_url: DiffUrl,
) -> Result<Option<String>> {
    let targets = ctx.targets;
    let package_group = ctx.package_group;

    let Some(first) = targets.first() else {
        return Ok(None);
    };

    let omitted = |n: usize| format!("- … and {} more\n", n);

    if targets.len() == 1 {
        let repo = repo_url(ctx, first).await;
        let name = if repo.is_empty() {
            format!("`{}`", first.name)
        } else {
            link(&first.name, Some(&repo))
        };
        let (old, new) = (
            &first.current_version.version,
            &first.target_version.version,
        );
        let versions = link(
            &format!("{} to {}", old, new),
            diff_url(&first.name, old, new).as_deref(),
        );

        let body = format!("This PR updates {} from version {}.\n\n", name, versions);
        return Ok(Some(body).filter(|body| body.len() <= budget));
    }

    let all_same = targets.iter().all(|t| {
        t.current_version.version == first.current_version.version
            && t.target_version.version == first.target_version.version
    });

    if all_same {
        let (old, new) = (
            &first.current_version.version,
            &first.target_version.version,
        );
        let repo = repo_url(ctx, first).await;
        let group = if repo.is_empty() {
            format!("`{}`", package_group)
        } else {
            link(package_group, Some(&repo))
        };

        let intro = format!(
            "This PR updates {} packages from {} to {}. The following packages are part of this set:\n\n",
            group, old, new
        );
        let items: Vec<String> = targets
            .iter()
            .map(|t| match diff_url(&t.name, old, new) {
                Some(url) => format!("- `{}` ([diff]({}))\n", t.name, url),
                None => format!("- `{}`\n", t.name),
            })
            .collect();

        return Ok(fit_items(&intro, &items, "\n", budget, omitted));
    }

    let intro = format!(
        "This PR updates {} dependencies from the **{}** group.\n\n",
        targets.len(),
        package_group
    );
    let mut items = Vec::with_capacity(targets.len());

    for target in targets {
        let repo = repo_url(ctx, target).await;
        let name = if repo.is_empty() {
            format!("`{}`", target.name)
        } else {
            link(&target.name, Some(&repo))
        };
        let (old, new) = (
            &target.current_version.version,
            &target.target_version.version,
        );
        let versions = link(
            &format!("`{}` to `{}`", old, new),
            diff_url(&target.name, old, new).as_deref(),
        );
        let major = if is_major_bump(target) {
            " (major)"
        } else {
            ""
        };

        items.push(format!("- {} from {}{}\n", name, versions, major));
    }

    Ok(fit_items(&intro, &items, "\n", budget, omitted))
}
