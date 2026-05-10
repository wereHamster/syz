use std::collections::HashMap;

use anyhow::Result;

use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency};

use super::Registry;

pub struct RegistryRouter {
    registries: HashMap<String, Box<dyn Registry>>,
}

impl RegistryRouter {
    pub fn new(registries: HashMap<String, Box<dyn Registry>>) -> Self {
        Self { registries }
    }

    pub async fn query_dependency_update_options(
        &self,
        dependency: &DiscoveredDependency,
    ) -> Result<DependencyUpdateOption> {
        let pkg_type = &dependency.purl.ecosystem;

        let registry = self.registries.get(pkg_type).ok_or_else(|| {
            anyhow::anyhow!("No registry configured for ecosystem: {}", pkg_type)
        })?;

        registry.query_dependency_update_options(dependency).await
    }
}
