use crate::core::application::Application;
use crate::core::engine::{PackageInfo, RequirementVersion, UpdateTarget};
use crate::core::message::Payload;
use anyhow::Result;
use tempfile::TempDir;

pub async fn run(app: &Application, bump_id: String) -> Result<()> {
    let bump = app.query().bump(&bump_id).await?;
    let project = app.query().project(&bump.project_id).await?;

    let bump_targets_data = app.query().bump_targets(&bump_id).await?;
    if bump_targets_data.is_empty() {
        tracing::warn!("No bump targets found for bump {}", bump_id);
        return Ok(());
    }

    let eco_type = bump_targets_data[0].eco_type.clone();
    let patcher = match app.patcher(&eco_type) {
        Some(p) => p,
        None => {
            tracing::error!("No patcher found for ecosystem {}", eco_type);
            anyhow::bail!("No patcher found for ecosystem {}", eco_type);
        }
    };

    let mut targets = Vec::new();
    for row in bump_targets_data {
        let mut full_name = match row.namespace {
            Some(ns) => format!("{}/{}", ns, row.name),
            None => row.name,
        };
        if let Some(sp) = row.subpath {
            full_name = format!("{}/{}", full_name, sp);
        }

        if let Some(new_req) = patcher.updated_requirement(&row.specifier, &row.target_version) {
            targets.push(UpdateTarget {
                name: full_name,
                current_version: RequirementVersion {
                    requirement: row.specifier,
                    version: row.current_version,
                },
                target_version: RequirementVersion {
                    requirement: new_req,
                    version: row.target_version,
                },
                latest_version: row.head_version,
                package_info: PackageInfo {
                    repo_url: row.repo_url,
                },
                minimum_release_age: row.minimum_release_age,
            });
        }
    }

    if targets.is_empty() {
        tracing::info!(
            "No changes detected. Target '{}' is already up to date.",
            bump.name
        );
        return Ok(());
    }

    let handle = app.handle();
    let pr_generator = app.pull_request_generator();
    let view = app.project_repository_view(&project.id).await?;
    let default_branch = view.get_default_branch().await?;
    let base_revision = view.get_revision(&default_branch).await?;
    let mutator = app.project_repository_mutator(&project.id).await?;
    let snapshot = view.snapshot(&base_revision);
    let is_major = bump.major;
    let bump_name = bump.name.clone();

    tokio::spawn(async move {
        // 1. Prepare temporary directory
        let temp_dir_result = TempDir::new();
        if let Err(e) = temp_dir_result {
            tracing::error!("Failed to create temporary directory: {}", e);
            return;
        }
        let temp_dir = temp_dir_result.unwrap();

        // 2. Apply updates (via Patcher)
        let modifications = match patcher
            .apply_updates(snapshot.as_ref(), temp_dir.path(), &targets)
            .await
        {
            Ok(mods) => mods,
            Err(e) => {
                tracing::error!("Failed to apply updates: {}", e);
                return;
            }
        };

        if modifications.is_empty() {
            tracing::info!("Modifications resulted in no changes. Skip creating PR.");
            // Send empty result
            let _ = handle
                .send(
                    crate::core::database::pk(),
                    Payload::PersistBumpResult {
                        bump_id: bump_id.clone(),
                        pull_request_url: None,
                    },
                )
                .await;
            return;
        }

        // 3. Form branch name and PR details
        let safe_branch_name = bump_name
            .chars()
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect::<String>();
        let safe_branch_name = safe_branch_name
            .replace("--", "-")
            .trim_matches('-')
            .to_string();
        let safe_branch_name = if is_major {
            format!("{}-major", safe_branch_name)
        } else {
            safe_branch_name
        };
        let branch_name = format!("syz/update-{}", safe_branch_name);

        let pr_url_opt = match async {
            let title = pr_generator.generate_pull_request_title(&bump_name, &targets, is_major)?;
            let body = pr_generator.generate_pull_request_body(&bump_name, &targets, is_major)?;

            // 4. Commit changes
            if let Some(_sha) = mutator
                .commit_changes(&base_revision, &branch_name, &title, modifications)
                .await?
            {
                let url = mutator
                    .create_pull_request(&title, &branch_name, &default_branch, &body)
                    .await?;
                Ok::<Option<String>, anyhow::Error>(Some(url))
            } else {
                tracing::info!("Modifications resulted in no changes relative to base branch. Cleaning up empty PRs.");
                let _ = mutator.close_pull_request(&branch_name, &default_branch).await;
                Ok(None)
            }
        }.await {
            Ok(url) => url,
            Err(e) => {
                tracing::error!("Failed to process branch/PR generation: {:?}", e);
                None
            }
        };

        // 5. Send result message
        let result_payload = Payload::PersistBumpResult {
            bump_id: bump_id.clone(),
            pull_request_url: pr_url_opt,
        };

        if let Err(e) = handle
            .send(crate::core::database::pk(), result_payload)
            .await
        {
            tracing::error!("Failed to send PersistBumpResult: {}", e);
        }
    });

    Ok(())
}
