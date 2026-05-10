use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use jsonwebtoken::EncodingKey;
use octocrab::{models::AppId, Octocrab};
use serde_json::Value;

use crate::core::engine::repository::{
    FileModification, FileState, ProjectRepositoryMutator, ProjectRepositorySnapshot, ProjectRepositoryView,
};

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

    pub async fn project_repository_mutator(
        &self,
        owner: String,
        repo: String,
    ) -> Result<GitHubProjectRepositoryMutator> {
        let view = self.project_repository_view(owner.clone(), repo.clone()).await?;
        Ok(GitHubProjectRepositoryMutator {
            octocrab: view.octocrab,
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

pub struct GitHubProjectRepositoryMutator {
    octocrab: Octocrab,
    owner: String,
    repo: String,
}

pub struct TreeItem {
    path: String,
    sha: Option<String>,
}

impl GitHubProjectRepositoryMutator {
    pub async fn create_blob(&self, content: &str) -> Result<String> {
        let response: Value = self
            .octocrab
            .post(
                format!("/repos/{}/{}/git/blobs", self.owner, self.repo),
                Some(&serde_json::json!({
                    "content": content,
                    "encoding": "utf-8"
                })),
            )
            .await?;

        let sha = response["sha"]
            .as_str()
            .context("Missing sha in blob")?
            .to_string();
        Ok(sha)
    }

    pub async fn create_tree(&self, base_tree: &str, tree_items: Vec<TreeItem>) -> Result<String> {
        let mut tree = Vec::new();
        for item in tree_items {
            let mut obj = serde_json::json!({
                "path": item.path,
                "mode": "100644",
                "type": "blob",
            });
            if let Some(sha) = item.sha {
                obj.as_object_mut()
                    .unwrap()
                    .insert("sha".to_string(), serde_json::Value::String(sha));
            } else {
                obj.as_object_mut()
                    .unwrap()
                    .insert("sha".to_string(), serde_json::Value::Null);
            }
            tree.push(obj);
        }

        let response: Value = self
            .octocrab
            .post(
                format!("/repos/{}/{}/git/trees", self.owner, self.repo),
                Some(&serde_json::json!({
                    "base_tree": base_tree,
                    "tree": tree
                })),
            )
            .await?;

        let sha = response["sha"]
            .as_str()
            .context("Missing sha in tree")?
            .to_string();
        Ok(sha)
    }

    pub async fn get_commit_tree_sha(&self, commit_sha: &str) -> Result<String> {
        let commit_url = format!(
            "/repos/{}/{}/git/commits/{}",
            self.owner, self.repo, commit_sha
        );
        let commit_data: Value = self.octocrab.get(commit_url, None::<&()>).await?;

        let tree_sha = commit_data["tree"]["sha"]
            .as_str()
            .unwrap_or("")
            .to_string();
        Ok(tree_sha)
    }

    pub async fn get_branch_commit_info(
        &self,
        branch: &str,
    ) -> Result<Option<(String, String, Vec<String>)>> {
        let url = format!(
            "/repos/{}/{}/git/refs/heads/{}",
            self.owner, self.repo, branch
        );
        let response = self.octocrab.get::<Value, _, _>(&url, None::<&()>).await;

        let ref_data = match response {
            Ok(v) => v,
            Err(_) => return Ok(None),
        };

        let commit_sha = ref_data["object"]["sha"].as_str().unwrap_or("").to_string();
        if commit_sha.is_empty() {
            return Ok(None);
        }

        let commit_url = format!(
            "/repos/{}/{}/git/commits/{}",
            self.owner, self.repo, commit_sha
        );
        let commit_data: Value = self.octocrab.get(commit_url, None::<&()>).await?;

        let tree_sha = commit_data["tree"]["sha"]
            .as_str()
            .unwrap_or("")
            .to_string();
        let mut parents = Vec::new();
        if let Some(parents_array) = commit_data["parents"].as_array() {
            for p in parents_array {
                if let Some(p_sha) = p["sha"].as_str() {
                    parents.push(p_sha.to_string());
                }
            }
        }

        Ok(Some((commit_sha, tree_sha, parents)))
    }

    pub async fn delete_branch(&self, branch: &str) -> Result<()> {
        let url = format!(
            "/repos/{}/{}/git/refs/heads/{}",
            self.owner, self.repo, branch
        );
        // Ignore 404s if it doesn't exist
        let _ = self.octocrab.delete::<Value, _, _>(url, None::<&()>).await;
        Ok(())
    }

    pub async fn create_commit(
        &self,
        message: &str,
        tree_sha: &str,
        parent_sha: &str,
    ) -> Result<String> {
        let response: Value = self
            .octocrab
            .post(
                format!("/repos/{}/{}/git/commits", self.owner, self.repo),
                Some(&serde_json::json!({
                    "message": message,
                    "tree": tree_sha,
                    "parents": [parent_sha]
                })),
            )
            .await?;

        let sha = response["sha"]
            .as_str()
            .context("Missing sha in commit")?
            .to_string();
        Ok(sha)
    }

    pub async fn create_branch(&self, new_branch: &str, sha: &str) -> Result<()> {
        let url = format!("/repos/{}/{}/git/refs", self.owner, self.repo);
        let ref_path = format!("refs/heads/{}", new_branch);

        let _response: Value = self
            .octocrab
            .post(
                url,
                Some(&serde_json::json!({
                    "ref": ref_path,
                    "sha": sha
                })),
            )
            .await?;

        Ok(())
    }

    pub async fn update_branch(&self, branch: &str, commit_sha: &str) -> Result<()> {
        let url = format!(
            "/repos/{}/{}/git/refs/heads/{}",
            self.owner, self.repo, branch
        );

        let _response: Value = self
            .octocrab
            .patch(
                url,
                Some(&serde_json::json!({
                    "sha": commit_sha,
                    "force": true
                })),
            )
            .await?;
        Ok(())
    }
}

#[async_trait]
impl ProjectRepositoryMutator for GitHubProjectRepositoryMutator {
    async fn commit_changes(
        &self,
        base_revision: &str,
        branch_name: &str,
        commit_message: &str,
        modifications: Vec<FileModification>,
    ) -> Result<Option<String>> {
        let mut tree_items = Vec::new();
        for modif in modifications {
            let sha = match modif.state {
                FileState::Write(content) => Some(self.create_blob(&content).await?),
                FileState::Delete => None,
            };
            tree_items.push(TreeItem {
                path: modif.path,
                sha,
            });
        }

        let new_tree_sha = self.create_tree(base_revision, tree_items).await?;
        let base_tree_sha = self.get_commit_tree_sha(base_revision).await?;

        if new_tree_sha == base_tree_sha {
            // No changes relative to the base revision
            let _ = self.delete_branch(branch_name).await;
            return Ok(None);
        }

        let mut needs_push = true;
        let mut branch_exists = false;

        if let Ok(Some((_, branch_tree_sha, branch_parents))) =
            self.get_branch_commit_info(branch_name).await
        {
            branch_exists = true;
            if branch_tree_sha == new_tree_sha
                && branch_parents.len() == 1
                && branch_parents[0] == base_revision
            {
                tracing::info!(
                    "Branch {} is already up to date with main. Skipping commit.",
                    branch_name
                );
                needs_push = false;
            }
        }

        if needs_push {
            let new_commit_sha = self
                .create_commit(commit_message, &new_tree_sha, base_revision)
                .await?;
            if branch_exists {
                self.update_branch(branch_name, &new_commit_sha).await?;
            } else {
                self.create_branch(branch_name, &new_commit_sha).await?;
            }
            Ok(Some(new_commit_sha))
        } else {
            // If we didn't push, we just return the existing branch's commit SHA
            let (branch_head_sha, _, _) = self.get_branch_commit_info(branch_name).await?.unwrap();
            Ok(Some(branch_head_sha))
        }
    }

    async fn create_pull_request(
        &self,
        title: &str,
        branch_name: &str,
        base_branch: &str,
        body: &str,
    ) -> Result<String> {
        let head_param = format!("{}:{}", self.owner, branch_name);
        let prs = self
            .octocrab
            .pulls(&self.owner, &self.repo)
            .list()
            .head(&head_param)
            .state(octocrab::params::State::Open)
            .send()
            .await?;

        if let Some(existing_pr) = prs.items.into_iter().next() {
            // Update the existing PR with the new title and body
            let pr_number = existing_pr.number;
            let url = format!("/repos/{}/{}/pulls/{}", self.owner, self.repo, pr_number);
            let updated_pr: Value = self
                .octocrab
                .patch(
                    url,
                    Some(&serde_json::json!({
                        "title": title,
                        "body": body
                    })),
                )
                .await?;

            return Ok(updated_pr["html_url"].as_str().unwrap_or("").to_string());
        }

        let pr = self
            .octocrab
            .pulls(&self.owner, &self.repo)
            .create(title, branch_name, base_branch)
            .body(body)
            .send()
            .await?;

        Ok(pr.html_url.to_string())
    }

    async fn close_pull_request(&self, branch_name: &str, base_branch: &str) -> Result<()> {
        let head_param = format!("{}:{}", self.owner, branch_name);
        let prs = self
            .octocrab
            .pulls(&self.owner, &self.repo)
            .list()
            .head(&head_param)
            .base(base_branch)
            .state(octocrab::params::State::Open)
            .send()
            .await?;

        if let Some(existing_pr) = prs.items.into_iter().next() {
            let pr_number = existing_pr.number;
            let url = format!("/repos/{}/{}/pulls/{}", self.owner, self.repo, pr_number);
            let _: Value = self
                .octocrab
                .patch(
                    url,
                    Some(&serde_json::json!({
                        "state": "closed"
                    })),
                )
                .await?;
            tracing::info!("Closed empty PR #{}", pr_number);
        }

        let _ = self.delete_branch(branch_name).await;

        Ok(())
    }
}

