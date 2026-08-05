use anyhow::Result;

use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency, PackageInfo};

pub async fn run(_dependency: &DiscoveredDependency) -> Result<DependencyUpdateOption> {
    Ok(DependencyUpdateOption {
        package_info: PackageInfo { repo_url: None },
        bumps: Vec::new(),
    })
}
