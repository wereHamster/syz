use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use jsonwebtoken::EncodingKey;
use octocrab::{models::AppId, Octocrab};
use serde_json::Value;

use crate::core::engine::repository::{ProjectRepositorySnapshot, ProjectRepositoryView};

#[derive(Clone)]
pub struct GitHub {
    octocrab: Octocrab,
}

impl GitHub {
    pub async fn new() -> Result<GitHub> {
        let app_id_str = std::env::var("GITHUB_APP_ID").context("GITHUB_APP_ID must be set")?;
        let private_key = std::env::var("GITHUB_APP_PRIVATE_KEY")
            .context("GITHUB_APP_PRIVATE_KEY must be set")?;

        let app_id: u64 = app_id_str
            .parse()
            .context("GITHUB_APP_ID must be a number")?;

        let formatted_key = private_key.replace("\\n", "\n");
        let key = EncodingKey::from_rsa_pem(formatted_key.as_bytes())
            .context("Failed to parse GITHUB_APP_PRIVATE_KEY as RSA PEM")?;

        let octocrab = Octocrab::builder()
            .app(AppId(app_id), key)
            .build()
            .context("Failed to build client")?;

        Ok(GitHub { octocrab })
    }

    pub async fn project_repository_view(
        &self,
        owner: String,
        repo: String,
    ) -> Result<GitHubProjectRepositoryView> {
        let installations = self
            .octocrab
            .apps()
            .installations()
            .send()
            .await
            .context("Failed to fetch GitHub App installations")?;

        let mut installation_id = None;
        for inst in installations.items {
            if inst.account.login == owner {
                installation_id = Some(inst.id.0);
                break;
            }
        }

        let inst_id = installation_id
            .context(format!("GitHub App is not installed for owner '{}'", owner))?;

        let octocrab = self
            .octocrab
            .installation(octocrab::models::InstallationId(inst_id))
            .context("Failed to create installation client")?;

        Ok(GitHubProjectRepositoryView {
            octocrab,
            owner,
            repo,
        })
    }

    pub async fn get_json(&self, route: &str) -> Result<serde_json::Value> {
        let response: serde_json::Value = self.octocrab.get(route, None::<&()>).await?;
        Ok(response)
    }
}

pub struct GitHubProjectRepositoryView {
    octocrab: Octocrab,
    owner: String,
    repo: String,
}

#[async_trait]
impl ProjectRepositoryView for GitHubProjectRepositoryView {
    async fn get_default_branch(&self) -> Result<String> {
        let repo = self.octocrab.repos(&self.owner, &self.repo).get().await?;
        Ok(repo.default_branch.unwrap_or_else(|| "main".to_string()))
    }

    async fn get_revision(&self, branch_name: &str) -> Result<String> {
        let url = format!(
            "/repos/{}/{}/git/refs/heads/{}",
            self.owner, self.repo, branch_name
        );
        let response = self.octocrab.get::<Value, _, _>(&url, None::<&()>).await?;

        let commit_sha = response["object"]["sha"].as_str().unwrap_or("").to_string();
        if commit_sha.is_empty() {
            anyhow::bail!("Branch {} not found", branch_name);
        }

        Ok(commit_sha)
    }

    fn snapshot(&self, revision: &str) -> Box<dyn ProjectRepositorySnapshot> {
        Box::new(GitHubProjectRepositorySnapshot::new(
            self.octocrab.clone(),
            self.owner.clone(),
            self.repo.clone(),
            revision.to_string(),
        ))
    }
}

pub struct GitHubProjectRepositorySnapshot {
    octocrab: Octocrab,
    owner: String,
    repo: String,
    revision: String,
}

impl GitHubProjectRepositorySnapshot {
    pub fn new(octocrab: Octocrab, owner: String, repo: String, revision: String) -> Self {
        Self {
            octocrab,
            owner,
            repo,
            revision,
        }
    }
}

#[async_trait]
impl ProjectRepositorySnapshot for GitHubProjectRepositorySnapshot {
    async fn list_files(&self) -> Result<Vec<String>> {
        let url = format!(
            "/repos/{}/{}/git/trees/{}?recursive=1",
            self.owner, self.repo, self.revision
        );
        let tree: Value = self.octocrab.get(url, None::<&()>).await?;

        let mut files = Vec::new();

        if let Some(tree_arr) = tree.get("tree").and_then(|t| t.as_array()) {
            for item in tree_arr {
                if let (Some(path), Some(item_type)) = (
                    item.get("path").and_then(|p| p.as_str()),
                    item.get("type").and_then(|t| t.as_str()),
                ) {
                    if item_type == "blob" {
                        files.push(path.to_string());
                    }
                }
            }
        }

        Ok(files)
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        let result = self
            .octocrab
            .repos(&self.owner, &self.repo)
            .get_content()
            .path(path)
            .r#ref(self.revision.as_str())
            .send()
            .await?;

        if result.items.is_empty() {
            anyhow::bail!("File not found");
        }

        let item = &result.items[0];

        if let Some(content_base64) = &item.content {
            let content_cleaned = content_base64.replace('\n', "");
            let decoded = general_purpose::STANDARD.decode(content_cleaned)?;
            let text = String::from_utf8(decoded)?;
            Ok(text)
        } else {
            anyhow::bail!("No content in file");
        }
    }
}
