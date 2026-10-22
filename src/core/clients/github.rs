use anyhow::{Context, Result};
use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use http_body_util::BodyExt;
use octocrab::{
    models::{AppId, InstallationId},
    EncodingKey, Octocrab,
};
use serde_json::Value;
use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

use super::tarball;
use crate::core::engine::repository::{
    FileModification, FileState, ProjectRepositoryMutator, ProjectRepositorySnapshot,
    ProjectRepositoryView,
};

/// How long a known installation is trusted before it is looked up again.
const INSTALLATION_TTL: Duration = Duration::from_secs(10 * 60);

/// How long the absence of an installation is trusted before it is looked up again.
const INSTALLATION_MISS_RETRY: Duration = Duration::from_secs(10);

/// The result of looking up the GitHub App installation of an account.
struct InstallationLookup {
    /// `None` if the app isn't installed for the account.
    id: Option<InstallationId>,
    fetched_at: Instant,
}

impl InstallationLookup {
    fn is_fresh(&self) -> bool {
        let max_age = match self.id {
            Some(_) => INSTALLATION_TTL,
            None => INSTALLATION_MISS_RETRY,
        };
        self.fetched_at.elapsed() < max_age
    }
}

/// The lookup result of one owner. Its lock is held while the lookup is in flight.
type InstallationSlot = Arc<Mutex<Option<InstallationLookup>>>;

#[derive(Clone)]
pub struct GitHub {
    octocrab: Octocrab,
    public_client: Octocrab,
    installations: Arc<std::sync::Mutex<HashMap<String, InstallationSlot>>>,
}

