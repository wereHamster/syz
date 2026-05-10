use crate::core::engine::repository::{
    FileModification, FileState, ProjectRepositoryMutator, ProjectRepositorySnapshot,
    ProjectRepositoryView,
};
use anyhow::Result;
use async_trait::async_trait;
use atrium_api::agent::atp_agent::{store::MemorySessionStore, AtpAgent};
use atrium_xrpc_client::reqwest::ReqwestClient;
use flate2::write::GzEncoder;
use flate2::Compression;
use std::env;
use std::sync::Arc;
use tempfile::TempDir;

#[derive(Clone)]
pub struct Tangled {
    agent: Arc<AtpAgent<MemorySessionStore, ReqwestClient>>,
}

impl Tangled {
    pub async fn new() -> Result<Self> {
        let handle = env::var("ATPROTO_HANDLE").unwrap_or_default();
        let password = env::var("ATPROTO_PASSWORD").unwrap_or_default();
        let pds_url = env::var("ATPROTO_PDS").unwrap_or_else(|_| "https://bsky.social".to_string());

        let client = ReqwestClient::new(pds_url);
        let session_store = MemorySessionStore::default();
        let agent = AtpAgent::new(client, session_store);

        if !handle.is_empty() && !password.is_empty() {
            agent.login(&handle, &password).await?;
        }

        Ok(Self {
            agent: Arc::new(agent),
        })
    }

    async fn resolve_repo(
        &self,
        owner_handle: &str,
        repo_name: &str,
    ) -> Result<(String, String, String)> {
        // Resolve handle to DID
        let resolve_params = atrium_api::com::atproto::identity::resolve_handle::ParametersData {
            handle: owner_handle
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid handle string"))?,
        };

        let owner_did = self
            .agent
            .api
            .com
            .atproto
            .identity
            .resolve_handle(resolve_params.into())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to resolve handle: {:?}", e))?
            .did
            .clone();

        // Search for repo name in repo collections
        let list_params = atrium_api::com::atproto::repo::list_records::ParametersData {
            collection: "sh.tangled.repo".parse().unwrap(),
            cursor: None,
            limit: None,
            repo: owner_did
                .as_str()
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid DID string"))?,
            reverse: None,
        };

        let mut found_rkey = None;
        let mut found_uri = None;
        let repos = self
            .agent
            .api
            .com
            .atproto
            .repo
            .list_records(list_params.into())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to list tangled repos: {:?}", e))?;

        for record in &repos.records {
            if let atrium_api::types::Unknown::Object(obj) = &record.value {
                if let Some(val) = obj.get("name") {
                    if let ipld_core::ipld::Ipld::String(name) = &**val {
                        if name == repo_name {
                            found_rkey =
                                Some(record.uri.split('/').last().unwrap_or("").to_string());
                            found_uri = Some(record.uri.clone());
                            break;
                        }
                    }
                }
            }
        }

        let rkey = found_rkey
            .ok_or_else(|| anyhow::anyhow!("Could not find repo named '{}'", repo_name))?;
        let repo_at_uri = found_uri.unwrap();

        // Fetch repo metadata to get knot URL and repoDid
        let params = atrium_api::com::atproto::repo::get_record::ParametersData {
            collection: "sh.tangled.repo".parse().unwrap(),
            repo: owner_did
                .as_str()
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid DID string"))?,
            rkey: rkey
                .parse()
                .map_err(|_| anyhow::anyhow!("Invalid rkey string"))?,
            cid: None,
        };

        let response = self
            .agent
            .api
            .com
            .atproto
            .repo
            .get_record(params.into())
            .await
            .map_err(|e| anyhow::anyhow!("Failed to fetch repo metadata: {:?}", e))?;

        let mut knot_url = String::new();
        let mut repo_did = String::new();

        if let atrium_api::types::Unknown::Object(obj) = &response.value {
            if let Some(val) = obj.get("knot") {
                if let ipld_core::ipld::Ipld::String(knot) = &**val {
                    knot_url = format!("https://{}", knot);
                }
            }
            if let Some(val) = obj.get("repoDid") {
                if let ipld_core::ipld::Ipld::String(did) = &**val {
                    repo_did = did.to_string();
                }
            }
        }

        if knot_url.is_empty() || repo_did.is_empty() {
            return Err(anyhow::anyhow!("Repo metadata missing knot or repoDid"));
        }

        Ok((repo_did, repo_at_uri, knot_url))
    }

