use anyhow::Result;
use async_trait::async_trait;

use crate::core::clients;
use crate::core::engine::ecosystems::npm::internal::discover_project_dependencies::WorkspaceConfig;
use crate::core::engine::ecosystems::{Registry, Scanner};
use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::{DependencyUpdateOption, DiscoveredDependency};

pub mod internal;

use crate::core::engine::{repository::FileModification, UpdateTarget};
use std::fs;
use std::process::Command;

pub struct NpmScanner;

#[async_trait]
impl Scanner for NpmScanner {
    async fn discover_project_dependencies(
        &self,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<Vec<DiscoveredDependency>> {
        internal::discover_project_dependencies::run(repo).await
    }
}

pub struct NpmRegistry {
    npm_client: clients::npm::Npm,
}

impl NpmRegistry {
    pub fn new(npm_client: clients::npm::Npm) -> Self {
        Self { npm_client }
    }
}

#[async_trait]
impl Registry for NpmRegistry {
    async fn query_dependency_update_options(
        &self,
        dependency: &DiscoveredDependency,
    ) -> Result<DependencyUpdateOption> {
        internal::query_dependency_update_options::run(self.npm_client.clone(), dependency).await
    }

    async fn fetch_package_info(&self, name: &str) -> Result<crate::core::engine::PackageInfo> {
        self.npm_client.get_package_info(name).await
    }

    async fn fetch_release_history(
        &self,
        name: &str,
        current_version: &str,
        target_version: &str,
    ) -> Result<Vec<crate::core::engine::Release>> {
        self.npm_client
            .get_release_history(name, current_version, target_version)
            .await
    }
}

pub struct NpmPatcher {
    npm_client: clients::npm::Npm,
}

impl NpmPatcher {
    pub fn new(npm_client: clients::npm::Npm) -> Self {
        Self { npm_client }
    }
}

#[async_trait]
impl crate::core::engine::ecosystems::Patcher for NpmPatcher {
    fn updated_requirement(&self, old_req: &str, target_version: &str) -> Option<String> {
        let prefix = if old_req.starts_with('^') {
            "^"
        } else if old_req.starts_with('~') {
            "~"
        } else if old_req.starts_with('=') {
            "="
        } else if old_req.starts_with('v') {
            "v"
        } else {
            ""
        };
        let new_req = format!("{}{}", prefix, target_version);
        if old_req != new_req {
            Some(new_req)
        } else {
            None
        }
    }

    async fn apply_updates(
        &self,
        snapshot: &dyn ProjectRepositorySnapshot,
        temp_dir: &std::path::Path,
        targets: &[UpdateTarget],
    ) -> Result<Vec<FileModification>> {
        let workspace = snapshot.read_file("pnpm-workspace.yaml").await.ok();
        let lockfile = snapshot.read_file("pnpm-lock.yaml").await.ok();

        tracing::info!("Fetching repository tree for NPM updates...");
        let files = snapshot.list_files().await?;

        let mut pkg_jsons_to_fetch = Vec::new();
        for path in files {
            if path.ends_with("package.json") {
                pkg_jsons_to_fetch.push(path.to_string());
            }
        }

        let mut tree_items = Vec::new();

        for path in &pkg_jsons_to_fetch {
            if let Ok(content) = snapshot.read_file(path).await {
                let mut pkg_json: serde_json::Value = match serde_json::from_str(&content) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                let mut was_updated = false;

                for target in targets {
                    for key in ["dependencies", "devDependencies", "peerDependencies"] {
                        if let Some(deps) = pkg_json.get_mut(key).and_then(|d| d.as_object_mut()) {
                            if deps.contains_key(&target.name) {
                                deps.insert(
                                    target.name.to_string(),
                                    serde_json::Value::String(
                                        target.target_version.requirement.clone(),
                                    ),
                                );
                                was_updated = true;
                            }
                        }
                    }
                }

                let full_path = temp_dir.join(path);
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }

                if was_updated {
                    let updated_pkg_json_str = serde_json::to_string_pretty(&pkg_json)? + "\n";
                    fs::write(&full_path, &updated_pkg_json_str)?;
                    tree_items.push(FileModification {
                        path: path.clone(),
                        state: crate::core::engine::repository::FileState::Write(
                            updated_pkg_json_str,
                        ),
                    });
                } else {
                    fs::write(&full_path, content)?;
                }
            }
        }

