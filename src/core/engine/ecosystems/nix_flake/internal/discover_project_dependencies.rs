use anyhow::Result;

use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::DiscoveredDependency;

pub async fn run(_repo: &dyn ProjectRepositorySnapshot) -> Result<Vec<DiscoveredDependency>> {
    Ok(Vec::new())
}