    pub async fn project_repository_view(
        &self,
        owner: String,
        repo: String,
    ) -> Result<TangledProjectRepositoryView> {
        let (repo_did, repo_at_uri, knot_url) = self.resolve_repo(&owner, &repo).await?;
        Ok(TangledProjectRepositoryView {
            agent: self.agent.clone(),
            owner_handle: owner,
            repo_name: repo,
            repo_did,
            repo_at_uri,
            knot_url,
        })
    }

    pub async fn project_repository_mutator(
        &self,
        owner: String,
        repo: String,
    ) -> Result<TangledProjectRepositoryMutator> {
        let (repo_did, repo_at_uri, knot_url) = self.resolve_repo(&owner, &repo).await?;
        Ok(TangledProjectRepositoryMutator {
            agent: self.agent.clone(),
            owner_handle: owner,
            repo_name: repo,
            repo_did,
            repo_at_uri,
            knot_url,
        })
    }
}

pub struct TangledProjectRepositoryView {
    agent: Arc<AtpAgent<MemorySessionStore, ReqwestClient>>,
    owner_handle: String,
    repo_name: String,
    repo_did: String,
    repo_at_uri: String,
    knot_url: String,
}

impl TangledProjectRepositoryView {
    pub async fn get_tree(
        &self,
        path: Option<&str>,
        git_ref: Option<&str>,
    ) -> Result<serde_json::Value> {
        let mut query = String::new();
        query.push_str(&format!("?repo={}", urlencoding::encode(&self.repo_did)));
        if let Some(p) = path {
            query.push_str(&format!("&path={}", urlencoding::encode(p)));
        }
        if let Some(r) = git_ref {
            query.push_str(&format!("&ref={}", urlencoding::encode(r)));
        } else {
            query.push_str("&ref=HEAD");
        }

        let full_url = format!("{}/xrpc/sh.tangled.repo.tree{}", self.knot_url, query);

        let session = self.agent.get_session().await;
        let client = reqwest::Client::new();
        let mut builder = client.get(&full_url);
        if let Some(s) = session {
            builder = builder.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", s.access_jwt),
            );
        }

        let response = builder.send().await?;
        if response.status().is_success() {
            Ok(response.json().await?)
        } else {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to fetch tree: HTTP {} for {}\nBody: {}",
                status,
                full_url,
                error_body
            ))
        }
    }

    pub async fn get_blob(&self, path: &str, git_ref: Option<&str>) -> Result<Vec<u8>> {
        let mut query = String::new();
        query.push_str(&format!("?repo={}", urlencoding::encode(&self.repo_did)));
        query.push_str(&format!("&path={}", urlencoding::encode(path)));
        query.push_str("&raw=true");
        if let Some(r) = git_ref {
            query.push_str(&format!("&ref={}", urlencoding::encode(r)));
        } else {
            query.push_str("&ref=HEAD");
        }

        let full_url = format!("{}/xrpc/sh.tangled.repo.blob{}", self.knot_url, query);

        let session = self.agent.get_session().await;
        let client = reqwest::Client::new();
        let mut builder = client.get(&full_url);
        if let Some(s) = session {
            builder = builder.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", s.access_jwt),
            );
        }

        let response = builder.send().await?;
        if response.status().is_success() {
            Ok(response.bytes().await?.to_vec())
        } else {
            let status = response.status();
            let error_body = response.text().await.unwrap_or_default();
            Err(anyhow::anyhow!(
                "Failed to fetch blob: HTTP {} for {}\nBody: {}",
                status,
                full_url,
                error_body
            ))
        }
    }
}

