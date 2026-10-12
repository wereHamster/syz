use super::{Change, DefaultBranch};
use crate::core::engine::ecosystems::Patcher;
use crate::core::engine::{PackageInfo, RequirementVersion, UpdateTarget};
use crate::core::job::LaneKey;
use crate::core::message::Payload;
use crate::core::services::Services;
use crate::core::store::BumpTargetData;
use anyhow::{Context, Result};

/// Create or update the bump's pull request, or close it if there is nothing to do.
///
/// The bump is read here rather than when the job was enqueued, so that a bump that was
/// removed or retracted meanwhile isn't published, and a rescan's newer targets are used.
pub async fn run(services: &Services, lane: LaneKey, bump_id: String) -> Result<Option<Payload>> {
    let store = services.store();

    let Some(bump) = store.find_bump(&bump_id).await? else {
        tracing::info!("Bump {} no longer exists, not publishing it", bump_id);
        return Ok(None);
    };
    if !bump.approved {
        tracing::info!(
            "Bump {} ({}) is no longer approved, not publishing it",
            bump.name,
            bump_id
        );
        return Ok(None);
    }

    let bump_targets_data = store.bump_targets(&bump_id).await?;
    let eco_type = bump_targets_data
        .first()
        .with_context(|| format!("Bump {} ({}) has no targets", bump.name, bump_id))?
        .eco_type
        .clone();
    let patcher = services
        .patcher(&eco_type)
        .with_context(|| format!("No patcher found for ecosystem {}", eco_type))?;

    let targets = build_update_targets(patcher.as_ref(), bump_targets_data);
    let branch_name = generate_branch_name(&bump.name, bump.major);

    let default_branch = DefaultBranch::resolve(services.repository(&lane)?.as_ref()).await?;

    let modifications = if targets.is_empty() {
        tracing::info!("Bump {} is already up to date", bump.name);
        Vec::new()
    } else {
        let temp_dir = super::create_temp_dir(services.work_dir())?;
        patcher
            .apply_updates(default_branch.snapshot.as_ref(), temp_dir.path(), &targets)
            .await
            .with_context(|| format!("Failed to apply the updates of bump {}", bump.name))?
    };

    // Generating the title and body makes registry, advisory and GitHub requests, which
    // are wasted when there is nothing to publish.
    let change = if modifications.is_empty() {
        None
    } else {
        let pr_generator = services.pull_request_generator();
        let title = pr_generator
            .generate_pull_request_title(
                &bump.name,
                &eco_type,
                &targets,
                bump.major,
                default_branch.snapshot.as_ref(),
            )
            .await?;
        let body = pr_generator
            .generate_pull_request_body(&bump.name, &eco_type, &targets, bump.major)
            .await?;

        Some(Change {
            title,
            body,
            modifications,
        })
    };

    let outcome = default_branch.publish(&branch_name, change).await?;

    Ok(Some(Payload::BumpPublished {
        lane,
        project_id: bump.project_id,
        bump_id,
        branch_name,
        outcome,
    }))
}

pub fn generate_branch_name(bump_name: &str, is_major: bool) -> String {
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
    let mut safe_branch_name = safe_branch_name;
    while safe_branch_name.contains("--") {
        safe_branch_name = safe_branch_name.replace("--", "-");
    }
    let safe_branch_name = safe_branch_name.trim_matches('-').to_string();
    let safe_branch_name = if is_major {
        format!("{}-major", safe_branch_name)
    } else {
        safe_branch_name
    };

    format!("syz/update-{}", safe_branch_name)
}

fn build_update_targets(patcher: &dyn Patcher, rows: Vec<BumpTargetData>) -> Vec<UpdateTarget> {
    let mut targets = Vec::new();

    for row in rows {
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

    targets
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_branch_name() {
        assert_eq!(
            generate_branch_name("actix-web", false),
            "syz/update-actix-web"
        );
        assert_eq!(
            generate_branch_name("actix-web", true),
            "syz/update-actix-web-major"
        );
        assert_eq!(generate_branch_name("Foo@Bar", false), "syz/update-foo-bar");
        assert_eq!(
            generate_branch_name("foo---bar", false),
            "syz/update-foo-bar"
        );
        assert_eq!(
            generate_branch_name("Foo@Bar", true),
            "syz/update-foo-bar-major"
        );
        assert_eq!(
            generate_branch_name("foo---bar", true),
            "syz/update-foo-bar-major"
        );
    }
}
