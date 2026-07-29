use anyhow::{Context, Result};
use std::collections::HashMap;
use turso::params;

use crate::core::actions::analyze_project_dependencies::{
    AnalyzedProjectDependencies, AnalyzedProjectDependency,
};
use crate::core::database::{pk, Bump, BumpDep, Database, Dependency, Package, Project};
use crate::core::event::Op;
use crate::core::platform::Platform;

#[derive(Clone)]
pub struct Store {
    database: Database,
}

#[derive(Clone)]
pub struct VacuumStats {
    pub scans_deleted: usize,
    pub dependencies_deleted: usize,
    pub packages_deleted: usize,
}

impl Store {
    pub fn new(database: Database) -> Self {
        Self { database }
    }

    pub fn database(&self) -> &Database {
        &self.database
    }

    pub async fn list_projects(&self) -> Result<Vec<Project>> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare("SELECT id, platform, repository FROM project")
            .await?;

        let mut rows = stmt.query(()).await?;

        let mut projects = Vec::new();
        while let Some(row) = rows.next().await? {
            let platform: String = row.get(1).context("Missing platform column")?;
            projects.push(Project {
                id: row.get(0).unwrap_or_default(),
                platform: platform.parse().context("Invalid platform in database")?,
                repository: row.get(2).unwrap_or_default(),
            });
        }