#[async_trait]
impl ProjectRepositoryView for TangledProjectRepositoryView {
    async fn get_default_branch(&self) -> Result<String> {
        let full_url = format!(
            "{}/xrpc/sh.tangled.repo.getDefaultBranch?repo={}",
            self.knot_url,
            urlencoding::encode(&self.repo_did)
        );

        let session = self.agent.get_session().await;
        let client = reqwest::Client::new();
        let mut builder = client.get(&full_url);
        if let Some(s) = session {
            builder = builder.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", s.access_jwt),
            );
        }

        let response = builder.send().await?;
        if response.status().is_success() {
            let json: serde_json::Value = response.json().await?;
            if let Some(name) = json.get("name").and_then(|n| n.as_str()) {
                Ok(name.to_string())
            } else {
                Err(anyhow::anyhow!(
                    "Invalid response from getDefaultBranch: {:?}",
                    json
                ))
            }
        } else {
            Ok("main".to_string())
        }
    }

    async fn get_revision(&self, branch_name: &str) -> Result<String> {
        let mut query = String::new();
        query.push_str(&format!("?repo={}", urlencoding::encode(&self.repo_did)));
        query.push_str(&format!("&name={}", urlencoding::encode(branch_name)));

        let full_url = format!("{}/xrpc/sh.tangled.repo.branch{}", self.knot_url, query);

        let session = self.agent.get_session().await;
        let client = reqwest::Client::new();
        let mut builder = client.get(&full_url);
        if let Some(s) = session {
            builder = builder.header(
                reqwest::header::AUTHORIZATION,
                format!("Bearer {}", s.access_jwt),
            );
        }

        let response = builder.send().await?;
        if response.status().is_success() {
            let json: serde_json::Value = response.json().await?;
            if let Some(hash) = json.get("hash").and_then(|h| h.as_str()) {
                Ok(hash.to_string())
            } else {
                Err(anyhow::anyhow!("Branch not found"))
            }
        } else {
            Err(anyhow::anyhow!("Branch not found"))
        }
    }

    fn snapshot(&self, revision: &str) -> Box<dyn ProjectRepositorySnapshot> {
        Box::new(TangledProjectRepositorySnapshot {
            view: Arc::new(self.clone()),
            revision: revision.to_string(),
        })
    }
}

// Implement Clone manually because of agent
impl Clone for TangledProjectRepositoryView {
    fn clone(&self) -> Self {
        Self {
            agent: self.agent.clone(),
            owner_handle: self.owner_handle.clone(),
            repo_name: self.repo_name.clone(),
            repo_did: self.repo_did.clone(),
            repo_at_uri: self.repo_at_uri.clone(),
            knot_url: self.knot_url.clone(),
        }
    }
}

pub struct TangledProjectRepositorySnapshot {
    view: Arc<TangledProjectRepositoryView>,
    revision: String,
}

#[async_trait]
impl ProjectRepositorySnapshot for TangledProjectRepositorySnapshot {
    async fn list_files(&self) -> Result<Vec<String>> {
        let mut all_files = Vec::new();
        let mut dirs_to_process = vec![String::new()];

        while let Some(current_path) = dirs_to_process.pop() {
            let path_opt = if current_path.is_empty() {
                None
            } else {
                Some(current_path.as_str())
            };
            let tree = self.view.get_tree(path_opt, Some(&self.revision)).await?;

            if let Some(files) = tree.get("files").and_then(|f| f.as_array()) {
                for file in files {
                    if let Some(name) = file.get("name").and_then(|n| n.as_str()) {
                        let full_path = if current_path.is_empty() {
                            name.to_string()
                        } else {
                            format!("{}/{}", current_path, name)
                        };

                        if let Some(mode) = file.get("mode").and_then(|m| m.as_str()) {
                            if mode == "0040000"
                                || mode == "040000"
                                || mode == "40000"
                                || mode == "dir"
                                || mode == "d"
                            {
                                dirs_to_process.push(full_path);
                            } else {
                                all_files.push(full_path);
                            }
                        } else {
                            all_files.push(full_path);
                        }
                    }
                }
            }
        }
        Ok(all_files)
    }

