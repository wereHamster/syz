use anyhow::Result;
use async_trait::async_trait;

use crate::core::clients;
use crate::core::engine::advisories::{ReleaseAgeException, ReleaseAgeExceptionKind};
use crate::core::engine::ecosystems::npm::internal::discover_project_dependencies::WorkspaceConfig;
use crate::core::engine::ecosystems::npm::internal::workspace_yaml::{self, PinnedVersions};
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

        let mut pruned_workspace = None;
        if let Some(ref w) = workspace {
            let mut publish_times = PublishTimes::new(self.npm_client.clone());
            let pruned = prune_matured_exclusions(
                w,
                effective_minimum_release_age(Some(w)),
                &mut publish_times,
            )
            .await;
            if pruned != *w {
                pruned_workspace = Some(pruned);
            }
        }

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

        if let Some(ref w) = pruned_workspace.as_ref().or(workspace.as_ref()) {
            fs::write(temp_dir.join("pnpm-workspace.yaml"), w)?;
        }
        if let Some(ref l) = lockfile {
            fs::write(temp_dir.join("pnpm-lock.yaml"), l)?;
        }
        if let Some(pruned) = pruned_workspace {
            tree_items.push(FileModification {
                path: "pnpm-workspace.yaml".to_string(),
                state: crate::core::engine::repository::FileState::Write(pruned),
            });
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
        let original_workspace = snapshot.read_file("pnpm-workspace.yaml").await.ok();
        let lockfile = snapshot.read_file("pnpm-lock.yaml").await.ok();
        let is_workspace = original_workspace.is_some();

        let minimum_release_age = effective_minimum_release_age(original_workspace.as_deref());
        let mut publish_times = PublishTimes::new(self.npm_client.clone());

        let workspace = match original_workspace.as_deref() {
            Some(w) => {
                Some(prune_matured_exclusions(w, minimum_release_age, &mut publish_times).await)
            }
            None => None,
        };

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
        if !run_pnpm_install(temp_dir.to_path_buf(), is_workspace, true, false).await? {
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

        let before_versions = extract_versions_from_lock(lockfile.as_deref().unwrap_or_default());

        let mut mature_fixes = std::collections::HashMap::new();
        let mut immature_fixes: std::collections::HashMap<
            String,
            Vec<(semver::Version, chrono::DateTime<chrono::Utc>)>,
        > = std::collections::HashMap::new();
        let mut module_to_vulnerable_versions = std::collections::HashMap::new();

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
            let installed: Vec<semver::Version> = before_versions
                .get(&module)
                .into_iter()
                .flatten()
                .filter_map(|v| semver::Version::parse(v).ok())
                .collect();

            match self
                .npm_client
                .resolve_mature_version(
                    &module,
                    &vulnerable_list,
                    &installed,
                    Some(minimum_release_age),
                )
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
                        mature_fixes.insert(module.clone(), resolution.resolved);
                    }
                    if !blocked_empty {
                        tracing::info!(
                            "Fixes for {} have not met the age policy yet: {:?}",
                            module,
                            resolution
                                .blocked
                                .iter()
                                .map(|(v, _)| v.to_string())
                                .collect::<Vec<_>>()
                        );
                        immature_fixes.insert(module.clone(), resolution.blocked);
                    }

                    if resolved_empty && blocked_empty {
                        tracing::warn!(
                            "No fix found for {} satisfying vulnerabilities {:?}",
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

        if mature_fixes.is_empty() && immature_fixes.is_empty() {
            tracing::info!("No fixes could be found for any vulnerabilities.");
            return Ok(None);
        }

        let baseline_lock = fs::read_to_string(temp_dir.join("pnpm-lock.yaml")).ok();
        let ctx = AuditContext {
            temp_dir,
            is_workspace,
            workspace: workspace.as_deref(),
            original_pkg_jsons: &original_pkg_jsons,
            pkg_jsons: &mutable_pkg_jsons,
            baseline_lock: baseline_lock.as_deref(),
            before_versions: &before_versions,
            minimum_release_age,
        };

        // Security fixes may bypass the minimum release age. The exact versions (of the fix and
        // of everything it pulls in that is too young) are added to `minimumReleaseAgeExclude`
        // and listed in the pull request for review.
        let mut blocked_by_age = std::collections::HashMap::new();
        let mut seeds = PinnedVersions::new();
        let mut applied = None;
        if !immature_fixes.is_empty() {
            let mut all_fixes = mature_fixes.clone();
            for (module, versions) in &immature_fixes {
                for (version, _) in versions {
                    all_fixes
                        .entry(module.clone())
                        .or_insert_with(Vec::new)
                        .push(version.clone());
                    seeds
                        .entry(module.clone())
                        .or_default()
                        .insert(version.to_string());
                }
            }

            let attempt = self
                .apply_security_fixes(&ctx, &all_fixes, &seeds, &mut publish_times)
                .await?;
            if attempt.verified {
                applied = Some(attempt);
            } else {
                tracing::warn!(
                    "Lockfile with fixes that haven't met minimumReleaseAge failed verification; falling back to mature fixes only"
                );
                blocked_by_age = immature_fixes;
                seeds.clear();
            }
        }

        let applied = match applied {
            Some(applied) => applied,
            None if mature_fixes.is_empty() => {
                tracing::info!("No mature fixes could be found for any vulnerabilities.");
                return Ok(None);
            }
            None => {
                let attempt = self
                    .apply_security_fixes(&ctx, &mature_fixes, &seeds, &mut publish_times)
                    .await?;
                if !attempt.verified {
                    tracing::warn!("Lockfile failed pnpm's supply-chain verification");
                }
                attempt
            }
        };

        let mut all_modifications = applied.modifications;

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

        if applied.workspace != original_workspace {
            if let Some(content) = &applied.workspace {
                all_modifications.push(FileModification {
                    path: "pnpm-workspace.yaml".to_string(),
                    state: crate::core::engine::repository::FileState::Write(content.clone()),
                });
            }
        }

        let mut release_age_exceptions = Vec::new();
        let new_exclusions = workspace_yaml::missing_exclusions(
            workspace.as_deref().unwrap_or_default(),
            &applied.exclusions,
        );
        for (name, versions) in &new_exclusions {
            for version in versions {
                let kind = if seeds.get(name).is_some_and(|s| s.contains(version)) {
                    ReleaseAgeExceptionKind::Fix
                } else {
                    ReleaseAgeExceptionKind::Dependency
                };
                release_age_exceptions.push(ReleaseAgeException {
                    name: name.clone(),
                    version: version.clone(),
                    published_at: publish_times.get(name, version).await,
                    kind,
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
                minimum_release_age: Some(minimum_release_age),
                release_age_exceptions,
                release_age_exceptions_location: Some(
                    "`minimumReleaseAgeExclude` in `pnpm-workspace.yaml`".to_string(),
                ),
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

/// The parts of the audit setup shared by every [`NpmPatcher::apply_security_fixes`] attempt.
struct AuditContext<'a> {
    temp_dir: &'a std::path::Path,
    is_workspace: bool,
    /// `pnpm-workspace.yaml` before any fix is applied (after pruning matured exclusions).
    workspace: Option<&'a str>,
    original_pkg_jsons: &'a std::collections::HashMap<String, String>,
    pkg_jsons: &'a std::collections::HashMap<String, serde_json::Value>,
    baseline_lock: Option<&'a str>,
    before_versions: &'a std::collections::HashMap<String, std::collections::HashSet<String>>,
    minimum_release_age: chrono::Duration,
}

struct AppliedFixes {
    /// Changed `package.json` files.
    modifications: Vec<FileModification>,
    /// The final `pnpm-workspace.yaml`, including any new release age exclusions.
    workspace: Option<String>,
    /// Exact versions that bypass the minimum release age.
    exclusions: PinnedVersions,
    /// Whether the lockfile passes `pnpm install --frozen-lockfile`.
    verified: bool,
}

impl NpmPatcher {
    /// Bumps the vulnerable packages to `fixes` (directly, and through temporary overrides for
    /// transitive dependencies) and regenerates the lockfile in `ctx.temp_dir`.
    ///
    /// `seeds` are fix versions that haven't met the minimum release age. They are excluded from
    /// the policy along with everything they pull in that is too young.
    async fn apply_security_fixes(
        &self,
        ctx: &AuditContext<'_>,
        fixes: &std::collections::HashMap<String, Vec<semver::Version>>,
        seeds: &PinnedVersions,
        publish_times: &mut PublishTimes,
    ) -> Result<AppliedFixes> {
        let temp_dir = ctx.temp_dir;

        // Every attempt starts from the baseline.
        for (path, content) in ctx.original_pkg_jsons {
            fs::write(temp_dir.join(path), content)?;
        }
        if let Some(lock) = ctx.baseline_lock {
            fs::write(temp_dir.join("pnpm-lock.yaml"), lock)?;
        }

        tracing::info!("Applying direct fixes to package.json files...");

        let mut pkg_jsons = ctx.pkg_jsons.clone();
        for (path, pkg_json) in pkg_jsons.iter_mut() {
            let mut file_updated = false;
            for (module, fix_versions) in fixes {
                for key in ["dependencies", "devDependencies"] {
                    if let Some(deps) = pkg_json.get_mut(key).and_then(|d| d.as_object_mut()) {
                        if let Some(req_val) = deps.get(module) {
                            if let Some(req) = req_val.as_str() {
                                let clean_req = req.trim_start_matches(&['^', '~', '=', 'v'][..]);
                                let current_ver = match semver::Version::parse(clean_req) {
                                    Ok(v) => v,
                                    Err(_) => continue, // Cannot parse, skip
                                };

                                let mut target_fix_version = None;
                                for fv in fix_versions {
                                    if current_ver.major == 0 {
                                        if fv.major == 0 && fv.minor == current_ver.minor {
                                            target_fix_version = Some(fv);
                                        }
                                    } else if fv.major == current_ver.major {
                                        target_fix_version = Some(fv);
                                    }
                                }

                                if let Some(fix_version) = target_fix_version {
                                    let prefix = if req.starts_with('^') {
                                        "^"
                                    } else if req.starts_with('~') {
                                        "~"
                                    } else {
                                        ""
                                    };
                                    let new_req = format!("{}{}", prefix, fix_version);
                                    deps.insert(
                                        module.to_string(),
                                        serde_json::Value::String(new_req),
                                    );
                                    file_updated = true;
                                } else {
                                    tracing::warn!("Skipping direct dependency bump for {} in {} because no fix exists in major version {}", module, path, current_ver.major);
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

        // pnpm v11 no longer reads the `pnpm` field from package.json; overrides must go in
        // pnpm-workspace.yaml under the `overrides:` key.
        let mut new_overrides = serde_yml::Mapping::new();
        for (module, fix_versions) in fixes {
            let active_majors = if let Some(vers) = ctx.before_versions.get(module) {
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

            for fix_version in fix_versions {
                let is_used = if fix_version.major == 0 {
                    active_majors.contains(&(0, fix_version.minor))
                } else {
                    active_majors.contains(&(fix_version.major, 0))
                };

                if is_used {
                    let req_prefix = if fix_version.major == 0 { "~" } else { "^" };
                    new_overrides.insert(
                        serde_yml::Value::String(format!("{}@{}", module, fix_version.major)),
                        serde_yml::Value::String(format!("{}{}", req_prefix, fix_version)),
                    );
                }
            }
        }

        let injected_transitive = !new_overrides.is_empty();
        let mut renderer = WorkspaceRenderer {
            base: ctx.workspace.map(str::to_string),
            overrides: injected_transitive.then_some(new_overrides),
        };
        let mut exclusions = seeds.clone();

        // pnpm v11 won't update pnpm-lock.yaml when overrides change unless node_modules is absent.
        let _ = fs::remove_dir_all(temp_dir.join("node_modules"));

        tracing::info!(
            "Running pnpm install in temp directory to update lockfile with forced fixes..."
        );
        if !run_with_release_age_exceptions(
            ctx,
            &renderer,
            &mut exclusions,
            publish_times,
            &install_args(ctx.is_workspace, &[]),
        )
        .await?
        {
            tracing::warn!("pnpm install failed, continuing anyway...");
        }

        tracing::info!("Running pnpm dedupe...");
        if !run_with_release_age_exceptions(
            ctx,
            &renderer,
            &mut exclusions,
            publish_times,
            &dedupe_args(),
        )
        .await?
        {
            tracing::warn!("pnpm dedupe failed, continuing anyway...");
        }

        if injected_transitive {
            // Drop the overrides from pnpm-workspace.yaml, then re-run pnpm install so that
            // pnpm itself removes the overrides section from pnpm-lock.yaml while keeping the
            // secure resolved versions (node_modules was deleted above, so pnpm re-resolves from
            // the lockfile rather than from installed packages).
            renderer.overrides = None;

            tracing::info!(
                "Running pnpm install --lockfile-only to remove override markers from lockfile..."
            );
            if !run_with_release_age_exceptions(
                ctx,
                &renderer,
                &mut exclusions,
                publish_times,
                &install_args(ctx.is_workspace, &[]),
            )
            .await?
            {
                tracing::warn!("Final pnpm install failed, continuing anyway...");
            }
        }

        // Earlier rounds may have excluded versions that dedupe replaced since.
        let lock = fs::read_to_string(temp_dir.join("pnpm-lock.yaml")).unwrap_or_default();
        let locked = extract_versions_from_lock(&lock);
        for (name, versions) in exclusions.iter_mut() {
            versions.retain(|v| locked.get(name).is_some_and(|l| l.contains(v)));
        }
        exclusions.retain(|_, versions| !versions.is_empty());
        renderer.write(temp_dir, &exclusions)?;

        tracing::info!("Verifying lockfile against pnpm's supply-chain policies...");
        let verified = run_pnpm(temp_dir, &frozen_install_args(ctx.is_workspace))
            .await?
            .success;

        let mut modifications = Vec::new();
        for (path, pkg_json) in &pkg_jsons {
            let updated_pkg_json_str = serde_json::to_string_pretty(pkg_json)? + "\n";
            if let Some(original) = ctx.original_pkg_jsons.get(path) {
                if updated_pkg_json_str != *original {
                    modifications.push(FileModification {
                        path: path.clone(),
                        state: crate::core::engine::repository::FileState::Write(
                            updated_pkg_json_str,
                        ),
                    });
                }
            }
        }

        Ok(AppliedFixes {
            modifications,
            workspace: renderer.render(&exclusions)?,
            exclusions,
            verified,
        })
    }
}

/// pnpm's default `minimumReleaseAge` (in minutes) since v11.
const PNPM_DEFAULT_MINIMUM_RELEASE_AGE_MINUTES: i64 = 1440;

/// How many times pnpm is re-run with additional release age exclusions before giving up.
const MAX_RELEASE_AGE_ROUNDS: usize = 4;

/// pnpm reports immature picks during resolution as `ERR_PNPM_NO_MATURE_MATCHING_VERSION` and
/// immature lockfile entries as `ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION`.
const RELEASE_AGE_ERRORS: [&str; 2] = [
    "ERR_PNPM_NO_MATURE_MATCHING_VERSION",
    "ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION",
];

fn effective_minimum_release_age(workspace: Option<&str>) -> chrono::Duration {
    let minutes = workspace
        .and_then(|w| serde_yml::from_str::<WorkspaceConfig>(w).ok())
        .and_then(|config| config.minimum_release_age)
        .unwrap_or(PNPM_DEFAULT_MINIMUM_RELEASE_AGE_MINUTES);
    chrono::Duration::minutes(minutes)
}

/// Memoized npm registry publish times.
struct PublishTimes {
    client: clients::npm::Npm,
    cache: std::collections::HashMap<
        String,
        Option<std::collections::HashMap<String, chrono::DateTime<chrono::Utc>>>,
    >,
}

impl PublishTimes {
    fn new(client: clients::npm::Npm) -> Self {
        Self {
            client,
            cache: std::collections::HashMap::new(),
        }
    }

    async fn get(&mut self, name: &str, version: &str) -> Option<chrono::DateTime<chrono::Utc>> {
        if !self.cache.contains_key(name) {
            let times = match self.client.get_publish_times(name).await {
                Ok(times) => Some(times),
                Err(e) => {
                    tracing::warn!("Failed to fetch publish times for {}: {}", name, e);
                    None
                }
            };
            self.cache.insert(name.to_string(), times);
        }
        self.cache
            .get(name)
            .and_then(|times| times.as_ref())
            .and_then(|times| times.get(version))
            .copied()
    }

    /// Versions with an unknown publish time count as immature.
    async fn is_mature(&mut self, name: &str, version: &str, min_age: chrono::Duration) -> bool {
        match self.get(name, version).await {
            Some(published_at) => chrono::Utc::now() - published_at >= min_age,
            None => false,
        }
    }
}

/// Removes exact-version `minimumReleaseAgeExclude` entries whose versions have matured, so
/// exceptions granted for security fixes don't outlive their purpose.
async fn prune_matured_exclusions(
    workspace: &str,
    minimum_release_age: chrono::Duration,
    publish_times: &mut PublishTimes,
) -> String {
    let mut matured = PinnedVersions::new();
    for exclusion in workspace_yaml::release_age_exclusions(workspace) {
        if let workspace_yaml::Exclusion::Versions { name, versions } = exclusion {
            for version in versions {
                if publish_times
                    .is_mature(&name, &version, minimum_release_age)
                    .await
                {
                    matured.entry(name.clone()).or_default().insert(version);
                }
            }
        }
    }

    if !matured.is_empty() {
        tracing::info!(
            "Pruning matured minimumReleaseAgeExclude entries: {:?}",
            matured
        );
    }
    workspace_yaml::remove_release_age_exclusions(workspace, &matured)
}

/// Renders `pnpm-workspace.yaml` from its base content, release age exclusions and (while the
/// lockfile is regenerated) temporary overrides.
struct WorkspaceRenderer {
    base: Option<String>,
    overrides: Option<serde_yml::Mapping>,
}

impl WorkspaceRenderer {
    fn render(&self, exclusions: &PinnedVersions) -> Result<Option<String>> {
        let content = match &self.base {
            Some(base) => Some(workspace_yaml::add_release_age_exclusions(base, exclusions)),
            None if !exclusions.is_empty() => {
                Some(workspace_yaml::add_release_age_exclusions("", exclusions))
            }
            None => None,
        };

        let Some(overrides) = &self.overrides else {
            return Ok(content);
        };

        let mut workspace_value: serde_yml::Value = content
            .as_deref()
            .and_then(|w| serde_yml::from_str(w).ok())
            .filter(|v: &serde_yml::Value| v.is_mapping())
            .unwrap_or_else(|| serde_yml::Value::Mapping(serde_yml::Mapping::new()));

        if let Some(ws_map) = workspace_value.as_mapping_mut() {
            if let Some(existing) = ws_map.get_mut("overrides").and_then(|v| v.as_mapping_mut()) {
                for (k, v) in overrides {
                    existing.insert(k.clone(), v.clone());
                }
            } else {
                ws_map.insert(
                    serde_yml::Value::String("overrides".to_string()),
                    serde_yml::Value::Mapping(overrides.clone()),
                );
            }
        }

        Ok(Some(serde_yml::to_string(&workspace_value)?))
    }

    fn write(&self, dir: &std::path::Path, exclusions: &PinnedVersions) -> Result<()> {
        match self.render(exclusions)? {
            Some(content) => fs::write(dir.join("pnpm-workspace.yaml"), content)?,
            None => {
                let _ = fs::remove_file(dir.join("pnpm-workspace.yaml"));
            }
        }
        Ok(())
    }
}

/// Runs pnpm with `args` (e.g. `install --lockfile-only` or `dedupe`) and, whenever pnpm rejects
/// versions because they haven't met `minimumReleaseAge`, adds them to `exclusions` and tries
/// again.
///
/// pnpm reports every offending version, including transitive ones (e.g. the `@next/swc-*`
/// binaries that `next` pins exactly), but only as human-readable text. If that report can't
/// be parsed, the lockfile is resolved once without the policy, and every new version that
/// hasn't matured is excluded.
async fn run_with_release_age_exceptions(
    ctx: &AuditContext<'_>,
    renderer: &WorkspaceRenderer,
    exclusions: &mut PinnedVersions,
    publish_times: &mut PublishTimes,
    args: &[String],
) -> Result<bool> {
    let temp_dir = ctx.temp_dir;
    let lock_path = temp_dir.join("pnpm-lock.yaml");

    for _ in 0..MAX_RELEASE_AGE_ROUNDS {
        renderer.write(temp_dir, exclusions)?;

        let output = run_pnpm(temp_dir, args).await?;
        if output.success {
            return Ok(true);
        }
        if !RELEASE_AGE_ERRORS.iter().any(|e| output.text.contains(e)) {
            tracing::warn!("pnpm {} failed:\n{}", args[0], output.text);
            return Ok(false);
        }

        let mut added = false;
        for (name, version) in parse_release_age_violations(&output.text) {
            added |= exclusions.entry(name).or_default().insert(version);
        }
        if added {
            tracing::info!(
                "Excluding versions from minimumReleaseAge: {:?}",
                exclusions
            );
            continue;
        }

        tracing::info!(
            "Resolving without minimumReleaseAge to find versions that need an exception..."
        );
        let before =
            extract_versions_from_lock(&fs::read_to_string(&lock_path).unwrap_or_default());
        let mut bypass_args = args.to_vec();
        bypass_args.push("--config.minimum-release-age=0".to_string());
        let bypass = run_pnpm(temp_dir, &bypass_args).await?;
        if !bypass.success {
            tracing::warn!("pnpm {} failed:\n{}", args[0], bypass.text);
            return Ok(false);
        }

        let after = extract_versions_from_lock(&fs::read_to_string(&lock_path).unwrap_or_default());
        for (name, versions) in &after {
            for version in versions {
                if before.get(name).is_some_and(|b| b.contains(version)) {
                    continue;
                }
                if !publish_times
                    .is_mature(name, version, ctx.minimum_release_age)
                    .await
                {
                    added |= exclusions
                        .entry(name.clone())
                        .or_default()
                        .insert(version.clone());
                }
            }
        }
        if !added {
            tracing::warn!(
                "pnpm rejected the lockfile, but no version that needs an exception was found"
            );
            return Ok(false);
        }
    }

    tracing::warn!(
        "Gave up on minimumReleaseAge exclusions after {} rounds",
        MAX_RELEASE_AGE_ROUNDS
    );
    Ok(false)
}

/// Extracts `name@version` pairs from pnpm's release age errors (see [`RELEASE_AGE_ERRORS`]),
/// which list them as `<name>@<version> was published at <time>, within the ... cutoff`.
fn parse_release_age_violations(text: &str) -> Vec<(String, String)> {
    let pattern = regex::Regex::new(r"(?m)^\s*(\S+) was published at").unwrap();
    pattern
        .captures_iter(text)
        .filter_map(|caps| {
            let spec = caps.get(1)?.as_str();
            let idx = spec.rfind('@').filter(|idx| *idx > 0)?;
            let version = &spec[idx + 1..];
            semver::Version::parse(version).ok()?;
            Some((spec[..idx].to_string(), version.to_string()))
        })
        .collect()
}

fn install_args(is_workspace: bool, extra: &[&str]) -> Vec<String> {
    let mut args = vec!["install", "--lockfile-only", "--ignore-scripts"];
    if is_workspace {
        args.push("--recursive");
    }
    args.extend_from_slice(extra);
    args.into_iter().map(str::to_string).collect()
}

fn dedupe_args() -> Vec<String> {
    vec!["dedupe".to_string(), "--ignore-scripts".to_string()]
}

fn frozen_install_args(is_workspace: bool) -> Vec<String> {
    install_args(is_workspace, &["--frozen-lockfile"])
}

struct PnpmOutput {
    success: bool,
    /// Combined stdout and stderr.
    text: String,
}

async fn run_pnpm(dir: &std::path::Path, args: &[String]) -> Result<PnpmOutput> {
    let dir = dir.to_path_buf();
    let args = args.to_vec();
    tokio::task::spawn_blocking(move || {
        let output = Command::new("pnpm")
            .args(&args)
            .current_dir(&dir)
            .output()?;
        let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
        text.push_str(&String::from_utf8_lossy(&output.stderr));
        Ok(PnpmOutput {
            success: output.status.success(),
            text,
        })
    })
    .await
    .map_err(|e| anyhow::anyhow!("Task join error: {}", e))?
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

        // Never let pnpm prompt (e.g. to approve versions that haven't met minimumReleaseAge):
        // there is nobody to answer, and the prompt would block forever.
        cmd.stdin(std::process::Stdio::null());

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

        // Never let pnpm prompt (e.g. to approve versions that haven't met minimumReleaseAge):
        // there is nobody to answer, and the prompt would block forever.
        cmd.stdin(std::process::Stdio::null());

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_release_age_violations() {
        // Output of pnpm 12.9.0, including a line that wrapped mid-timestamp.
        let output = "Error: ERR_PNPM_MINIMUM_RELEASE_AGE_VIOLATION\n\n  × installing dependencies\n  ╰─▶ 3 versions do not meet the minimumReleaseAge constraint:\n        @next/swc-win32-x64-msvc@15.5.27 was published at 2026-09-\n      30T16:06:43.442Z, within the minimumReleaseAge cutoff (2026-04-\n      10T23:16:41.427Z)\n        next@15.5.27 was published at 2026-09-30T16:19:50.862Z, within the\n      minimumReleaseAge cutoff (2026-04-10T23:16:41.427Z)\n        caniuse-lite@1.0.30001815 was published at 2026-10-07T09:15:10.000Z,\n";
        assert_eq!(
            parse_release_age_violations(output),
            vec![
                (
                    "@next/swc-win32-x64-msvc".to_string(),
                    "15.5.27".to_string()
                ),
                ("next".to_string(), "15.5.27".to_string()),
                ("caniuse-lite".to_string(), "1.0.30001815".to_string()),
            ]
        );
    }

    /// Runs real pnpm against the npm registry: `cargo test -- --ignored release_age_closure`.
    #[tokio::test]
    #[ignore]
    async fn release_age_closure_covers_lockstep_siblings() {
        let dir = tempfile::TempDir::new().unwrap();
        let package_json = r#"{"name":"t","version":"1.0.0","dependencies":{"next":"15.5.27","react":"19.0.0","react-dom":"19.0.0"}}"#;
        fs::write(dir.path().join("package.json"), package_json).unwrap();

        // A one-year gate makes `next` and its `@next/swc-*` siblings immature.
        let workspace = "# policy\nminimumReleaseAge: 525600\n";
        let original_pkg_jsons = std::collections::HashMap::new();
        let pkg_jsons = std::collections::HashMap::new();
        let before_versions = std::collections::HashMap::new();
        let ctx = AuditContext {
            temp_dir: dir.path(),
            is_workspace: false,
            workspace: Some(workspace),
            original_pkg_jsons: &original_pkg_jsons,
            pkg_jsons: &pkg_jsons,
            baseline_lock: None,
            before_versions: &before_versions,
            minimum_release_age: effective_minimum_release_age(Some(workspace)),
        };
        let renderer = WorkspaceRenderer {
            base: Some(workspace.to_string()),
            overrides: None,
        };
        let mut publish_times = PublishTimes::new(clients::npm::Npm::new(
            crate::core::http_agent::HttpAgent::new(),
        ));

        let mut exclusions = PinnedVersions::new();
        exclusions
            .entry("next".to_string())
            .or_default()
            .insert("15.5.27".to_string());

        assert!(run_with_release_age_exceptions(
            &ctx,
            &renderer,
            &mut exclusions,
            &mut publish_times,
            &install_args(false, &[]),
        )
        .await
        .unwrap());
        assert!(exclusions
            .get("@next/swc-linux-x64-gnu")
            .is_some_and(|v| v.contains("15.5.27")));

        let verify = run_pnpm(dir.path(), &frozen_install_args(false))
            .await
            .unwrap();
        assert!(verify.success, "{}", verify.text);

        let written = fs::read_to_string(dir.path().join("pnpm-workspace.yaml")).unwrap();
        assert!(
            written.starts_with("# policy\nminimumReleaseAge: 525600\nminimumReleaseAgeExclude:\n")
        );
        eprintln!("{}", written);
    }

    #[test]
    fn defaults_minimum_release_age_to_pnpm_default() {
        assert_eq!(
            effective_minimum_release_age(None),
            chrono::Duration::minutes(1440)
        );
        assert_eq!(
            effective_minimum_release_age(Some("packages:\n  - apps/*\n")),
            chrono::Duration::minutes(1440)
        );
        assert_eq!(
            effective_minimum_release_age(Some("minimumReleaseAge: 4320\n")),
            chrono::Duration::minutes(4320)
        );
    }
}
