use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;

use crate::core::clients::github::GitHub;
use crate::core::clients::tangled::Tangled;
use crate::core::engine::repository::{ProjectRepositoryMutator, ProjectRepositoryView};

/// The project hosting platform a project lives on.
///
/// This is the in-core representation; strings only appear at IO edges (CLI
/// arguments, database rows) and are parsed into this enum immediately.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Platform {
    GitHub,
    Tangled,
}

impl Platform {
    pub fn as_str(&self) -> &'static str {
        match self {
            Platform::GitHub => "github",
            Platform::Tangled => "tangled",
        }
    }
}

impl std::str::FromStr for Platform {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "github" => Ok(Platform::GitHub),
            "tangled" => Ok(Platform::Tangled),
            other => anyhow::bail!("Unsupported project platform: {}", other),
        }
    }
}

impl std::fmt::Display for Platform {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

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
    platforms: HashMap<Platform, Arc<dyn ProjectPlatform>>,
}

impl PlatformRegistry {
    pub fn new() -> Self {
        Self {
            platforms: HashMap::new(),
        }
    }

    pub fn register(&mut self, id: Platform, platform: Arc<dyn ProjectPlatform>) {
        self.platforms.insert(id, platform);
    }

    pub fn resolve(&self, id: Platform) -> Result<Arc<dyn ProjectPlatform>> {
        self.platforms
            .get(&id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Unsupported project platform: {}", id))
    }
}

impl Default for PlatformRegistry {
    fn default() -> Self {
        Self::new()
    }
}