    async fn read_file(&self, path: &str) -> Result<String> {
        match self.view.get_blob(path, Some(&self.revision)).await {
            Ok(bytes) => Ok(String::from_utf8_lossy(&bytes).to_string()),
            Err(e) => {
                if e.to_string().contains("HTTP 404") {
                    Err(anyhow::anyhow!("File not found"))
                } else {
                    Err(e)
                }
            }
        }
    }
}

pub struct TangledProjectRepositoryMutator {
    agent: Arc<AtpAgent<MemorySessionStore, ReqwestClient>>,
    owner_handle: String,
    repo_name: String,
    repo_did: String,
    repo_at_uri: String,
    knot_url: String,
}

impl TangledProjectRepositoryMutator {
    fn clone_repo(&self, temp_dir: &TempDir) -> Result<git2::Repository> {
        let mut fetch_options = git2::FetchOptions::new();
        let mut callbacks = git2::RemoteCallbacks::new();

        callbacks.credentials(|_url, username_from_url, _allowed_types| {
            git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
        });

        fetch_options.remote_callbacks(callbacks);

        let mut builder = git2::build::RepoBuilder::new();
        builder.fetch_options(fetch_options);

        let knot_domain = self.knot_url.replace("https://", "").replace("http://", "");
        let clone_url = format!("git@{}:{}", knot_domain, self.repo_did);

        builder
            .clone(&clone_url, temp_dir.path())
            .map_err(|e| anyhow::anyhow!("Failed to clone Tangled repo via SSH: {}", e))
    }
}

#[async_trait]
impl ProjectRepositoryMutator for TangledProjectRepositoryMutator {
    async fn commit_changes(
        &self,
        base_revision: &str,
        new_branch: &str,
        commit_message: &str,
        modifications: Vec<FileModification>,
    ) -> Result<Option<String>> {
        let temp_dir = tempfile::Builder::new().prefix("syz-tangled-").tempdir()?;
        let repo = self.clone_repo(&temp_dir)?;

        let obj = repo.revparse_single(base_revision)?;
        repo.checkout_tree(&obj, None)?;
        repo.set_head_detached(obj.id())?;

        let commit = obj.peel_to_commit()?;
        let branch = repo.branch(new_branch, &commit, false)?;
        repo.set_head(branch.get().name().unwrap())?;

        let repo_path = repo.workdir().unwrap();

        let mut changed_any = false;
        for modif in modifications {
            let file_path = repo_path.join(&modif.path);
            match modif.state {
                FileState::Write(content) => {
                    if let Some(parent) = file_path.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    let current_content = std::fs::read_to_string(&file_path).unwrap_or_default();
                    if current_content != content {
                        std::fs::write(&file_path, &content)?;
                        changed_any = true;
                    }
                }
                FileState::Delete => {
                    if file_path.exists() {
                        std::fs::remove_file(&file_path)?;
                        changed_any = true;
                    }
                }
            }
        }

        if !changed_any {
            return Ok(None);
        }

        let mut index = repo.index()?;
        index.add_all(["*"].iter(), git2::IndexAddOption::DEFAULT, None)?;
        index.write()?;
        let tree_id = index.write_tree()?;
        let tree = repo.find_tree(tree_id)?;

        let signature = git2::Signature::now("syz", "bot@syz.app")?;
        let head = repo.head()?;
        let parent_commit = repo.find_commit(head.target().unwrap())?;

        let commit_id = repo.commit(
            Some("HEAD"),
            &signature,
            &signature,
            commit_message,
            &tree,
            &[&parent_commit],
        )?;

        let mut remote = repo.find_remote("origin")?;
        let mut push_options = git2::PushOptions::new();
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(|_url, username_from_url, _allowed_types| {
            git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
        });
        push_options.remote_callbacks(callbacks);

        let refspec = format!("+refs/heads/{}:refs/heads/{}", new_branch, new_branch);
        remote.push(&[&refspec], Some(&mut push_options))?;

        Ok(Some(commit_id.to_string()))
    }