        Ok(projects)
    }

    pub async fn project(&self, project_id: &str) -> Result<Project> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare("SELECT id, platform, repository FROM project WHERE id = ?1")
            .await?;

        let mut rows = stmt.query((project_id,)).await?;

        if let Some(row) = rows.next().await? {
            let platform: String = row.get(1).context("Missing platform column")?;
            return Ok(Project {
                id: row.get(0).unwrap_or_default(),
                platform: platform.parse().context("Invalid platform in database")?,
                repository: row.get(2).unwrap_or_default(),
            });
        }

        Err(anyhow::anyhow!("Project not found"))
    }

    pub async fn add_project(&self, platform: Platform, repository: String) -> Result<Project> {
        let conn = self.database.conn()?;
        let id = pk();

        conn.execute(
            "INSERT INTO project (id, platform, repository) VALUES (?, ?, ?)",
            params![id.as_str(), platform.as_str(), repository.as_str()],
        )
        .await
        .context("Failed to insert project")?;

        Ok(Project {
            id,
            platform,
            repository,
        })
    }

    pub async fn remove_project(&self, platform: Platform, repository: String) -> Result<Vec<Op>> {
        let conn = self.database.conn()?;

        // Find the project
        let mut stmt = conn
            .prepare("SELECT id FROM project WHERE platform = ?1 AND repository = ?2")
            .await?;

        let mut rows = stmt.query((platform.as_str(), repository.as_str())).await?;

        let project_id = if let Some(row) = rows.next().await? {
            row.get_value(0)?
                .as_text()
                .context("project id should be text")?
                .to_string()
        } else {
            return Err(anyhow::anyhow!("Project not found"));
        };

        let mut ops = Vec::new();

        // 1. Delete bumpdeps and bumps
        let mut bumps_stmt = conn
            .prepare("SELECT id FROM bump WHERE project_id = ?1")
            .await?;
        let mut bumps_rows = bumps_stmt.query((project_id.as_str(),)).await?;
        let mut bump_ids = Vec::new();
        while let Some(row) = bumps_rows.next().await? {
            bump_ids.push(row.get_value(0)?.as_text().unwrap().to_string());
        }

        for bump_id in &bump_ids {
            let mut bumpdeps_stmt = conn
                .prepare("SELECT dependency_id FROM bumpdep WHERE bump_id = ?1")
                .await?;
            let mut bumpdeps_rows = bumpdeps_stmt.query((bump_id.as_str(),)).await?;
            while let Some(dep_row) = bumpdeps_rows.next().await? {
                let dep_id = dep_row.get_value(0)?.as_text().unwrap().to_string();
                ops.push(Op::Delete {
                    path: format!("bumpdep/{}/{}", bump_id, dep_id),
                });
            }
            ops.push(Op::Delete {
                path: format!("bump/{}", bump_id),
            });
        }
        conn.execute(
            "DELETE FROM bumpdep WHERE bump_id IN (SELECT id FROM bump WHERE project_id = ?1)",
            params![project_id.as_str()],
        )
        .await?;
        conn.execute(
            "DELETE FROM bump WHERE project_id = ?1",
            params![project_id.as_str()],
        )
        .await?;

        // 2. Delete scans and dependencies
        let mut scans_stmt = conn
            .prepare("SELECT id FROM scan WHERE project_id = ?1")
            .await?;
        let mut scans_rows = scans_stmt.query((project_id.as_str(),)).await?;
        while let Some(row) = scans_rows.next().await? {
            let scan_id = row.get_value(0)?.as_text().unwrap().to_string();

            let mut deps_stmt = conn
                .prepare("SELECT id FROM dependency WHERE scan_id = ?1")
                .await?;
            let mut deps_rows = deps_stmt.query((scan_id.as_str(),)).await?;
            while let Some(dep_row) = deps_rows.next().await? {
                let dep_id = dep_row.get_value(0)?.as_text().unwrap().to_string();
                ops.push(Op::Delete {
                    path: format!("dependency/{}", dep_id),
                });
            }
            ops.push(Op::Delete {
                path: format!("scan/{}", scan_id),
            });
        }
        conn.execute(
            "DELETE FROM dependency WHERE scan_id IN (SELECT id FROM scan WHERE project_id = ?1)",
            params![project_id.as_str()],
        )
        .await?;
        conn.execute(
            "DELETE FROM scan WHERE project_id = ?1",
            params![project_id.as_str()],
        )
        .await?;

        // 3. Delete project
        ops.push(Op::Delete {
            path: format!("project/{}", project_id),
        });
        conn.execute(
            "DELETE FROM project WHERE id = ?1",
            params![project_id.as_str()],
        )
        .await?;

        Ok(ops)
    }

    pub async fn list_dependencies(&self) -> Result<Vec<Dependency>> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare("SELECT id, scan_id, specifier, package_id FROM dependency")
            .await?;

        let mut rows = stmt.query(()).await?;

        let mut dependencies = Vec::new();
        while let Some(row) = rows.next().await? {
            dependencies.push(Dependency {
                id: row.get(0).unwrap_or_default(),
                scan_id: row.get(1).unwrap_or_default(),
                specifier: row.get(2).unwrap_or_default(),
                package_id: row.get(3).unwrap_or_default(),
            });
        }

        Ok(dependencies)
    }

    pub async fn list_packages(&self) -> Result<Vec<Package>> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare("SELECT id, type, namespace, name, version, subpath FROM package")
            .await?;

        let mut rows = stmt.query(()).await?;

        let mut packages = Vec::new();
        while let Some(row) = rows.next().await? {
            packages.push(Package {
                id: row.get(0).unwrap_or_default(),
                r#type: row.get(1).unwrap_or_default(),
                namespace: row.get(2).unwrap_or_default(),
                name: row.get(3).unwrap_or_default(),
                version: row.get(4).unwrap_or_default(),
                subpath: row.get(5).unwrap_or_default(),
            });
        }

        Ok(packages)
    }

    pub async fn list_bumps(&self) -> Result<Vec<Bump>> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare("SELECT id, project_id, name, major, approved, url FROM bump")
            .await?;

        let mut rows = stmt.query(()).await?;

        let mut bumps = Vec::new();
        while let Some(row) = rows.next().await? {
            bumps.push(Bump {
                id: row.get(0).unwrap_or_default(),
                project_id: row.get(1).unwrap_or_default(),
                name: row.get(2).unwrap_or_default(),
                major: row.get(3).unwrap_or_default(),
                approved: row.get(4).unwrap_or_default(),
                url: row.get(5).unwrap_or_default(),
            });
        }

        Ok(bumps)
    }

    pub async fn bump(&self, bump_id: &str) -> Result<Bump> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare("SELECT id, project_id, name, major, approved, url FROM bump WHERE id = ?1")
            .await?;

        let mut rows = stmt.query((bump_id,)).await?;

        if let Some(row) = rows.next().await? {
            return Ok(Bump {
                id: row.get(0).unwrap_or_default(),
                project_id: row.get(1).unwrap_or_default(),
                name: row.get(2).unwrap_or_default(),
                major: row.get(3).unwrap_or_default(),
                approved: row.get(4).unwrap_or_default(),
                url: row.get(5).unwrap_or_default(),
            });
        }

        Err(anyhow::anyhow!("Bump not found"))
    }

    pub async fn bump_by_url(&self, url: &str) -> Result<Option<Bump>> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare("SELECT id, project_id, name, major, approved, url FROM bump WHERE url = ?1")
            .await?;

        let mut rows = stmt.query((url,)).await?;

        if let Some(row) = rows.next().await? {
            return Ok(Some(Bump {
                id: row.get(0).unwrap_or_default(),
                project_id: row.get(1).unwrap_or_default(),
                name: row.get(2).unwrap_or_default(),
                major: row.get(3).unwrap_or_default(),
                approved: row.get(4).unwrap_or_default(),
                url: row.get(5).unwrap_or_default(),
            }));
        }

        Ok(None)
    }

    pub async fn remove_bump(&self, bump_id: &str) -> Result<Vec<Op>> {
        let conn = self.database.conn()?;
        let mut ops = Vec::new();

        let mut bumpdeps_stmt = conn
            .prepare("SELECT dependency_id FROM bumpdep WHERE bump_id = ?1")
            .await?;
        let mut bumpdeps_rows = bumpdeps_stmt.query((bump_id,)).await?;
        while let Some(dep_row) = bumpdeps_rows.next().await? {
            let dep_id = dep_row.get_value(0)?.as_text().unwrap().to_string();
            ops.push(Op::Delete {
                path: format!("bumpdep/{}/{}", bump_id, dep_id),
            });
        }
        ops.push(Op::Delete {
            path: format!("bump/{}", bump_id),
        });

        conn.execute(
            "DELETE FROM bumpdep WHERE bump_id = ?1",
            params![bump_id],
        )
        .await?;
        conn.execute("DELETE FROM bump WHERE id = ?1", params![bump_id])
            .await?;

        Ok(ops)
    }

    pub async fn vacuum(&self) -> Result<Vec<Op>> {
        let conn = self.database.conn()?;
        let mut ops = Vec::new();

        conn.execute("BEGIN TRANSACTION", params![]).await?;

        let mut stats = VacuumStats {
            scans_deleted: 0,
            dependencies_deleted: 0,
            packages_deleted: 0,
        };

        let result: Result<Vec<Op>> = async {
            let mut obsolete_scan_ids = Vec::new();
            {
                let mut stmt = conn.prepare("SELECT id FROM project").await?;
                let mut project_rows = stmt.query(()).await?;
                while let Some(row) = project_rows.next().await? {
                    let project_id: String = row.get_value(0)?.as_text().unwrap().to_string();

                    let mut scan_stmt = conn
                        .prepare(
                            "SELECT id FROM scan WHERE project_id = ? ORDER BY create_time DESC",
                        )
                        .await?;
                    let mut scan_rows = scan_stmt.query((project_id.as_str(),)).await?;

                    let mut latest_scan_id = None;
                    let mut all_scans_in_project = Vec::new();

                    while let Some(row) = scan_rows.next().await? {
                        let sid: String = row.get_value(0)?.as_text().unwrap().to_string();
                        if latest_scan_id.is_none() {
                            latest_scan_id = Some(sid.clone());
                        }
                        all_scans_in_project.push(sid);
                    }

                    if let Some(latest) = latest_scan_id {
                        for sid in all_scans_in_project {
                            if sid != latest {
                                obsolete_scan_ids.push(sid.clone());
                            }
                        }
                    }
                }
            }

            stats.scans_deleted = obsolete_scan_ids.len();

            let mut deps_to_delete = std::collections::HashSet::new();
            let mut scans_to_delete = Vec::new();

            for scan_id in &obsolete_scan_ids {
                let mut deps_stmt = conn
                    .prepare("SELECT id FROM dependency WHERE scan_id = ?")
                    .await?;
                let mut deps_rows = deps_stmt.query((scan_id.as_str(),)).await?;
                while let Some(dep_row) = deps_rows.next().await? {
                    deps_to_delete.insert(dep_row.get_value(0)?.as_text().unwrap().to_string());
                }
                scans_to_delete.push(scan_id.clone());
            }

            for dep_id in deps_to_delete {
                stats.dependencies_deleted += 1;
                ops.push(Op::Delete {
                    path: format!("dependency/{}", dep_id),
                });
                conn.execute(
                    "DELETE FROM dependency WHERE id = ?",
                    params![dep_id.as_str()],
                )
                .await?;
            }

            for scan_id in scans_to_delete {
                ops.push(Op::Delete {
                    path: format!("scan/{}", scan_id),
                });
                conn.execute("DELETE FROM scan WHERE id = ?", params![scan_id.as_str()])
                    .await?;
            }

            let mut unused_pkg_stmt = conn
                .prepare(
                    "SELECT id FROM package WHERE id NOT IN (SELECT package_id FROM dependency)",
                )
                .await?;
            let mut unused_pkg_rows = unused_pkg_stmt.query(()).await?;
            let mut pkg_to_delete = Vec::new();
            while let Some(row) = unused_pkg_rows.next().await? {
                pkg_to_delete.push(row.get_value(0)?.as_text().unwrap().to_string());
            }

            for pkg_id in pkg_to_delete {
                stats.packages_deleted += 1;
                ops.push(Op::Delete {
                    path: format!("package/{}", pkg_id),
                });
                conn.execute("DELETE FROM package WHERE id = ?", params![pkg_id.as_str()])
                    .await?;
            }

            Ok(ops)
        }
        .await;

        match result {
            Ok(ops) => {
                conn.execute("COMMIT", params![]).await?;

                tracing::info!(
                    "Vacuum completed: {} scans, {} dependencies. {} packages",
                    stats.scans_deleted,
                    stats.dependencies_deleted,
                    stats.packages_deleted
                );

                Ok(ops)
            }
            Err(e) => {
                conn.execute("ROLLBACK", params![]).await?;
                Err(e)
            }
        }
    }

    pub async fn list_bumpdeps(&self) -> Result<Vec<BumpDep>> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare("SELECT bump_id, dependency_id, target_version, head_version, minimum_release_age FROM bumpdep")
            .await?;

        let mut rows = stmt.query(()).await?;

        let mut bumpdeps = Vec::new();
        while let Some(row) = rows.next().await? {
            bumpdeps.push(BumpDep {
                bump_id: row.get(0).unwrap_or_default(),
                dependency_id: row.get(1).unwrap_or_default(),
                target_version: row.get(2).unwrap_or_default(),
                head_version: row.get(3).unwrap_or_default(),
                minimum_release_age: row.get(4).unwrap_or_default(),
            });
        }

        Ok(bumpdeps)
    }

    pub async fn bump_targets(&self, bump_id: &str) -> Result<Vec<BumpTargetData>> {
        let conn = self.database.conn()?;

        let mut stmt = conn
            .prepare(
                "SELECT p.name, d.specifier, p.version, p.type, bd.target_version, bd.head_version, NULL as repo_url, p.namespace, p.subpath, bd.minimum_release_age
                 FROM bumpdep bd
                 JOIN dependency d ON bd.dependency_id = d.id
                 JOIN package p ON d.package_id = p.id
                 WHERE bd.bump_id = ?1"
            )
            .await?;

        let mut rows = stmt.query((bump_id,)).await?;
        let mut targets = Vec::new();

        while let Some(row) = rows.next().await? {
            let minimum_release_age_secs: Option<i64> = row.get(9).unwrap_or_default();
            targets.push(BumpTargetData {
                name: row.get(0).unwrap_or_default(),
                specifier: row.get(1).unwrap_or_default(),
                current_version: row.get(2).unwrap_or_default(),
                eco_type: row.get(3).unwrap_or_default(),
                target_version: row.get(4).unwrap_or_default(),
                head_version: row.get(5).unwrap_or_default(),
                repo_url: row.get(6).unwrap_or_default(),
                namespace: row.get(7).unwrap_or_default(),
                subpath: row.get(8).unwrap_or_default(),
                minimum_release_age: minimum_release_age_secs.map(chrono::Duration::seconds),
            });
        }

        Ok(targets)
    }

    pub async fn persist_bump_result(
        &self,
        bump_id: &str,
        pull_request_url: Option<String>,
    ) -> Result<Bump> {
        let conn = self.database.conn()?;
        if let Some(url) = pull_request_url {
            conn.execute(
                "UPDATE bump SET url = ? WHERE id = ?",
                turso::params![url, bump_id],
            )
            .await?;
        }

        self.bump(bump_id).await
    }

    pub async fn clear_bump_url(&self, bump_id: &str) -> Result<Bump> {
        let conn = self.database.conn()?;
        conn.execute("UPDATE bump SET url = NULL WHERE id = ?", params![bump_id])
            .await?;
        self.bump(bump_id).await
    }

    pub async fn approve_bump(&self, bump_id: &str) -> Result<Bump> {
        let conn = self.database.conn()?;
        conn.execute(
            "UPDATE bump SET approved = 1 WHERE id = ?",
            params![bump_id],
        )
        .await?;
        self.bump(bump_id).await
    }

    pub async fn retract_bump_approval(&self, bump_id: &str) -> Result<Bump> {
        let conn = self.database.conn()?;
        conn.execute(
            "UPDATE bump SET approved = 0 WHERE id = ?",
            params![bump_id],
        )
        .await?;
        self.bump(bump_id).await
    }

    pub async fn persist_analyzed_project_dependencies(
        &self,
        project_id: &str,
        scan_result: AnalyzedProjectDependencies,
    ) -> Result<Vec<Op>> {
        let mut ops = Vec::new();
        let scan_id = pk();
        let now = chrono::Utc::now().to_rfc3339();

        let conn = self.database.conn()?;

        conn.execute(
            "INSERT INTO scan (id, project_id, create_time) VALUES (?, ?, ?)",
            params![scan_id.as_str(), project_id, now],
        )
        .await
        .context("Failed to insert scan")?;

        let mut success_count = 0;

        let mut existing_bumps_query = conn
            .query(
                "SELECT id, name, major FROM bump WHERE project_id = ?",
                params![project_id],
            )
            .await?;
        let mut existing_bumps: HashMap<String, [Option<String>; 2]> = HashMap::new();
        let mut bump_ids_to_wipe = Vec::new();
        while let Some(row) = existing_bumps_query.next().await? {
            let id = row.get_value(0)?.as_text().unwrap().to_string();
            let name = row.get_value(1)?.as_text().unwrap().to_string();
            let major = *row.get_value(2)?.as_integer().unwrap_or(&0) != 0;
            existing_bumps.entry(name).or_insert([None, None])[major as usize] = Some(id.clone());
            bump_ids_to_wipe.push(id);
        }

        for b_id in bump_ids_to_wipe {
            let mut bumpdeps_to_delete_query = conn
                .query(
                    "SELECT dependency_id FROM bumpdep WHERE bump_id = ?",
                    params![b_id.clone()],
                )
                .await?;
            while let Some(row) = bumpdeps_to_delete_query.next().await? {
                let dep_id = row.get_value(0)?.as_text().unwrap().to_string();
                ops.push(Op::Delete {
                    path: format!("bumpdep/{}/{}", b_id, dep_id),
                });
            }
            conn.execute("DELETE FROM bumpdep WHERE bump_id = ?", params![b_id])
                .await?;
        }

        let mut bump_cache: HashMap<String, [Option<String>; 2]> = HashMap::new();

        for res in scan_result.analyzed_project_dependencies {
            let group_name = res.group_name();

            let AnalyzedProjectDependency {
                discovered_dependency,
                dependency_update_options,
            } = res;

            let r#type = &discovered_dependency.purl.ecosystem;
            let namespace = &discovered_dependency.purl.namespace;
            let db_name = &discovered_dependency.purl.name;
            let subpath = &discovered_dependency.purl.subpath;
            let locked_version = &discovered_dependency.purl.version;
            let req = &discovered_dependency.requirement;
            let min_release_age = discovered_dependency.minimum_release_age;

            {
                let mut latest_allowed = "0.0.0".to_string();
                if let Some(first_bump) = dependency_update_options.bumps.first() {
                    latest_allowed = first_bump.target_version.clone();
                }

                let pkg_version = locked_version.clone().unwrap_or(latest_allowed.clone());
                let eco_name = &r#type;
                let mut pkg_query = conn.query(
                    "SELECT id FROM package WHERE type = ? AND namespace IS ? AND name = ? AND subpath IS ? AND version = ?",
                    params![eco_name.as_str(), namespace.as_deref(), db_name.as_str(), subpath.as_deref(), pkg_version.as_str()]
                ).await?;

                let pkg_id = if let Some(row) = pkg_query.next().await? {
                    row.get_value(0)?
                        .as_text()
                        .context("package id should be text")?
                        .to_string()
                } else {
                    let new_pkg_id = pk();
                    conn.execute(
                        "INSERT INTO package (id, type, namespace, name, subpath, version) VALUES (?, ?, ?, ?, ?, ?)",
                        params![new_pkg_id.as_str(), eco_name.as_str(), namespace.as_deref(), db_name.as_str(), subpath.as_deref(), pkg_version.as_str()]
                    ).await?;

                    ops.push(Op::Upsert {
                        path: format!("package/{}", new_pkg_id),
                        data: serde_json::json!({
                            "id": new_pkg_id,
                            "type": eco_name,
                            "namespace": namespace,
                            "name": db_name,
                            "subpath": subpath,
                            "version": pkg_version
                        }),
                    });

                    new_pkg_id
                };

                let dep_id = pk();
                conn.execute(
                    "INSERT INTO dependency (id, scan_id, specifier, package_id) VALUES (?, ?, ?, ?)",
                    params![dep_id.as_str(), scan_id.as_str(), req.as_str(), pkg_id.clone()],
                )
                .await?;

                ops.push(Op::Upsert {
                    path: format!("dependency/{}", dep_id),
                    data: serde_json::json!({
                        "id": dep_id,
                        "scan_id": scan_id,
                        "specifier": req,
                        "package_id": pkg_id
                    }),
                });

                success_count += 1;

                let mut bumps_to_process = Vec::new();
                for bump in &dependency_update_options.bumps {
                    let bump_version = bump.target_version.clone();
                    bumps_to_process.push((bump_version, bump.is_major, bump.head_version.clone()));
                }

                for (bump_version, bump_is_major, head_ver) in bumps_to_process {
                    let bump_id = if let Some(id) = existing_bumps
                        .get(group_name.as_str())
                        .and_then(|m| m[bump_is_major as usize].as_ref())
                    {
                        id.clone()
                    } else if let Some(id) = bump_cache
                        .get(group_name.as_str())
                        .and_then(|m| m[bump_is_major as usize].as_ref())
                    {
                        id.clone()
                    } else {
                        let new_bump_id = pk();
                        conn.execute(
                            "INSERT INTO bump (id, project_id, name, major, approved) VALUES (?, ?, ?, ?, 0)",
                            params![new_bump_id.as_str(), project_id, group_name.as_str(), bump_is_major]
                        ).await?;

                        ops.push(Op::Upsert {
                            path: format!("bump/{}", new_bump_id),
                            data: serde_json::json!({
                                "id": new_bump_id,
                                "project_id": project_id,
                                "name": group_name,
                                "major": bump_is_major,
                                "approved": false,
                                "url": null
                            }),
                        });

                        bump_cache.entry(group_name.clone()).or_insert([None, None])
                            [bump_is_major as usize] = Some(new_bump_id.clone());
                        new_bump_id
                    };

                    let target_ver = bump_version.clone();
                    let min_age_secs = min_release_age.map(|d| d.num_seconds());

                    conn.execute(
                        "INSERT INTO bumpdep (bump_id, dependency_id, target_version, head_version, minimum_release_age) VALUES (?, ?, ?, ?, ?)",
                        params![bump_id.clone(), dep_id.as_str(), target_ver.clone(), head_ver.clone(), min_age_secs]
                    ).await?;

                    ops.push(Op::Upsert {
                        path: format!("bumpdep/{}/{}", bump_id, dep_id),
                        data: serde_json::json!({
                            "bump_id": bump_id,
                            "dependency_id": dep_id,
                            "target_version": target_ver,
                            "head_version": head_ver,
                            "minimum_release_age": min_age_secs
                        }),
                    });
                }
            }
        }

        let mut bumps_to_delete_query = conn
            .query(
                "SELECT id FROM bump WHERE project_id = ? AND id NOT IN (SELECT bump_id FROM bumpdep)",
                params![project_id],
            )
            .await?;
        while let Some(row) = bumps_to_delete_query.next().await? {
            let id = row.get_value(0)?.as_text().unwrap().to_string();
            ops.push(Op::Delete {
                path: format!("bump/{}", id),
            });
        }

        conn.execute(
            "DELETE FROM bump WHERE project_id = ? AND id NOT IN (SELECT bump_id FROM bumpdep)",
            params![project_id],
        )
        .await?;

        let msg = format!(
            "Scan complete. Inserted {} dependencies (found {} potential bumps).",
            success_count,
            bump_cache.len() + existing_bumps.len()
        );
        tracing::info!("{}", msg);

        Ok(ops)
    }
}

pub struct BumpTargetData {
    pub name: String,
    pub specifier: String,
    pub current_version: String,
    pub eco_type: String,
    pub target_version: String,
    pub head_version: String,
    pub repo_url: Option<String>,
    pub namespace: Option<String>,
    pub subpath: Option<String>,
    pub minimum_release_age: Option<chrono::Duration>,
}
