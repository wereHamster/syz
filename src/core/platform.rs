use anyhow::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::Arc;

use crate::core::clients::github::GitHub;
use crate::core::clients::tangled::Tangled;
use crate::core::engine::repository::{ProjectRepositoryMutator, ProjectRepositoryView};

/// A seam for interacting with different project hosting platforms.
pub trait ProjectPlatform: Send + Sync {
    fn repository(&self, repository: &str) -> Box<dyn ProjectPlatformRepository>;
}

#[async_trait]
pub trait ProjectPlatformRepository: Send + Sync {
    async fn view(&self) -> Result<Box<dyn ProjectRepositoryView>>;
    async fn mutator(&self) -> Result<Box<dyn ProjectRepositoryMutator>>;
}

/// Helper to split a repository string into owner and repo.
///
/// Both GitHub and Tangled structure the repository as "<owner>/<repo>".
fn parse_repository(repository: &str) -> Result<(String, String)> {
    let parts: Vec<&str> = repository.split('/').collect();
    if parts.len() != 2 {
        anyhow::bail!("Repository must be in the format owner/repo");
    }
    Ok((parts[0].to_string(), parts[1].to_string()))
}

pub struct GitHubPlatformAdapter {
    client: GitHub,
}

impl GitHubPlatformAdapter {
    pub fn new(client: GitHub) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ProjectPlatform for GitHubPlatformAdapter {
    fn repository(&self, repository: &str) -> Box<dyn ProjectPlatformRepository> {
        Box::new(GitHubPlatformRepository {
            client: self.client.clone(),
            repository: repository.to_string(),
        })
    }
}

struct GitHubPlatformRepository {
    client: GitHub,
    repository: String,
}

#[async_trait]
impl ProjectPlatformRepository for GitHubPlatformRepository {
    async fn view(&self) -> Result<Box<dyn ProjectRepositoryView>> {
        let (owner, repo) = parse_repository(&self.repository)?;
        Ok(Box::new(
            self.client.project_repository_view(owner, repo).await?,
        ))
    }

    async fn mutator(&self) -> Result<Box<dyn ProjectRepositoryMutator>> {
        let (owner, repo) = parse_repository(&self.repository)?;
        Ok(Box::new(
            self.client.project_repository_mutator(owner, repo).await?,
        ))
    }
}

pub struct TangledPlatformAdapter {
    client: Tangled,
}

impl TangledPlatformAdapter {
    pub fn new(client: Tangled) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ProjectPlatform for TangledPlatformAdapter {
    fn repository(&self, repository: &str) -> Box<dyn ProjectPlatformRepository> {
        Box::new(TangledPlatformRepository {
            client: self.client.clone(),
            repository: repository.to_string(),
        })
    }
}

struct TangledPlatformRepository {
    client: Tangled,
    repository: String,
}

#[async_trait]
impl ProjectPlatformRepository for TangledPlatformRepository {
    async fn view(&self) -> Result<Box<dyn ProjectRepositoryView>> {
        let (owner, repo) = parse_repository(&self.repository)?;
        Ok(Box::new(
            self.client.project_repository_view(owner, repo).await?,
        ))
    }

    async fn mutator(&self) -> Result<Box<dyn ProjectRepositoryMutator>> {
        let (owner, repo) = parse_repository(&self.repository)?;
        Ok(Box::new(
            self.client.project_repository_mutator(owner, repo).await?,
        ))
    }
}

/// Registry for resolving project platforms by their identifier.
pub struct PlatformRegistry {
    platforms: HashMap<String, Arc<dyn ProjectPlatform>>,
}

impl PlatformRegistry {
    pub fn new() -> Self {
        Self {
            platforms: HashMap::new(),
        }
    }

    pub fn register(&mut self, id: &str, platform: Arc<dyn ProjectPlatform>) {
        self.platforms.insert(id.to_string(), platform);
    }

    pub fn resolve(&self, id: &str) -> Result<Arc<dyn ProjectPlatform>> {
        self.platforms
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Unsupported project platform: {}", id))
    }
}
