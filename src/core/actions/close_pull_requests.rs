use crate::core::engine::repository::ProjectRepositoryMutator;
use crate::core::job::LaneKey;
use crate::core::services::Services;
use anyhow::Result;

/// Close the pull requests opened from the given branches, and delete the branches.
///
/// The caller decided that nothing tracks these pull requests anymore, so this looks nothing
/// up: the records that owned them may be gone already.
pub async fn run(services: &Services, lane: LaneKey, branches: Vec<String>) -> Result<()> {
    let repository = services.repository(&lane)?;
    let default_branch = repository.view().await?.get_default_branch().await?;
    let mutator = repository.mutator().await?;

    close_all(mutator.as_ref(), &default_branch, &branches).await
}

/// Close the pull request of every branch, even if closing one of them fails.
async fn close_all(
    mutator: &dyn ProjectRepositoryMutator,
    default_branch: &str,
    branches: &[String],
) -> Result<()> {
    let mut failed = Vec::new();

    for branch in branches {
        match mutator.close_pull_request(branch, default_branch).await {
            Ok(()) => tracing::info!("Closed the pull request of {}", branch),
            Err(e) => {
                tracing::error!("Failed to close the pull request of {}: {:#}", branch, e);
                failed.push(branch.as_str());
            }
        }
    }

    if !failed.is_empty() {
        anyhow::bail!("Failed to close the pull requests of {}", failed.join(", "));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::actions::test_support::RecordingMutator;

    #[tokio::test]
    async fn closes_every_branch_even_if_one_fails() {
        let mutator = RecordingMutator {
            fail_closing: vec!["syz/a".to_string()],
            ..RecordingMutator::default()
        };

        let err = close_all(
            &mutator,
            "main",
            &["syz/a".to_string(), "syz/b".to_string()],
        )
        .await
        .unwrap_err();

        assert_eq!(
            mutator.calls(),
            vec!["close syz/a against main", "close syz/b against main"]
        );
        assert_eq!(
            err.to_string(),
            "Failed to close the pull requests of syz/a"
        );
    }
}