/// A raw GitHub API response: status preserved for classification, headers
/// for pagination (`Link`) and rate-limit hints, body as UTF-8 text.
pub struct GitHubHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
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

        let mut public_client = Octocrab::builder().build().unwrap();

        // Try to fetch installations and use the first one's token for public queries
        if let Ok(installations) = octocrab.apps().installations().send().await {
            if let Some(inst) = installations.items.first() {
                if let Ok(inst_client) = octocrab.installation(inst.id) {
                    public_client = inst_client;
                }
            }
        }

        Ok(GitHub {
            octocrab,
            public_client,
            installations: Arc::new(std::sync::Mutex::new(HashMap::new())),
        })
    }

    /// Returns the ID of the GitHub App installation for the `owner` account, or `None` when
    /// the app isn't installed for them.
    ///
    /// Lookups are cached per owner. Concurrent callers for the same owner share one request,
    /// while different owners are looked up concurrently. A known installation is trusted for
    /// [`INSTALLATION_TTL`], so a removed or replaced one is dropped after at most that long.
    /// A missing one is looked up again after [`INSTALLATION_MISS_RETRY`], so a new
    /// installation is found quickly but misses don't cost a request each. Failed lookups are
    /// not cached.
    async fn installation_id(&self, owner: &str) -> Result<Option<InstallationId>> {
        let owner = owner.to_lowercase();

        // The map lock is only held to find the owner's slot, never across a request.
        let slot = {
            let mut slots = self.installations.lock().unwrap();
            slots.entry(owner.clone()).or_default().clone()
        };
        let mut lookup = slot.lock().await;

        if let Some(lookup) = lookup.as_ref().filter(|lookup| lookup.is_fresh()) {
            return Ok(lookup.id);
        }

        let id = self.fetch_installation_id(&owner).await?;
        *lookup = Some(InstallationLookup {
            id,
            fetched_at: Instant::now(),
        });

        Ok(id)
    }

    /// Asks GitHub for the installation of the `owner` account. The account may be a user
    /// or an organization, so the organization is tried if there is no user installation.
    async fn fetch_installation_id(&self, owner: &str) -> Result<Option<InstallationId>> {
        fn not_found(e: &octocrab::Error) -> bool {
            matches!(e, octocrab::Error::GitHub { source, .. }
                if source.status_code == reqwest::StatusCode::NOT_FOUND)
        }

        let apps = self.octocrab.apps();

        match apps.get_user_installation(owner).await {
            Ok(installation) => return Ok(Some(installation.id)),
            Err(e) if not_found(&e) => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!(
                        "Failed to look up the GitHub App installation of '{}'",
                        owner
                    )
                })
            }
        }

        match apps.get_org_installation(owner).await {
            Ok(installation) => Ok(Some(installation.id)),
            Err(e) if not_found(&e) => Ok(None),
            Err(e) => Err(e).with_context(|| {
                format!(
                    "Failed to look up the GitHub App installation of '{}'",
                    owner
                )
            }),
        }
    }

    /// Returns an installation-scoped client for `owner`, or `None` when the GitHub App
    /// isn't installed for them.
    async fn installation_client(&self, owner: &str) -> Result<Option<Octocrab>> {
        match self.installation_id(owner).await? {
            Some(id) => Ok(Some(
                self.octocrab
                    .installation(id)
                    .context("Failed to create installation client")?,
            )),
            None => Ok(None),
        }
    }

    /// Mints a short-lived token that can read the contents of the repositories the GitHub App
    /// may access for `owner`, or `None` when the app isn't installed for them. The token is
    /// meant for tools that talk to GitHub themselves (e.g. `nix`), so it is limited to
    /// `contents: read`, whatever else the installation is allowed to do.
    pub async fn contents_read_token(&self, owner: &str) -> Result<Option<String>> {
        let Some(id) = self.installation_id(owner).await? else {
            return Ok(None);
        };

        let request = octocrab::models::apps::CreateInstallationAccessToken {
            permissions: Some(HashMap::from([(
                "contents".to_string(),
                "read".to_string(),
            )])),
            ..Default::default()
        };
        let token = self
            .octocrab
            .apps()
            .create_installation_access_token(id, &request)
            .await
            .with_context(|| format!("Failed to create an access token for '{}'", owner))?;

        Ok(Some(token.token))
    }

    /// Returns a client authorized for `owner`, falling back to `public_client` when no
    /// installation covers that owner (e.g. a public repo with no app installation at all).
    async fn client_for_owner(&self, owner: &str) -> Result<Octocrab> {
        Ok(self
            .installation_client(owner)
            .await?
            .unwrap_or_else(|| self.public_client.clone()))
    }

    pub async fn get_release_history(
        &self,
        name: &str,
        clean_current: &str,
        clean_new: &str,
    ) -> Result<Vec<crate::core::engine::Release>> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() < 2 {
            return Ok(Vec::new());
        }
        let owner = parts[0];
        let repo = parts[1];

        let route = format!("/repos/{}/{}/releases", owner, repo);
        let res = match self.get_json(&route).await {
            Ok(v) => v,
            Err(_) => return Ok(Vec::new()),
        };

        #[derive(serde::Deserialize)]
        struct GithubRelease {
            tag_name: String,
            published_at: Option<chrono::DateTime<chrono::Utc>>,
        }

        let releases: Vec<GithubRelease> = serde_json::from_value(res)?;
        let mut history = Vec::new();

        fn coerce_version(ver_str: &str) -> Option<semver::Version> {
            let s = ver_str.trim_start_matches(['v', '^', '~', '=']);
            semver::Version::parse(s).ok()
        }

        let old_ver =
            coerce_version(clean_current).unwrap_or_else(|| semver::Version::new(0, 0, 0));
        let new_ver = coerce_version(clean_new).unwrap_or_else(|| semver::Version::new(0, 0, 0));

        for rel in releases {
            let ver_str = rel.tag_name.trim_start_matches('v');
            if let Some(ver) = coerce_version(ver_str) {
                if ver >= old_ver && ver <= new_ver {
                    history.push(crate::core::engine::Release {
                        version: rel.tag_name.clone(),
                        publish_time: rel.published_at.unwrap_or_else(chrono::Utc::now),
                    });
                }
            }
        }

        history.sort_by_key(|r| std::cmp::Reverse(r.publish_time));

        Ok(history)
    }

    pub async fn get_package_info(&self, name: &str) -> Result<crate::core::engine::PackageInfo> {
        let parts: Vec<&str> = name.split('/').collect();
        if parts.len() < 2 {
            return Ok(crate::core::engine::PackageInfo { repo_url: None });
        }
        let owner = parts[0];
        let repo = parts[1];

        let url = format!("https://github.com/{}/{}", owner, repo);
        Ok(crate::core::engine::PackageInfo {
            repo_url: Some(url),
        })
    }

    pub async fn project_repository_view(
        &self,
        owner: String,
        repo: String,
    ) -> Result<GitHubProjectRepositoryView> {
        let octocrab = self
            .installation_client(&owner)
            .await?
            .with_context(|| format!("GitHub App is not installed for owner '{}'", owner))?;

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
        let view = self.project_repository_view(owner, repo).await?;
        Ok(GitHubProjectRepositoryMutator {
            octocrab: view.octocrab,
            owner: view.owner,
            repo: view.repo,
        })
    }

    pub async fn get_json(&self, route: &str) -> Result<serde_json::Value> {
        let client = match Self::parse_owner_from_route(route) {
            Some(owner) => self.client_for_owner(&owner).await?,
            None => self.public_client.clone(),
        };

        match client
            .get::<serde_json::Value, _, _>(route, None::<&()>)
            .await
        {
            Ok(v) => Ok(v),
            Err(octocrab::Error::GitHub { source, .. }) => {
                if source.message.contains("Not Found")
                    || source.status_code == reqwest::StatusCode::NOT_FOUND
                {
                    anyhow::bail!("404 Not Found")
                }
                anyhow::bail!("GitHub error: {}", source.message)
            }
            Err(e) => anyhow::bail!("Request error: {}", e),
        }
    }

    /// Raw GET with installation-resolved auth, preserving the status, the
    /// headers and the body. Unlike [`Self::get_json`] it does not coerce
    /// error statuses — classification is left to the caller (the
    /// release-notes crate maps 404 → miss, 403/429 → rate limit).
    ///
    /// `owner` selects the installation-aware client explicitly instead of
    /// being derived from the route: GitHub canonicalizes `Link` pagination
    /// targets to ownerless `/repositories/{id}/…` routes, which route
    /// parsing cannot map back to an owner.
    pub async fn get_response(&self, owner: &str, route: &str) -> Result<GitHubHttpResponse> {
        let client = self.client_for_owner(owner).await?;

        let response = client._get(route).await.context("GitHub request failed")?;

        let status = response.status().as_u16();
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| {
                let value = value.to_str().ok()?;
                Some((name.as_str().to_string(), value.to_string()))
            })
            .collect::<Vec<_>>();
        let body = client
            .body_to_string(response)
            .await
            .context("GitHub response body read failed")?;

        Ok(GitHubHttpResponse {
            status,
            headers,
            body,
        })
    }

    /// Extracts the owner from a `/repos/{owner}/{repo}/...` route.
    fn parse_owner_from_route(route: &str) -> Option<String> {
        let mut parts = route.trim_start_matches('/').split('/');
        if parts.next()? != "repos" {
            return None;
        }
        parts.next().map(|s| s.to_string())
    }

    /// Reads `path` of another repository, at `reference` (a branch, tag or commit) or at its
    /// default branch.
    pub async fn get_foreign_file(
        &self,
        owner: &str,
        repo: &str,
        path: &str,
        reference: Option<&str>,
    ) -> Result<Option<String>> {
        let route = match reference {
            Some(r) => format!(
                "/repos/{}/{}/contents/{}?ref={}",
                owner,
                repo,
                path,
                urlencoding::encode(r)
            ),
            None => format!("/repos/{}/{}/contents/{}", owner, repo, path),
        };
        let response = match self.get_json(&route).await {
            Ok(res) => res,
            Err(e) => {
                let err_str = e.to_string().to_lowercase();
                if err_str.contains("rate limit")
                    || err_str.contains("forbidden")
                    || err_str.contains("403")
                {
                    return Err(anyhow::anyhow!("GitHub API rate limit exceeded (403 Forbidden) fetching file via authenticated client"));
                }
                return Ok(None);
            }
        };

        if let Some(content_base64) = response.get("content").and_then(|c| c.as_str()) {
            let content_cleaned = content_base64.replace('\n', "");
            let decoded = general_purpose::STANDARD.decode(content_cleaned)?;
            let text = String::from_utf8(decoded)?;
            Ok(Some(text))
        } else {
            Ok(None)
        }
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

    async fn list_branches(&self, prefix: &str) -> Result<Vec<String>> {
        // Not paginated: the endpoint ignores `per_page` and `page`, and returns every match.
        let url = format!(
            "/repos/{}/{}/git/matching-refs/heads/{}",
            self.owner, self.repo, prefix
        );
        let refs: Vec<Value> = self.octocrab.get(&url, None::<&()>).await?;

        Ok(refs
            .iter()
            .filter_map(|r| r["ref"].as_str())
            .filter_map(|r| r.strip_prefix("refs/heads/"))
            .map(str::to_string)
            .collect())
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

    async fn export(&self, dir: &Path, max_bytes: usize) -> Result<bool> {
        let response = self
            .octocrab
            .repos(&self.owner, &self.repo)
            .download_tarball(self.revision.clone())
            .await?;
        if !response.status().is_success() {
            anyhow::bail!("Downloading the repository failed: {}", response.status());
        }

        // The archive is smaller than what is in it, so the same limit holds it comfortably.
        let mut body = response.into_body();
        let mut archive = Vec::new();
        while let Some(frame) = body.frame().await {
            if let Ok(data) = frame?.into_data() {
                archive.extend_from_slice(&data);
            }
            if archive.len() > max_bytes {
                anyhow::bail!(
                    "The repository is larger than the {max_bytes} bytes that are put on disk"
                );
            }
        }

        let dir = dir.to_path_buf();
        tokio::task::spawn_blocking(move || tarball::unpack(archive.as_slice(), &dir, max_bytes))
            .await??;
        Ok(true)
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

        let base_tree_sha = self.get_commit_tree_sha(base_revision).await?;
        let new_tree_sha = self.create_tree(&base_tree_sha, tree_items).await?;

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
        let url = format!(
            "/repos/{}/{}/pulls?head={}&state=open",
            self.owner, self.repo, head_param
        );
        let prs_response: Value = self.octocrab.get(url, None::<&()>).await?;

        if let Some(existing_pr) = prs_response.as_array().and_then(|arr| arr.first()) {
            if let Some(pr_number) = existing_pr["number"].as_u64() {
                let patch_url = format!("/repos/{}/{}/pulls/{}", self.owner, self.repo, pr_number);
                let updated_pr: Value = self
                    .octocrab
                    .patch(
                        patch_url,
                        Some(&serde_json::json!({
                            "title": title,
                            "body": body
                        })),
                    )
                    .await?;

                return Ok(updated_pr["html_url"].as_str().unwrap_or("").to_string());
            }
        }

        let create_url = format!("/repos/{}/{}/pulls", self.owner, self.repo);
        let created_pr: Value = self
            .octocrab
            .post(
                create_url,
                Some(&serde_json::json!({
                    "title": title,
                    "head": branch_name,
                    "base": base_branch,
                    "body": body
                })),
            )
            .await?;

        Ok(created_pr["html_url"].as_str().unwrap_or("").to_string())
    }

    async fn close_pull_request(&self, branch_name: &str, base_branch: &str) -> Result<()> {
        let head_param = format!("{}:{}", self.owner, branch_name);
        let url = format!(
            "/repos/{}/{}/pulls?head={}&base={}&state=open",
            self.owner, self.repo, head_param, base_branch
        );
        let prs_response: Value = self.octocrab.get(url, None::<&()>).await?;

        if let Some(existing_pr) = prs_response.as_array().and_then(|arr| arr.first()) {
            if let Some(pr_number) = existing_pr["number"].as_u64() {
                let patch_url = format!("/repos/{}/{}/pulls/{}", self.owner, self.repo, pr_number);
                let _: Value = self
                    .octocrab
                    .patch(
                        patch_url,
                        Some(&serde_json::json!({
                            "state": "closed"
                        })),
                    )
                    .await?;
                tracing::info!("Closed empty PR #{}", pr_number);
            }
        }

        let _ = self.delete_branch(branch_name).await;

        Ok(())
    }
}