    async fn create_pull_request(
        &self,
        title: &str,
        head_branch: &str,
        base_branch: &str,
        body: &str,
    ) -> Result<String> {
        let user_did = self
            .agent
            .get_session()
            .await
            .map(|s| s.did.clone())
            .unwrap();

        let list_params = atrium_api::com::atproto::repo::list_records::ParametersData {
            collection: "sh.tangled.repo.pull".parse().unwrap(),
            cursor: None,
            limit: None,
            repo: user_did.parse().unwrap(),
            reverse: None,
        };

        let mut existing_pr_rkey = None;
        let mut existing_pr_record = None;

        if let Ok(existing_records) = self
            .agent
            .api
            .com
            .atproto
            .repo
            .list_records(list_params.into())
            .await
        {
            for record in &existing_records.records {
                if let atrium_api::types::Unknown::Object(obj) = &record.value {
                    let mut matches = false;

                    if let Some(target) = obj.get("target").and_then(|v| {
                        if let ipld_core::ipld::Ipld::Map(m) = &**v {
                            Some(m)
                        } else {
                            None
                        }
                    }) {
                        if let Some(repo_uri) = target.get("repo").and_then(|v| {
                            if let ipld_core::ipld::Ipld::String(s) = v {
                                Some(s)
                            } else {
                                None
                            }
                        }) {
                            if repo_uri == &self.repo_at_uri {
                                if let Some(source) = obj.get("source").and_then(|v| {
                                    if let ipld_core::ipld::Ipld::Map(m) = &**v {
                                        Some(m)
                                    } else {
                                        None
                                    }
                                }) {
                                    if let Some(branch) = source.get("branch").and_then(|v| {
                                        if let ipld_core::ipld::Ipld::String(s) = v {
                                            Some(s)
                                        } else {
                                            None
                                        }
                                    }) {
                                        if branch == head_branch {
                                            matches = true;
                                        }
                                    }
                                }
                            }
                        }
                    }

                    if matches {
                        existing_pr_rkey = Some(
                            record
                                .uri
                                .split('/')
                                .last()
                                .unwrap_or("unknown")
                                .to_string(),
                        );
                        existing_pr_record = Some(obj.clone());
                        break;
                    }
                }
            }
        }

        let patch_content = {
            let temp_dir = tempfile::Builder::new()
                .prefix("syz-tangled-diff-")
                .tempdir()?;
            let repo = self.clone_repo(&temp_dir)?;

            let base_obj = repo.revparse_single(&format!("origin/{}", base_branch))?;
            let head_obj = repo.revparse_single(&format!("origin/{}", head_branch))?;
            let base_commit = base_obj.peel_to_commit()?;
            let head_commit = head_obj.peel_to_commit()?;

            let base_tree = base_commit.tree()?;
            let head_tree = head_commit.tree()?;
            let diff = repo.diff_tree_to_tree(Some(&base_tree), Some(&head_tree), None)?;

            let mut diff_text = Vec::new();
            diff.print(git2::DiffFormat::Patch, |_delta, _hunk, line| {
                let origin = line.origin();
                if origin == '+' || origin == '-' || origin == ' ' {
                    diff_text.push(origin as u8);
                }
                diff_text.extend_from_slice(line.content());
                true
            })?;

            diff_text.extend_from_slice(b"--\n2.39.2\n");

            let author = head_commit.author();
            let author_name = author.name().unwrap_or("syz");
            let author_email = author.email().unwrap_or("bot@syz.app");
            let commit_msg = head_commit.message().unwrap_or("");

            let rfc2822_date = chrono::DateTime::from_timestamp(head_commit.time().seconds(), 0)
                .unwrap_or_default()
                .to_rfc2822();

            let mut commit_msg_lines = commit_msg.lines().collect::<Vec<_>>();
            let subject = if commit_msg_lines.is_empty() {
                ""
            } else {
                commit_msg_lines.remove(0)
            };
            let msg_body = commit_msg_lines.join("\n");

            let change_id = format!("Change-Id: I{}", head_commit.id());

            format!(
                "From {} Mon Sep 17 00:00:00 2001\nFrom: {} <{}>\nDate: {}\nSubject: [PATCH] {}\n\n{}\n\n{}\n---\n{}",
                head_commit.id(),
                author_name,
                author_email,
                rfc2822_date,
                subject,
                msg_body.trim(),
                change_id,
                String::from_utf8_lossy(&diff_text)
            )
        };

        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        use std::io::Write;
        encoder.write_all(patch_content.as_bytes())?;
        let compressed_patch = encoder.finish()?;

        let blob_res = self
            .agent
            .api
            .com
            .atproto
            .repo
            .upload_blob(compressed_patch)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to upload patch blob: {:?}", e))?;

        let patch_blob = blob_res.blob.clone();
        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);

