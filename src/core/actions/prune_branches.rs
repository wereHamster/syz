use super::close_pull_requests::close_all;
use super::process_bump::generate_branch_name;
use crate::core::database::{Bump, Sweep};
use crate::core::job::LaneKey;
use crate::core::services::Services;
use anyhow::Result;
use std::collections::HashSet;

/// The namespace that syz reserves for its own branches. Every branch under it is syz's, so
/// one that nothing tracks is deleted, whoever made it.
const BRANCH_PREFIX: &str = "syz/";

/// Close the pull requests of the branches under the `syz/` prefix that no bump or sweep
/// tracks, and delete the branches.
///
/// Events that would have cleaned up after a branch can be lost, fail or arrive out of order,
/// so this compares what the repository has with what the store tracks instead. It runs on the
/// project's lane, which waits for the outcome of each job before it starts the next one, so
/// the store already has the pull requests of every publish that ran before it.
pub async fn run(services: &Services, lane: LaneKey, project_id: String) -> Result<()> {
    let store = services.store();

    if store.find_project(&project_id).await?.is_none() {
        tracing::info!(
            "Project {} was removed, not pruning its branches",
            project_id
        );
        return Ok(());
    }

    let repository = services.repository(&lane)?;
    let view = repository.view().await?;
    let listed = view.list_branches(BRANCH_PREFIX).await?;

    let bumps = store.list_bumps().await?;
    let sweeps = store.list_sweeps().await?;
    let orphans = orphaned_branches(listed, &bumps, &sweeps, &project_id);
    if orphans.is_empty() {
        return Ok(());
    }

    tracing::info!("Pruning untracked branches: {}", orphans.join(", "));

    let default_branch = view.get_default_branch().await?;
    let mutator = repository.mutator().await?;

    close_all(mutator.as_ref(), &default_branch, &orphans).await
}

/// The listed branches that none of the project's bumps and sweeps has a pull request from.
fn orphaned_branches(
    listed: Vec<String>,
    bumps: &[Bump],
    sweeps: &[Sweep],
    project_id: &str,
) -> Vec<String> {
    let tracked: HashSet<String> = bumps
        .iter()
        .filter(|bump| bump.project_id == project_id && bump.url.is_some())
        .map(|bump| generate_branch_name(&bump.name, bump.major))
        .chain(
            sweeps
                .iter()
                .filter(|sweep| sweep.project_id == project_id)
                .map(|sweep| sweep.kind.branch_name().to_string()),
        )
        .collect();

    listed
        .into_iter()
        .filter(|branch| !tracked.contains(branch))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::database::SweepKind;

    fn bump(project_id: &str, name: &str, url: Option<&str>) -> Bump {
        Bump {
            id: format!("{}/{}", project_id, name),
            project_id: project_id.to_string(),
            name: name.to_string(),
            major: false,
            approved: true,
            url: url.map(str::to_string),
        }
    }

    fn sweep(project_id: &str, kind: SweepKind) -> Sweep {
        Sweep {
            project_id: project_id.to_string(),
            kind,
            url: "https://example.com/pull/3".to_string(),
        }
    }

    fn branches(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    #[test]
    fn keeps_the_branches_of_bumps_and_sweeps_with_a_pull_request() {
        let orphans = orphaned_branches(
            branches(&["syz/update-serde", "syz/transitive"]),
            &[bump("p", "serde", Some("https://example.com/pull/1"))],
            &[sweep("p", SweepKind::Transitive)],
            "p",
        );

        assert!(orphans.is_empty());
    }

    #[test]
    fn prunes_the_branches_nothing_tracks() {
        let orphans = orphaned_branches(
            branches(&["syz/update-serde", "syz/update-tokio", "syz/transitive"]),
            &[
                bump("p", "serde", None),
                bump("p", "tokio", Some("https://example.com/pull/2")),
            ],
            &[],
            "p",
        );

        assert_eq!(orphans, branches(&["syz/update-serde", "syz/transitive"]));
    }

    #[test]
    fn another_projects_bumps_do_not_keep_a_branch() {
        let orphans = orphaned_branches(
            branches(&["syz/update-serde"]),
            &[bump("q", "serde", Some("https://example.com/pull/1"))],
            &[],
            "p",
        );

        assert_eq!(orphans, branches(&["syz/update-serde"]));
    }

    #[test]
    fn prunes_any_untracked_branch_under_the_prefix() {
        let orphans = orphaned_branches(
            branches(&["syz/my-experiment", "syz/update-serde"]),
            &[bump("p", "serde", Some("https://example.com/pull/1"))],
            &[],
            "p",
        );

        assert_eq!(orphans, branches(&["syz/my-experiment"]));
    }
}