        if let Some(ref w) = workspace {
            fs::write(temp_dir.join("pnpm-workspace.yaml"), w)?;
        }
        if let Some(ref l) = lockfile {
            fs::write(temp_dir.join("pnpm-lock.yaml"), l)?;
        }

        tracing::info!("Running pnpm install in temp directory to update lockfile...");
        if !run_pnpm_install(temp_dir.to_path_buf(), workspace.is_some(), true, false).await? {
            tracing::info!("Fallback: Running without --lockfile-only...");
            if !run_pnpm_install(temp_dir.to_path_buf(), workspace.is_some(), false, false).await? {
                anyhow::bail!("pnpm install failed");
            }
        }

        tracing::info!("Running pnpm dedupe...");
        if !run_pnpm_dedupe(temp_dir.to_path_buf(), false).await? {
            tracing::warn!("pnpm dedupe failed, continuing anyway...");
        }

        let updated_lock = fs::read_to_string(temp_dir.join("pnpm-lock.yaml")).ok();

        if let Some(lock) = updated_lock {
            let old_lock = lockfile.unwrap_or_default();
            if lock != old_lock {
                tree_items.push(FileModification {
                    path: "pnpm-lock.yaml".to_string(),
                    state: crate::core::engine::repository::FileState::Write(lock),
                });
            }
        }

        Ok(tree_items)
    }

    async fn update_transitive_dependencies(
        &self,
        snapshot: &dyn ProjectRepositorySnapshot,
        temp_dir: &std::path::Path,
    ) -> Result<Option<crate::core::engine::TransitiveUpdateResult>> {
        let workspace = snapshot.read_file("pnpm-workspace.yaml").await.ok();
        let lockfile = snapshot.read_file("pnpm-lock.yaml").await.ok();

        if lockfile.is_none() {
            tracing::info!(
                "No pnpm-lock.yaml found. Transitive bump currently only supports pnpm."
            );
            return Ok(None);
        }

        tracing::info!("Fetching repository tree...");
        let files = snapshot.list_files().await?;

        let mut pkg_json_paths_vec = Vec::new();
        for path in &files {
            if path.ends_with("package.json") {
                pkg_json_paths_vec.push(path.to_string());
            }
        }

        if pkg_json_paths_vec.is_empty() {
            tracing::info!("No package.json found.");
            return Ok(None);
        }

        let mut original_pkg_jsons = std::collections::HashMap::new();
        let mut mutable_pkg_jsons = std::collections::HashMap::new();

        for path in &pkg_json_paths_vec {
            if let Ok(content) = snapshot.read_file(path).await {
                original_pkg_jsons.insert(path.clone(), content.clone());
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                    mutable_pkg_jsons.insert(path.clone(), val);
                }
            }
        }

        if let Some(ref w) = workspace {
            fs::write(temp_dir.join("pnpm-workspace.yaml"), w)?;
        }
        if let Some(ref l) = lockfile {
            fs::write(temp_dir.join("pnpm-lock.yaml"), l)?;
        }