        let new_round = serde_json::json!({
            "createdAt": now,
            "patchBlob": patch_blob
        });

        if let (Some(rkey), Some(record_obj)) = (existing_pr_rkey, existing_pr_record) {
            tracing::info!("Updating existing Pull Request for branch: {}", head_branch);

            let mut record_json: serde_json::Value = serde_json::to_value(record_obj)?;
            if let Some(rounds) = record_json.get_mut("rounds").and_then(|v| v.as_array_mut()) {
                rounds.push(new_round);
            }

            let put_params = atrium_api::com::atproto::repo::put_record::InputData {
                collection: "sh.tangled.repo.pull".parse().unwrap(),
                repo: user_did.parse().unwrap(),
                rkey: rkey.parse().unwrap(),
                record: atrium_api::types::Unknown::Object(
                    serde_json::from_value(record_json).unwrap(),
                ),
                swap_commit: None,
                swap_record: None,
                validate: None,
            };

            self.agent
                .api
                .com
                .atproto
                .repo
                .put_record(put_params.clone().into())
                .await
                .map_err(|e| anyhow::anyhow!("Failed to update PR record: {:?}", e))?;

            return Ok(format!(
                "https://tangled.org/{}/{}/pulls",
                self.owner_handle, self.repo_name
            ));
        }

        let record = serde_json::json!({
            "$type": "sh.tangled.repo.pull",
            "title": title,
            "body": body,
            "createdAt": now,
            "source": {
                "branch": head_branch,
            },
            "target": {
                "repo": self.repo_at_uri,
                "branch": base_branch,
            },
            "rounds": [new_round]
        });

        let create_params = atrium_api::com::atproto::repo::create_record::InputData {
            collection: "sh.tangled.repo.pull".parse().unwrap(),
            repo: user_did.parse().unwrap(),
            record: atrium_api::types::Unknown::Object(serde_json::from_value(record).unwrap()),
            rkey: None,
            swap_commit: None,
            validate: None,
        };

        match self
            .agent
            .api
            .com
            .atproto
            .repo
            .create_record(create_params.into())
            .await
        {
            Ok(res) => {
                tracing::info!("Successfully created Pull Request record: {}", res.uri);
                Ok(format!(
                    "https://tangled.org/{}/{}/pulls",
                    self.owner_handle, self.repo_name
                ))
            }
            Err(e) => Err(anyhow::anyhow!(
                "Failed to create pull request record: {:?}",
                e
            )),
        }
    }

    async fn close_pull_request(&self, head_branch: &str, _base_branch: &str) -> Result<()> {
        let temp_dir = tempfile::Builder::new()
            .prefix("syz-tangled-del-")
            .tempdir()?;
        let repo = self.clone_repo(&temp_dir)?;

        let mut remote = repo.find_remote("origin")?;
        let mut push_options = git2::PushOptions::new();
        let mut callbacks = git2::RemoteCallbacks::new();
        callbacks.credentials(|_url, username_from_url, _allowed_types| {
            git2::Cred::ssh_key_from_agent(username_from_url.unwrap_or("git"))
        });
        push_options.remote_callbacks(callbacks);

        let refspec = format!(":refs/heads/{}", head_branch);
        match remote.push(&[&refspec], Some(&mut push_options)) {
            Ok(_) => Ok(()),
            Err(e) => {
                tracing::warn!(
                    "Failed to delete branch '{}' on Tangled: {}",
                    head_branch,
                    e
                );
                Ok(())
            }
        }
    }
}
