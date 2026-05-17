use anyhow::{Context, Result};
use std::collections::HashMap;
use turso::params;

use crate::core::actions::analyze_project_dependencies::{
    AnalyzedProjectDependencies, AnalyzedProjectDependency,
};
use crate::core::database::{pk, Bump, BumpDep, Database, Dependency, Package, Project};
use crate::core::event::Op;

#[derive(Clone)]
pub struct Store {
    database: Database,
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
            projects.push(Project {
                id: row.get(0).unwrap_or_default(),
                platform: row.get(1).unwrap_or_default(),
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
            return Ok(Project {
                id: row.get(0).unwrap_or_default(),
                platform: row.get(1).unwrap_or_default(),
                repository: row.get(2).unwrap_or_default(),
            });
        }

        Err(anyhow::anyhow!("Project not found"))
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
                    let min_age_mins = min_release_age.map(|d| d.num_minutes());

                    conn.execute(
                        "INSERT INTO bumpdep (bump_id, dependency_id, target_version, head_version, minimum_release_age) VALUES (?, ?, ?, ?, ?)",
                        params![bump_id.clone(), dep_id.as_str(), target_ver.clone(), head_ver.clone(), min_age_mins]
                    ).await?;

                    ops.push(Op::Upsert {
                        path: format!("bumpdep/{}/{}", bump_id, dep_id),
                        data: serde_json::json!({
                            "bump_id": bump_id,
                            "dependency_id": dep_id,
                            "target_version": target_ver,
                            "head_version": head_ver,
                            "minimum_release_age": min_age_mins
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