        let mut project_exact_versions: std::collections::HashMap<
            String,
            std::collections::HashMap<String, String>,
        > = std::collections::HashMap::new();
        if let Ok(lock_val) = serde_yml::from_str::<serde_yml::Value>(lockfile.as_ref().unwrap()) {
            if let Some(importers) = lock_val.get("importers").and_then(|i| i.as_mapping()) {
                for (k, v) in importers {
                    if let Some(project_path) = k.as_str() {
                        let mut deps = std::collections::HashMap::new();
                        for dep_type in ["dependencies", "devDependencies", "optionalDependencies"]
                        {
                            if let Some(dep_map) = v.get(dep_type).and_then(|d| d.as_mapping()) {
                                for (pkg_name, pkg_info) in dep_map {
                                    if let (Some(name), Some(info)) =
                                        (pkg_name.as_str(), pkg_info.as_mapping())
                                    {
                                        if let Some(version_val) = info
                                            .get(serde_yml::Value::String("version".to_string()))
                                        {
                                            if let Some(version_str) = version_val.as_str() {
                                                let clean_version = version_str
                                                    .split('(')
                                                    .next()
                                                    .unwrap_or("")
                                                    .to_string();
                                                deps.insert(name.to_string(), clean_version);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        project_exact_versions.insert(project_path.to_string(), deps);
                    }
                }
            }
        }

        let is_workspace = workspace.is_some();

        tracing::info!("Phase 1: Removing all dependencies from package.json files");
        for (path, pkg_json) in mutable_pkg_jsons.iter_mut() {
            if let Some(obj) = pkg_json.as_object_mut() {
                obj.remove("dependencies");
                obj.remove("devDependencies");
                obj.remove("optionalDependencies");
                obj.remove("peerDependencies");
            }
            let full_path = temp_dir.join(path);
            if let Some(parent) = full_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::write(&full_path, serde_json::to_string_pretty(pkg_json)? + "\n")?;
        }

        run_pnpm_install(temp_dir.to_path_buf(), is_workspace, true, true).await?;

        tracing::info!("Phase 2: Adding dependencies with exact versions");
        for (path, pkg_json) in mutable_pkg_jsons.iter_mut() {
            let original_content = original_pkg_jsons.get(path).unwrap();
            let original_val: serde_json::Value = serde_json::from_str(original_content).unwrap();

            let project_key = if path == "package.json" {
                ".".to_string()
            } else {
                path.strip_suffix("/package.json")
                    .unwrap_or(path)
                    .to_string()
            };

            let exact_deps = project_exact_versions
                .get(&project_key)
                .cloned()
                .unwrap_or_default();

            if let Some(obj) = pkg_json.as_object_mut() {
                for dep_type in ["dependencies", "devDependencies", "optionalDependencies"] {
                    if let Some(orig_deps) = original_val.get(dep_type).and_then(|d| d.as_object())
                    {
                        let mut new_deps = serde_json::Map::new();
                        for (pkg_name, _) in orig_deps {
                            if let Some(exact_ver) = exact_deps.get(pkg_name) {
                                new_deps.insert(
                                    pkg_name.clone(),
                                    serde_json::Value::String(exact_ver.clone()),
                                );
                            } else {
                                new_deps.insert(
                                    pkg_name.clone(),
                                    orig_deps.get(pkg_name).unwrap().clone(),
                                );
                            }
                        }
                        if !new_deps.is_empty() {
                            obj.insert(dep_type.to_string(), serde_json::Value::Object(new_deps));
                        }
                    }
                }
            }

            let full_path = temp_dir.join(path);
            fs::write(&full_path, serde_json::to_string_pretty(pkg_json)? + "\n")?;
        }

        run_pnpm_install(temp_dir.to_path_buf(), is_workspace, true, true).await?;

        tracing::info!("Phase 3: Restoring original package.json specs");
        for (path, original_content) in &original_pkg_jsons {
            let full_path = temp_dir.join(path);
            fs::write(&full_path, original_content)?;
        }

        run_pnpm_install(temp_dir.to_path_buf(), is_workspace, false, false).await?;

        tracing::info!("Running pnpm dedupe to consolidate transitive dependencies...");
        if !run_pnpm_dedupe(temp_dir.to_path_buf(), false).await? {
            tracing::warn!("pnpm dedupe failed, continuing anyway...");
        }

        let new_lockfile = fs::read_to_string(temp_dir.join("pnpm-lock.yaml"))?;
        let lockfile_content = lockfile.unwrap();
        if new_lockfile == lockfile_content {
            tracing::info!("No transitive dependencies needed updating.");
            return Ok(None);
        }

        tracing::info!("Transitive updates found!");

        let old_versions = extract_versions_from_lock(&lockfile_content);
        let new_versions = extract_versions_from_lock(&new_lockfile);

        let mut added = Vec::new();
        let mut removed = Vec::new();
        let mut major_bumps = Vec::new();
        let mut minor_bumps = Vec::new();

        let all_modules: std::collections::HashSet<String> = old_versions
            .keys()
            .chain(new_versions.keys())
            .cloned()
            .collect();

        for module in all_modules {
            let old_set = old_versions.get(&module);
            let new_set = new_versions.get(&module);

            match (old_set, new_set) {
                (None, Some(new_vers)) => {
                    let vers: Vec<_> = new_vers.iter().cloned().collect();
                    added.push((module, vers.join(", ")));
                }
                (Some(old_vers), None) => {
                    let vers: Vec<_> = old_vers.iter().cloned().collect();
                    removed.push((module, vers.join(", ")));
                }
                (Some(old_vers), Some(new_vers)) if old_vers != new_vers => {
                    let mut old_sorted: Vec<_> = old_vers.iter().cloned().collect();
                    let mut new_sorted: Vec<_> = new_vers.iter().cloned().collect();
                    old_sorted.sort();
                    new_sorted.sort();

                    let mut old_majors = std::collections::HashSet::new();
                    for o in &old_sorted {
                        if let Ok(ver) = semver::Version::parse(o) {
                            if ver.major == 0 {
                                old_majors.insert((0, ver.minor));
                            } else {
                                old_majors.insert((ver.major, 0));
                            }
                        }
                    }

                    let mut is_major = false;
                    for n in &new_sorted {
                        if let Ok(ver) = semver::Version::parse(n) {
                            let major_key = if ver.major == 0 {
                                (0, ver.minor)
                            } else {
                                (ver.major, 0)
                            };

                            if !old_majors.contains(&major_key) {
                                is_major = true;
                                break;
                            }
                        }
                    }

                    let label =
                        format!("`{}` -> `{}`", old_sorted.join(", "), new_sorted.join(", "));
                    if is_major {
                        major_bumps.push((module, label));
                    } else {
                        minor_bumps.push((module, label));
                    }
                }
                _ => {}
            }
        }

        added.sort_by(|a, b| a.0.cmp(&b.0));
        removed.sort_by(|a, b| a.0.cmp(&b.0));
        major_bumps.sort_by(|a, b| a.0.cmp(&b.0));
        minor_bumps.sort_by(|a, b| a.0.cmp(&b.0));

        let modifications = vec![FileModification {
            path: "pnpm-lock.yaml".to_string(),
            state: crate::core::engine::repository::FileState::Write(new_lockfile),
        }];

        let summary = crate::core::engine::TransitiveUpdateSummary {
            added,
            removed,
            major_bumps,
            minor_bumps,
            resolved_advisories: std::collections::HashMap::new(),
        };

        Ok(Some(crate::core::engine::TransitiveUpdateResult {
            modifications,
            summary,
        }))
    }

    async fn update_vulnerable_dependencies(
        &self,
        snapshot: &dyn ProjectRepositorySnapshot,
        temp_dir: &std::path::Path,
    ) -> Result<Option<crate::core::engine::advisories::SecurityUpdateResult>> {
        let workspace = snapshot.read_file("pnpm-workspace.yaml").await.ok();
        let lockfile = snapshot.read_file("pnpm-lock.yaml").await.ok();

        let mut minimum_release_age = None;
        if let Some(ref w_content) = workspace {
            if let Ok(config) = serde_yml::from_str::<WorkspaceConfig>(w_content) {
                if let Some(age) = config.minimum_release_age {
                    minimum_release_age = Some(chrono::Duration::minutes(age));
                }
            }
        }

        tracing::info!("Fetching repository tree for NPM audit...");
        let files = snapshot.list_files().await?;

        let mut pkg_json_paths_vec = Vec::new();
        for path in &files {
            if path.ends_with("package.json") {
                pkg_json_paths_vec.push(path.to_string());
            }
        }

        if pkg_json_paths_vec.is_empty() {
            return Ok(None);
        }

        let mut original_pkg_jsons = std::collections::HashMap::new();
        let mut mutable_pkg_jsons = std::collections::HashMap::new();

        for path in &pkg_json_paths_vec {
            if let Ok(content) = snapshot.read_file(path).await {
                original_pkg_jsons.insert(path.clone(), content.clone());
                if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                    mutable_pkg_jsons.insert(path.clone(), val);
                }
                let full_path = temp_dir.join(path);
                if let Some(parent) = full_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::write(&full_path, content)?;
            }
        }

        if let Some(ref w) = workspace {
            fs::write(temp_dir.join("pnpm-workspace.yaml"), w)?;
        }
        if let Some(ref l) = lockfile {
            fs::write(temp_dir.join("pnpm-lock.yaml"), l)?;
        }

        tracing::info!("Running pnpm install --lockfile-only to set baseline...");
        if !run_pnpm_install(temp_dir.to_path_buf(), workspace.is_some(), true, false).await? {
            tracing::warn!("pnpm install failed, continuing anyway...");
        }

        tracing::info!("Running pnpm audit --json for baseline...");
        let baseline_json = run_pnpm_audit(temp_dir.to_path_buf()).await?;

        let mut baseline_advisories = std::collections::HashMap::new();
        if let Some(advs) = baseline_json.get("advisories").and_then(|a| a.as_object()) {
            for (id, adv) in advs {
                baseline_advisories.insert(id.clone(), adv.clone());
            }
        }

        if baseline_advisories.is_empty() {
            tracing::info!("No vulnerabilities found in baseline.");
            return Ok(None);
        }

        tracing::info!(
            "Found {} vulnerabilities. Discovering mature fixes...",
            baseline_advisories.len()
        );

        let mut overrides = std::collections::HashMap::new();
        let mut module_to_vulnerable_versions = std::collections::HashMap::new();
        let mut blocked_by_age: std::collections::HashMap<
            String,
            Vec<(semver::Version, chrono::DateTime<chrono::Utc>)>,
        > = std::collections::HashMap::new();

        for adv in baseline_advisories.values() {
            if let (Some(module), Some(vulnerable)) = (
                adv.get("module_name").and_then(|m| m.as_str()),
                adv.get("vulnerable_versions").and_then(|p| p.as_str()),
            ) {
                module_to_vulnerable_versions
                    .entry(module.to_string())
                    .or_insert_with(Vec::new)
                    .push(vulnerable.to_string());
            }
        }

        for (module, vulnerable_list) in module_to_vulnerable_versions {
            match self
                .npm_client
                .resolve_mature_version(&module, &vulnerable_list, minimum_release_age)
                .await
            {
                Ok(resolution) => {
                    let resolved_empty = resolution.resolved.is_empty();
                    let blocked_empty = resolution.blocked.is_empty();

                    if !resolved_empty {
                        tracing::info!(
                            "Found mature fixes for {}: {:?}",
                            module,
                            resolution.resolved
                        );
                        overrides.insert(module.clone(), resolution.resolved);
                    }
                    if !blocked_empty {
                        tracing::info!(
                            "Fixes for {} are blocked by age policy: {:?}",
                            module,
                            resolution
                                .blocked
                                .iter()
                                .map(|(v, _)| v.to_string())
                                .collect::<Vec<_>>()
                        );
                        blocked_by_age.insert(module.clone(), resolution.blocked);
                    }

                    if resolved_empty && blocked_empty {
                        tracing::warn!(
                            "No mature fix found for {} satisfying vulnerabilities {:?}",
                            module,
                            vulnerable_list
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("Error resolving mature fix for {}: {}", module, e);
                }
            }
        }

        if overrides.is_empty() {
            tracing::info!("No mature fixes could be found for any vulnerabilities.");
            return Ok(None);
        }

        tracing::info!("Applying direct fixes to package.json files...");

        for (path, pkg_json) in mutable_pkg_jsons.iter_mut() {
            let mut file_updated = false;
            for (module, mature_versions) in &overrides {
                for key in ["dependencies", "devDependencies"] {
                    if let Some(deps) = pkg_json.get_mut(key).and_then(|d| d.as_object_mut()) {
                        if let Some(req_val) = deps.get(module) {
                            if let Some(req) = req_val.as_str() {
                                let clean_req = req.trim_start_matches(&['^', '~', '=', 'v'][..]);
                                let current_ver = match semver::Version::parse(clean_req) {
                                    Ok(v) => v,
                                    Err(_) => continue, // Cannot parse, skip
                                };

                                let mut target_mature_version = None;
                                for mv in mature_versions {
                                    if current_ver.major == 0 {
                                        if mv.major == 0 && mv.minor == current_ver.minor {
                                            target_mature_version = Some(mv);
                                        }
                                    } else if mv.major == current_ver.major {
                                        target_mature_version = Some(mv);
                                    }
                                }

                                if let Some(mature_version) = target_mature_version {
                                    let prefix = if req.starts_with('^') {
                                        "^"
                                    } else if req.starts_with('~') {
                                        "~"
                                    } else {
                                        ""
                                    };
                                    let new_req = format!("{}{}", prefix, mature_version);
                                    deps.insert(
                                        module.to_string(),
                                        serde_json::Value::String(new_req),
                                    );
                                    file_updated = true;
                                } else {
                                    tracing::warn!("Skipping direct dependency bump for {} in {} because no mature fix exists in major version {}", module, path, current_ver.major);
                                }
                            }
                        }
                    }
                }
            }

            if file_updated {
                let updated_pkg_json_str = serde_json::to_string_pretty(pkg_json)? + "\n";
                let full_path = temp_dir.join(path);
                fs::write(&full_path, &updated_pkg_json_str)?;
            }
        }

        // Snapshot the clean state with first-party bumps (but before forced transitive overrides)
        let clean_pkg_jsons: std::collections::HashMap<String, serde_json::Value> =
            mutable_pkg_jsons.clone();

        let before_versions = extract_versions_from_lock(lockfile.as_deref().unwrap_or_default());
        let mut injected_transitive = false;

        // pnpm v11 no longer reads the `pnpm` field from package.json; overrides must go in
        // pnpm-workspace.yaml under the `overrides:` key.
        let mut new_overrides = serde_yml::Mapping::new();
        for (module, mature_versions) in &overrides {
            let active_majors = if let Some(vers) = before_versions.get(module) {
                let mut majors = std::collections::HashSet::new();
                for v in vers {
                    if let Ok(ver) = semver::Version::parse(v) {
                        if ver.major == 0 {
                            majors.insert((0, ver.minor));
                        } else {
                            majors.insert((ver.major, 0));
                        }
                    }
                }
                majors
            } else {
                std::collections::HashSet::new()
            };

            for mature_version in mature_versions {
                let is_used = if mature_version.major == 0 {
                    active_majors.contains(&(0, mature_version.minor))
                } else {
                    active_majors.contains(&(mature_version.major, 0))
                };

                if is_used {
                    let req_prefix = if mature_version.major == 0 { "~" } else { "^" };
                    new_overrides.insert(
                        serde_yml::Value::String(format!("{}@{}", module, mature_version.major)),
                        serde_yml::Value::String(format!("{}{}", req_prefix, mature_version)),
                    );
                    injected_transitive = true;
                }
            }
        }

        if injected_transitive {
            let mut workspace_value: serde_yml::Value = workspace
                .as_deref()
                .and_then(|w| serde_yml::from_str(w).ok())
                .unwrap_or_else(|| serde_yml::Value::Mapping(serde_yml::Mapping::new()));

            if let Some(ws_map) = workspace_value.as_mapping_mut() {
                if let Some(existing) = ws_map.get_mut("overrides").and_then(|v| v.as_mapping_mut())
                {
                    for (k, v) in new_overrides {
                        existing.insert(k, v);
                    }
                } else {
                    ws_map.insert(
                        serde_yml::Value::String("overrides".to_string()),
                        serde_yml::Value::Mapping(new_overrides),
                    );
                }
            }
            fs::write(
                temp_dir.join("pnpm-workspace.yaml"),
                serde_yml::to_string(&workspace_value)?,
            )?;
        }

        // pnpm v11 won't update pnpm-lock.yaml when overrides change unless node_modules is absent.
        let _ = fs::remove_dir_all(temp_dir.join("node_modules"));

        tracing::info!(
            "Running pnpm install in temp directory to update lockfile with forced fixes..."
        );
        if !run_pnpm_install(temp_dir.to_path_buf(), workspace.is_some(), true, false).await? {
            tracing::warn!("pnpm install failed, continuing anyway...");
        }

        tracing::info!("Running pnpm dedupe...");
        if !run_pnpm_dedupe(temp_dir.to_path_buf(), false).await? {
            tracing::warn!("pnpm dedupe failed, continuing anyway...");
        }

        if injected_transitive {
            // Restore pnpm-workspace.yaml without overrides, then re-run pnpm install so that
            // pnpm itself removes the overrides section from pnpm-lock.yaml while keeping the
            // secure resolved versions (node_modules was deleted above, so pnpm re-resolves from
            // the lockfile rather than from installed packages).
            match workspace.as_deref() {
                Some(original) => {
                    fs::write(temp_dir.join("pnpm-workspace.yaml"), original)?;
                }
                None => {
                    let _ = fs::remove_file(temp_dir.join("pnpm-workspace.yaml"));
                }
            }

            tracing::info!(
                "Running pnpm install --lockfile-only to remove override markers from lockfile..."
            );
            if !run_pnpm_install(temp_dir.to_path_buf(), workspace.is_some(), true, false).await? {
                tracing::warn!("Final pnpm install failed, continuing anyway...");
            }
        }

        tracing::info!("Restoring original package.json files...");
        let mut all_modifications = Vec::new();

        for (path, clean_json) in &clean_pkg_jsons {
            let full_path = temp_dir.join(path);
            let updated_pkg_json_str = serde_json::to_string_pretty(clean_json)? + "\n";
            fs::write(&full_path, &updated_pkg_json_str)?;

            if let Some(original) = original_pkg_jsons.get(path) {
                if updated_pkg_json_str != *original {
                    all_modifications.push(FileModification {
                        path: path.clone(),
                        state: crate::core::engine::repository::FileState::Write(
                            updated_pkg_json_str,
                        ),
                    });
                }
            }
        }

        tracing::info!("Running pnpm audit --json for post-fix comparison...");
        let after_json = run_pnpm_audit(temp_dir.to_path_buf()).await?;

        let mut after_advisories = std::collections::HashSet::new();
        if let Some(advs) = after_json.get("advisories").and_then(|a| a.as_object()) {
            for id in advs.keys() {
                after_advisories.insert(id.clone());
            }
        }

        let mut resolved_advisories = Vec::new();
        for (id, adv) in &baseline_advisories {
            if !after_advisories.contains(id) {
                resolved_advisories.push(adv.clone());
            }
        }

        if resolved_advisories.is_empty() {
            tracing::info!("No vulnerabilities could be resolved.");
            return Ok(None);
        }

        let updated_lock = fs::read_to_string(temp_dir.join("pnpm-lock.yaml")).ok();

        let after_versions =
            extract_versions_from_lock(updated_lock.as_deref().unwrap_or_default());

        let mut still_vulnerable = std::collections::HashSet::new();
        for id in baseline_advisories.keys() {
            if after_advisories.contains(id) {
                still_vulnerable.insert(id.clone());
            }
        }

        let mut unfixable_vulnerabilities: std::collections::HashMap<
            String,
            Vec<serde_json::Value>,
        > = std::collections::HashMap::new();
        for id in &still_vulnerable {
            if let Some(adv) = baseline_advisories.get(id) {
                if let Some(module) = adv.get("module_name").and_then(|m| m.as_str()) {
                    unfixable_vulnerabilities
                        .entry(module.to_string())
                        .or_default()
                        .push(adv.clone());
                }
            }
        }

        let mut resolved_by_module: std::collections::HashMap<String, Vec<serde_json::Value>> =
            std::collections::HashMap::new();
        for adv in resolved_advisories {
            if let Some(module) = adv.get("module_name").and_then(|m| m.as_str()) {
                resolved_by_module
                    .entry(module.to_string())
                    .or_default()
                    .push(adv);
            }
        }

        let mut resolved_advisories_map = std::collections::HashMap::new();
        for (module_name, advisories) in resolved_by_module {
            let before_set = before_versions.get(&module_name);
            let after_set = after_versions.get(&module_name);

            let mut before_list: Vec<String> = before_set
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();
            let mut after_list: Vec<String> = after_set
                .map(|s| s.iter().cloned().collect())
                .unwrap_or_default();
            before_list.sort();
            after_list.sort();

            resolved_advisories_map.insert(
                module_name,
                crate::core::engine::advisories::ResolvedAdvisoryBump {
                    before_versions: before_list,
                    after_versions: after_list,
                    advisories,
                },
            );
        }

        if let Some(lock) = updated_lock {
            let old_lock = lockfile.unwrap_or_default();
            if lock != old_lock {
                all_modifications.push(FileModification {
                    path: "pnpm-lock.yaml".to_string(),
                    state: crate::core::engine::repository::FileState::Write(lock),
                });
            }
        }

        if all_modifications.is_empty() {
            Ok(None)
        } else {
            let summary = crate::core::engine::advisories::SecurityUpdateSummary {
                resolved_advisories: resolved_advisories_map,
                blocked_by_age,
                unfixable_vulnerabilities,
                minimum_release_age,
            };

            Ok(Some(
                crate::core::engine::advisories::SecurityUpdateResult {
                    modifications: all_modifications,
                    summary,
                },
            ))
        }
    }
}

async fn run_pnpm_install(
    dir_path: std::path::PathBuf,
    is_workspace: bool,
    lockfile_only: bool,
    silent: bool,
) -> Result<bool> {
    tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new("pnpm");
        cmd.arg("install");

        if lockfile_only {
            cmd.arg("--lockfile-only");
        }

        cmd.arg("--ignore-scripts");

        if is_workspace {
            cmd.arg("--recursive");
        }

        if silent {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }

        let success = cmd.current_dir(&dir_path).status()?.success();
        Ok(success)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Task join error: {}", e))?
}

async fn run_pnpm_dedupe(dir_path: std::path::PathBuf, silent: bool) -> Result<bool> {
    tokio::task::spawn_blocking(move || {
        let mut cmd = Command::new("pnpm");
        cmd.arg("dedupe").arg("--ignore-scripts");

        if silent {
            cmd.stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null());
        }

        let success = cmd.current_dir(&dir_path).status()?.success();
        Ok(success)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Task join error: {}", e))?
}

async fn run_pnpm_audit(dir_path: std::path::PathBuf) -> Result<serde_json::Value> {
    tokio::task::spawn_blocking(move || {
        let output = Command::new("pnpm")
            .arg("audit")
            .arg("--json")
            .current_dir(&dir_path)
            .output()?;

        let json: serde_json::Value =
            serde_json::from_slice(&output.stdout).unwrap_or(serde_json::json!({}));
        Ok(json)
    })
    .await
    .map_err(|e| anyhow::anyhow!("Task join error: {}", e))?
}

pub(crate) fn extract_versions_from_lock(
    lock_content: &str,
) -> std::collections::HashMap<String, std::collections::HashSet<String>> {
    let mut module_versions = std::collections::HashMap::new();
    if let Ok(lock_val) = serde_yml::from_str::<serde_yml::Value>(lock_content) {
        if let Some(packages) = lock_val.get("packages").and_then(|p| p.as_mapping()) {
            for (k, _) in packages {
                if let Some(path) = k.as_str() {
                    if let Some(stripped) = path.strip_prefix('/') {
                        let parts: Vec<&str> = stripped.split('@').collect();
                        if parts.len() == 2 {
                            let name = parts[0];
                            let version = parts[1].split('(').next().unwrap_or("").to_string();
                            module_versions
                                .entry(name.to_string())
                                .or_insert_with(std::collections::HashSet::new)
                                .insert(version);
                        } else {
                            let parts: Vec<&str> = stripped.split('/').collect();
                            if parts.len() >= 2 {
                                let version = parts
                                    .last()
                                    .unwrap()
                                    .split('(')
                                    .next()
                                    .unwrap_or("")
                                    .to_string();
                                let name = parts[0..parts.len() - 1].join("/");
                                module_versions
                                    .entry(name)
                                    .or_insert_with(std::collections::HashSet::new)
                                    .insert(version);
                            }
                        }
                    } else if let Some(at_idx) = path.rfind('@') {
                        if at_idx > 0 {
                            let name = &path[0..at_idx];
                            let version = path[at_idx + 1..]
                                .split('(')
                                .next()
                                .unwrap_or("")
                                .to_string();
                            module_versions
                                .entry(name.to_string())
                                .or_insert_with(std::collections::HashSet::new)
                                .insert(version);
                        }
                    }
                }
            }
        }
    }
    module_versions
}
