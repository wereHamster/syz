use super::process_bump::build_update_targets;
use super::{Change, DefaultBranch};
use crate::core::database::SweepKind;
use crate::core::engine::pull_request_generator::budget::{fit_items, BODY_LIMIT};
use crate::core::engine::UpdateTarget;
use crate::core::job::LaneKey;
use crate::core::message::Payload;
use crate::core::services::Services;
use crate::core::store::BumpTargetData;
use anyhow::{Context, Result};
use std::collections::HashMap;

/// The ecosystems whose versions follow semver, so that a bump within the current major
/// version is a compatible one. Nix flake inputs aren't versioned that way.
pub const ECOSYSTEMS: [&str; 2] = ["cargo", "npm"];

const TITLE: &str = "Update dependencies to compatible versions";

/// Create or update the pull request that applies all of the project's non-major bumps of
/// the [`ECOSYSTEMS`], or close it if there is nothing to update.
///
/// The bumps are those of the latest scan, so a rescan before this picks up new releases.
pub async fn run(
    services: &Services,
    lane: LaneKey,
    project_id: String,
) -> Result<Option<Payload>> {
    let default_branch = DefaultBranch::resolve(services.repository(&lane)?.as_ref()).await?;

    let mut rows_by_ecosystem: HashMap<String, Vec<BumpTargetData>> = HashMap::new();
    for row in services.store().non_major_bump_targets(&project_id).await? {
        rows_by_ecosystem
            .entry(row.eco_type.clone())
            .or_default()
            .push(row);
    }

    let mut modifications = Vec::new();
    let mut sections = Vec::new();

    for ecosystem in ECOSYSTEMS {
        let Some(rows) = rows_by_ecosystem.remove(ecosystem) else {
            continue;
        };
        let patcher = services
            .patcher(ecosystem)
            .with_context(|| format!("No patcher found for ecosystem {}", ecosystem))?;

        let targets = build_update_targets(patcher.as_ref(), rows);
        if targets.is_empty() {
            continue;
        }

        // Without a trustworthy result for every ecosystem, neither opening nor closing the
        // pull request would be correct.
        let temp_dir = super::create_temp_dir(services.work_dir())?;
        let result = patcher
            .apply_updates(default_branch.snapshot.as_ref(), temp_dir.path(), &targets)
            .await
            .with_context(|| format!("Failed to apply the compatible updates for {}", ecosystem))?;
        if result.is_empty() {
            continue;
        }

        modifications.extend(result);
        sections.push((ecosystem, targets));
    }

    let change = (!modifications.is_empty()).then(|| Change {
        title: TITLE.to_string(),
        body: pull_request_body(&sections, BODY_LIMIT),
        modifications,
    });

    let kind = SweepKind::Compatible;
    let outcome = default_branch.publish(kind.branch_name(), change).await?;

    Ok(Some(Payload::SweepPublished {
        lane,
        project_id,
        kind,
        outcome,
    }))
}

/// Lists how each package is updated, by ecosystem, within `limit`. Packages that appear more
/// than once (in several workspace members, say) with the same update are listed once.
fn pull_request_body(sections: &[(&str, Vec<UpdateTarget>)], limit: usize) -> String {
    let intro = "This pull request updates dependencies to newer versions that are compatible \
                 with their current ones.\n";

    let mut items = Vec::new();
    for (ecosystem, targets) in sections {
        let mut lines: Vec<String> = targets
            .iter()
            .map(|target| {
                format!(
                    "- `{}`: `{}` -> `{}`\n",
                    target.name, target.current_version.version, target.target_version.version
                )
            })
            .collect();
        lines.sort();
        lines.dedup();

        // The heading goes with the first line, so that it is only dropped along with all of
        // its section's lines.
        if let Some(first) = lines.first_mut() {
            first.insert_str(0, &format!("\n### {}\n\n", ecosystem));
        }
        items.extend(lines);
    }

    fit_items(intro, &items, "", limit, |n| {
        format!("- … and {} more\n", n)
    })
    .unwrap_or_else(|| intro.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::{PackageInfo, RequirementVersion};

    fn target(ecosystem: &str, name: &str, current: &str, target: &str) -> UpdateTarget {
        UpdateTarget {
            ecosystem: ecosystem.to_string(),
            name: name.to_string(),
            current_version: RequirementVersion {
                requirement: current.to_string(),
                version: current.to_string(),
            },
            target_version: RequirementVersion {
                requirement: target.to_string(),
                version: target.to_string(),
            },
            latest_version: target.to_string(),
            package_info: PackageInfo { repo_url: None },
            minimum_release_age: None,
        }
    }

    #[test]
    fn lists_the_updates_by_ecosystem() {
        let body = pull_request_body(
            &[
                (
                    "cargo",
                    vec![
                        target("cargo", "tokio", "1.0.0", "1.5.0"),
                        target("cargo", "serde", "1.0.200", "1.0.219"),
                        target("cargo", "serde", "1.0.200", "1.0.219"),
                    ],
                ),
                ("npm", vec![target("npm", "react", "18.2.0", "18.3.1")]),
            ],
            BODY_LIMIT,
        );

        assert_eq!(
            body,
            "This pull request updates dependencies to newer versions that are compatible with \
             their current ones.\n\
             \n### cargo\n\n\
             - `serde`: `1.0.200` -> `1.0.219`\n\
             - `tokio`: `1.0.0` -> `1.5.0`\n\
             \n### npm\n\n\
             - `react`: `18.2.0` -> `18.3.1`\n"
        );
    }

    #[test]
    fn says_how_many_updates_do_not_fit() {
        let targets: Vec<UpdateTarget> = (0..100)
            .map(|i| target("npm", &format!("package-{:03}", i), "1.0.0", "1.1.0"))
            .collect();

        let body = pull_request_body(&[("npm", targets)], 1_000);

        assert!(body.len() <= 1_000, "{body}");
        let kept = body.matches("- `package-").count();
        assert!(kept > 0 && kept < 100, "{body}");
        assert!(
            body.ends_with(&format!("- … and {} more\n", 100 - kept)),
            "{body}"
        );
    }
}
