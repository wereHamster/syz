use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::{
    advisories::{ReleaseAgeExceptionKind, SecurityUpdateSummary},
    repository::{FileModification, ProjectRepositorySnapshot},
    TransitiveUpdateSummary, UpdateTarget,
};

pub mod context;
pub mod formatting;
pub mod sections;
pub mod title;

use context::PullRequestGenerationContext;
use sections::{sections_for, PullRequestSectionGenerator};

#[async_trait]
pub trait PullRequestGenerator: Send + Sync {
    async fn generate_pull_request_title(
        &self,
        package_group: &str,
        ecosystem: &str,
        targets: &[UpdateTarget],
        is_major: bool,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<String>;

    /// `snapshot` is the repository before the update and `modifications` the files the
    /// update changes.
    async fn generate_pull_request_body(
        &self,
        package_group: &str,
        targets: &[UpdateTarget],
        is_major: bool,
        snapshot: &dyn ProjectRepositorySnapshot,
        modifications: &[FileModification],
    ) -> Result<String>;
}

#[async_trait]
pub trait TransitiveUpdatesPullRequestGenerator: Send + Sync {
    async fn generate_pull_request_body(&self, summary: &TransitiveUpdateSummary)
        -> Result<String>;
}

#[async_trait]
pub trait AuditPullRequestGenerator: Send + Sync {
    async fn generate_pull_request_body(&self, summary: &SecurityUpdateSummary) -> Result<String>;
}

pub struct DefaultTransitiveUpdatesPullRequestGenerator;

#[async_trait]
impl TransitiveUpdatesPullRequestGenerator for DefaultTransitiveUpdatesPullRequestGenerator {
    async fn generate_pull_request_body(
        &self,
        summary: &TransitiveUpdateSummary,
    ) -> Result<String> {
        let mut pr_body = "This pull request automatically bumps all transitive dependencies to their latest versions.\n\n".to_string();

        if !summary.resolved_advisories.is_empty() {
            pr_body.push_str("### Resolved Vulnerabilities\n\n");

            let mut sorted_modules: Vec<String> =
                summary.resolved_advisories.keys().cloned().collect();
            sorted_modules.sort();

            for module_name in sorted_modules {
                let bump = summary.resolved_advisories.get(&module_name).unwrap();
                pr_body.push_str(&format!("#### `{}`\n", module_name));

                let before_str = bump.before_versions.join(", ");
                let after_str = bump.after_versions.join(", ");
                pr_body.push_str(&format!("`{}` -> `{}`\n\n", before_str, after_str));

                pr_body.push_str("Resolved advisories:\n");
                let mut seen_advisories = std::collections::HashSet::new();
                for adv in &bump.advisories {
                    let title = adv
                        .get("title")
                        .and_then(|t| t.as_str())
                        .unwrap_or("Unknown Advisory");

                    let line = if let Some(gh_id) =
                        adv.get("github_advisory_id").and_then(|i| i.as_str())
                    {
                        let url = adv
                            .get("url")
                            .and_then(|u| u.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("https://github.com/advisories/{}", gh_id));
                        format!("- [{}]({}) - {}\n", gh_id, url, title)
                    } else if let Some(id) = adv.get("id").and_then(|i| {
                        if i.is_number() {
                            i.as_i64().map(|n| n.to_string())
                        } else {
                            i.as_str().map(|s| s.to_string())
                        }
                    }) {
                        let url = adv.get("url").and_then(|u| u.as_str()).unwrap_or("");
                        format!("- [{}]({}) - {}\n", id, url, title)
                    } else {
                        format!("- {}\n", title)
                    };

                    if seen_advisories.insert(line.clone()) {
                        pr_body.push_str(&line);
                    }
                }
                pr_body.push('\n');
            }

            pr_body.push_str("---\n\n");
        }

        if !summary.major_bumps.is_empty() {
            pr_body.push_str("### Major Version Bumps\n");
            for (module, desc) in &summary.major_bumps {
                pr_body.push_str(&format!("- `{}`: {}\n", module, desc));
            }
            pr_body.push('\n');
        }

        if !summary.minor_bumps.is_empty() {
            pr_body.push_str("### Minor & Patch Bumps\n");
            for (module, desc) in &summary.minor_bumps {
                pr_body.push_str(&format!("- `{}`: {}\n", module, desc));
            }
            pr_body.push('\n');
        }

        if !summary.added.is_empty() {
            pr_body.push_str("### Added Dependencies\n");
            for (module, desc) in &summary.added {
                pr_body.push_str(&format!("- `{}`: `{}`\n", module, desc));
            }
            pr_body.push('\n');
        }

        if !summary.removed.is_empty() {
            pr_body.push_str("### Removed Dependencies\n");
            for (module, desc) in &summary.removed {
                pr_body.push_str(&format!("- `{}`: `{}`\n", module, desc));
            }
            pr_body.push('\n');
        }

        Ok(pr_body)
    }
}

pub struct DefaultAuditPullRequestGenerator;

#[async_trait]
impl AuditPullRequestGenerator for DefaultAuditPullRequestGenerator {
    async fn generate_pull_request_body(&self, summary: &SecurityUpdateSummary) -> Result<String> {
        let mut pr_body = "This pull request automatically updates dependencies to resolve known security vulnerabilities.\n\n".to_string();

        let now = chrono::Utc::now();

        if !summary.unfixable_vulnerabilities.is_empty() {
            pr_body.push_str("> [!CAUTION]\n> The following packages are still vulnerable because no safe update could be applied (either no patch exists, or it was blocked by policy, or `pnpm dedupe` couldn't resolve the constraint tree):\n>\n");

            let mut sorted_unresolved: Vec<String> =
                summary.unfixable_vulnerabilities.keys().cloned().collect();
            sorted_unresolved.sort();

            for module in sorted_unresolved {
                let advs = summary.unfixable_vulnerabilities.get(&module).unwrap();
                let mut unique_titles = std::collections::HashSet::new();
                for adv in advs {
                    if let Some(title) = adv.get("title").and_then(|t| t.as_str()) {
                        unique_titles.insert(title.to_string());
                    }
                }

                let mut titles: Vec<String> = unique_titles.into_iter().collect();
                titles.sort();
                let titles_str = titles.join(", ");

                pr_body.push_str(&format!("> - `{}`: {}\n", module, titles_str));
            }
            pr_body.push_str("\n---\n\n");
        }

        if !summary.release_age_exceptions.is_empty() {
            let min_age = summary
                .minimum_release_age
                .unwrap_or(chrono::Duration::zero());
            let location = summary
                .release_age_exceptions_location
                .as_deref()
                .unwrap_or("the package manager configuration");

            pr_body.push_str(&format!(
                "> [!IMPORTANT]\n> The following versions have not met the `minimumReleaseAge` requirement ({}) yet, but are needed to fix the vulnerabilities below. They were added to {} and should be reviewed before merging. The exceptions are removed automatically once the versions have matured.\n\n",
                format_duration(min_age),
                location
            ));
            pr_body.push_str("| Package | Version | Published | Matures | Reason |\n");
            pr_body.push_str("| --- | --- | --- | --- | --- |\n");

            let mut exceptions = summary.release_age_exceptions.clone();
            exceptions.sort_by(|a, b| {
                (a.kind != ReleaseAgeExceptionKind::Fix, &a.name, &a.version).cmp(&(
                    b.kind != ReleaseAgeExceptionKind::Fix,
                    &b.name,
                    &b.version,
                ))
            });

            for exception in exceptions {
                let (published, matures) = match exception.published_at {
                    Some(published_at) => (
                        format!("{} UTC", published_at.format("%Y-%m-%d %H:%M")),
                        format_availability(published_at + min_age, now),
                    ),
                    None => ("unknown".to_string(), "unknown".to_string()),
                };
                let reason = match exception.kind {
                    ReleaseAgeExceptionKind::Fix => "security fix",
                    ReleaseAgeExceptionKind::Dependency => "required by a fix",
                };
                pr_body.push_str(&format!(
                    "| `{}` | `{}` | {} | {} | {} |\n",
                    exception.name, exception.version, published, matures, reason
                ));
            }
            pr_body.push_str("\n---\n\n");
        }

        if !summary.blocked_by_age.is_empty() {
            pr_body.push_str("> [!WARNING]\n> The following vulnerable packages have fixes available, but they have not met the `minimumReleaseAge` requirement yet and were skipped:\n>\n");

            let mut sorted_blocked: Vec<String> = summary.blocked_by_age.keys().cloned().collect();
            sorted_blocked.sort();

            let min_age = summary
                .minimum_release_age
                .unwrap_or(chrono::Duration::zero());

            for module in sorted_blocked {
                let blocked_versions = summary.blocked_by_age.get(&module).unwrap();
                for (ver, publish_time) in blocked_versions {
                    pr_body.push_str(&format!(
                        "> - `{}` (`{}`): available {}\n",
                        module,
                        ver,
                        format_availability(*publish_time + min_age, now)
                    ));
                }
            }
            pr_body.push_str("\n---\n\n");
        }

        let mut sorted_modules: Vec<String> = summary.resolved_advisories.keys().cloned().collect();
        sorted_modules.sort();

        for module_name in sorted_modules {
            let bump = summary.resolved_advisories.get(&module_name).unwrap();
            pr_body.push_str(&format!("### `{}`\n", module_name));

            if bump.before_versions.is_empty() && bump.after_versions.is_empty() {
                pr_body.push_str("- Bumped\n");
            } else {
                let before_str = bump.before_versions.join(", ");
                let after_str = bump.after_versions.join(", ");
                pr_body.push_str(&format!("`{}` -> `{}`\n\n", before_str, after_str));
            }

            pr_body.push_str("Resolved advisories:\n");
            let mut seen_advisories = std::collections::HashSet::new();
            for adv in &bump.advisories {
                let title = adv
                    .get("title")
                    .and_then(|t| t.as_str())
                    .unwrap_or("Unknown Advisory");

                let line =
                    if let Some(gh_id) = adv.get("github_advisory_id").and_then(|i| i.as_str()) {
                        let url = adv
                            .get("url")
                            .and_then(|u| u.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| format!("https://github.com/advisories/{}", gh_id));
                        format!("- [{}]({}) - {}\n", gh_id, url, title)
                    } else if let Some(id) = adv.get("id").and_then(|i| {
                        if i.is_number() {
                            i.as_i64().map(|n| n.to_string())
                        } else {
                            i.as_str().map(|s| s.to_string())
                        }
                    }) {
                        let url = adv.get("url").and_then(|u| u.as_str()).unwrap_or("");
                        format!("- [{}]({}) - {}\n", id, url, title)
                    } else {
                        format!("- {}\n", title)
                    };

                if seen_advisories.insert(line.clone()) {
                    pr_body.push_str(&line);
                }
            }
            pr_body.push('\n');
        }

        Ok(pr_body)
    }
}

/// When something that becomes available at `available_time` can be used, relative to `now`.
fn format_availability(
    available_time: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> String {
    let remaining = available_time.signed_duration_since(now).num_seconds();
    let days = (remaining as f64 / 86400.0).ceil() as i64;

    if days > 1 {
        format!("in {} days", days)
    } else {
        format!("{} UTC", available_time.format("%A at %H:%M"))
    }
}

fn format_duration(duration: chrono::Duration) -> String {
    let minutes = duration.num_minutes();
    let (value, unit) = if minutes % 1440 == 0 {
        (minutes / 1440, "day")
    } else if minutes % 60 == 0 {
        (minutes / 60, "hour")
    } else {
        (minutes, "minute")
    };
    format!("{} {}{}", value, unit, if value == 1 { "" } else { "s" })
}

/// What the sections of one ecosystem's targets produced.
struct PartitionBody {
    // Not read yet: how several partitions are laid out in one description is undecided.
    #[allow(dead_code)]
    ecosystem: String,
    sections: Vec<String>,
}

/// Splits the targets by ecosystem, in the order the ecosystems first appear.
fn partition_by_ecosystem(targets: &[UpdateTarget]) -> Vec<(String, Vec<UpdateTarget>)> {
    let mut partitions: Vec<(String, Vec<UpdateTarget>)> = Vec::new();

    for target in targets {
        match partitions
            .iter_mut()
            .find(|(ecosystem, _)| *ecosystem == target.ecosystem)
        {
            Some((_, partition)) => partition.push(target.clone()),
            None => partitions.push((target.ecosystem.clone(), vec![target.clone()])),
        }
    }

    partitions
}

/// Runs the sections in order and returns what they produced, with `base_length` being the
/// length of the description before them.
///
/// A section that has nothing to report is left out. One that fails is logged and left out,
/// so it can't fail the description.
async fn run_sections(
    sections: &[Box<dyn PullRequestSectionGenerator>],
    ctx: &PullRequestGenerationContext<'_>,
    base_length: usize,
) -> Vec<String> {
    let mut produced = Vec::new();
    let mut length = base_length;

    for section in sections {
        match section.generate(ctx, length).await {
            Ok(Some(content)) => {
                length += content.len();
                produced.push(content);
            }
            Ok(None) => {}
            Err(e) => tracing::warn!(
                "Pull request section {} failed for {} ({}): {:#}",
                section.name(),
                ctx.package_group,
                ctx.ecosystem,
                e
            ),
        }
    }

    produced
}

fn render(partitions: &[PartitionBody]) -> String {
    partitions
        .iter()
        .flat_map(|partition| partition.sections.iter().map(String::as_str))
        .collect()
}

pub struct DefaultPullRequestGenerator {
    registry_router: crate::core::engine::ecosystems::registry_router::RegistryRouter,
    advisory_resolver: Box<dyn crate::core::engine::advisories::AdvisoryResolver>,
    github: crate::core::clients::github::GitHub,
}

impl DefaultPullRequestGenerator {
    pub fn new(
        registry_router: crate::core::engine::ecosystems::registry_router::RegistryRouter,
        advisory_resolver: Box<dyn crate::core::engine::advisories::AdvisoryResolver>,
        github: crate::core::clients::github::GitHub,
    ) -> Self {
        Self {
            registry_router,
            advisory_resolver,
            github,
        }
    }
}

#[async_trait]
impl PullRequestGenerator for DefaultPullRequestGenerator {
    async fn generate_pull_request_title(
        &self,
        package_group: &str,
        ecosystem: &str,
        targets: &[UpdateTarget],
        is_major: bool,
        repo: &dyn ProjectRepositorySnapshot,
    ) -> Result<String> {
        Ok(title::generate_title(
            package_group,
            ecosystem,
            targets,
            is_major,
            Some(&self.github),
            repo,
        )
        .await)
    }

    async fn generate_pull_request_body(
        &self,
        package_group: &str,
        targets: &[UpdateTarget],
        is_major: bool,
        snapshot: &dyn ProjectRepositorySnapshot,
        modifications: &[FileModification],
    ) -> Result<String> {
        let mut partitions = Vec::new();
        let mut length = 0;

        for (ecosystem, targets) in partition_by_ecosystem(targets) {
            let display_targets = self.display_targets(&ecosystem, targets).await;

            let ctx = PullRequestGenerationContext {
                package_group,
                ecosystem: &ecosystem,
                targets: &display_targets,
                is_major,
                registry_router: &self.registry_router,
                advisory_resolver: self.advisory_resolver.as_ref(),
                github: Some(&self.github),
                snapshot,
                modifications,
            };

            let sections = run_sections(&sections_for(&ecosystem), &ctx, length).await;
            length += sections.iter().map(String::len).sum::<usize>();
            partitions.push(PartitionBody {
                ecosystem,
                sections,
            });
        }

        Ok(render(&partitions))
    }
}

impl DefaultPullRequestGenerator {
    /// Rewrites the targets of an ecosystem the way they are shown in the description.
    async fn display_targets(
        &self,
        ecosystem: &str,
        targets: Vec<UpdateTarget>,
    ) -> Vec<UpdateTarget> {
        let mut display_targets = targets;

        if ecosystem == "github-actions" {
            for t in display_targets.iter_mut() {
                t.current_version.version = t.current_version.requirement.clone();
                t.target_version.version = t.target_version.requirement.clone();

                let action_repo = crate::core::engine::ecosystems::github_actions::internal::helpers::extract_repo(&t.name);

                if t.current_version.version.len() == 40 {
                    if let Some(tag) = crate::core::engine::ecosystems::github_actions::internal::helpers::get_tag_for_sha(&self.github, &action_repo, &t.current_version.version).await {
                        t.current_version.version = tag;
                    }
                }

                if t.target_version.version.len() == 40 {
                    if let Some(tag) = crate::core::engine::ecosystems::github_actions::internal::helpers::get_tag_for_sha(&self.github, &action_repo, &t.target_version.version).await {
                        t.target_version.version = tag;
                    }
                }

                if t.latest_version.len() == 40 {
                    if let Some(tag) = crate::core::engine::ecosystems::github_actions::internal::helpers::get_tag_for_sha(&self.github, &action_repo, &t.latest_version).await {
                        t.latest_version = tag;
                    }
                }
            }
        }

        display_targets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::engine::advisories::{Advisory, AdvisoryResolver, ReleaseAgeException};
    use crate::core::engine::ecosystems::registry_router::RegistryRouter;
    use crate::core::engine::repository::test_support::MockSnapshot;
    use crate::core::engine::{PackageInfo, RequirementVersion};
    use std::sync::{Arc, Mutex};

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

    struct NoAdvisories;

    #[async_trait]
    impl AdvisoryResolver for NoAdvisories {
        async fn resolve_advisories(
            &self,
            _ecosystem: &str,
            _name: &str,
            _current_version: &str,
            _target_version: &str,
        ) -> Result<Vec<Advisory>> {
            Ok(Vec::new())
        }
    }

    #[test]
    fn partitions_targets_by_ecosystem_in_order_of_appearance() {
        let partitions = partition_by_ecosystem(&[
            target("npm", "a", "1.0.0", "1.1.0"),
            target("cargo", "b", "1.0.0", "1.1.0"),
            target("npm", "c", "1.0.0", "1.1.0"),
        ]);

        let summary: Vec<(&str, Vec<&str>)> = partitions
            .iter()
            .map(|(ecosystem, targets)| {
                (
                    ecosystem.as_str(),
                    targets.iter().map(|t| t.name.as_str()).collect(),
                )
            })
            .collect();
        assert_eq!(summary, [("npm", vec!["a", "c"]), ("cargo", vec!["b"])]);
    }

    /// A section that returns fixed content, or fails when it has none.
    struct FakeSection(&'static str, Option<&'static str>);

    #[async_trait]
    impl PullRequestSectionGenerator for FakeSection {
        fn name(&self) -> &'static str {
            self.0
        }

        async fn generate(
            &self,
            _ctx: &PullRequestGenerationContext<'_>,
            _current_length: usize,
        ) -> Result<Option<String>> {
            match self.1 {
                Some(content) => Ok(Some(content.to_string())),
                None => Err(anyhow::anyhow!("boom")),
            }
        }
    }

    /// Collects what is logged.
    #[derive(Clone, Default)]
    struct LogBuffer(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for LogBuffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Runs the sections on the targets. Returns what they produced and what was logged.
    ///
    /// The context has no GitHub client, so sections can't make requests.
    async fn run(
        ecosystem: &str,
        targets: &[UpdateTarget],
        sections: Vec<Box<dyn PullRequestSectionGenerator>>,
    ) -> (Vec<String>, String) {
        let log = LogBuffer::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer({
                let log = log.clone();
                move || log.clone()
            })
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let router = RegistryRouter::new(std::collections::HashMap::new());
        let ctx = PullRequestGenerationContext {
            package_group: "group",
            ecosystem,
            targets,
            is_major: false,
            registry_router: &router,
            advisory_resolver: &NoAdvisories,
            github: None,
            snapshot: &MockSnapshot::empty(),
            modifications: &[],
        };

        let produced = run_sections(&sections, &ctx, 0).await;
        let log = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        (produced, log)
    }

    #[tokio::test]
    async fn a_failing_section_is_logged_and_left_out() {
        let (produced, log) = run(
            "npm",
            &[target("npm", "react", "17.0.0", "18.0.0")],
            vec![
                Box::new(FakeSection("first", Some("one"))),
                Box::new(FakeSection("broken", None)),
                Box::new(FakeSection("second", Some("two"))),
            ],
        )
        .await;

        assert_eq!(produced, ["one", "two"]);
        assert!(log.contains("WARN"), "{}", log);
        assert!(log.contains("section broken failed"), "{}", log);
        assert!(log.contains("boom"), "{}", log);
    }

    #[tokio::test]
    async fn nix_flake_body_is_just_the_summary() {
        let (produced, _) = run(
            "nix-flake",
            &[target("nix-flake", "nixpkgs", "aaa", "bbb")],
            sections_for("nix-flake"),
        )
        .await;

        assert_eq!(
            produced,
            ["This PR updates `nixpkgs` from version aaa to bbb.\n\n"]
        );
    }

    #[tokio::test]
    async fn audit_body_lists_release_age_exceptions() {
        let published_at = chrono::Utc::now() - chrono::Duration::hours(2);
        let summary = SecurityUpdateSummary {
            resolved_advisories: std::collections::HashMap::new(),
            blocked_by_age: std::collections::HashMap::new(),
            unfixable_vulnerabilities: std::collections::HashMap::new(),
            minimum_release_age: Some(chrono::Duration::days(3)),
            release_age_exceptions: vec![
                ReleaseAgeException {
                    name: "@next/env".to_string(),
                    version: "15.5.27".to_string(),
                    published_at: Some(published_at),
                    kind: ReleaseAgeExceptionKind::Dependency,
                },
                ReleaseAgeException {
                    name: "next".to_string(),
                    version: "15.5.27".to_string(),
                    published_at: None,
                    kind: ReleaseAgeExceptionKind::Fix,
                },
            ],
            release_age_exceptions_location: Some(
                "`minimumReleaseAgeExclude` in `pnpm-workspace.yaml`".to_string(),
            ),
        };

        let body = DefaultAuditPullRequestGenerator
            .generate_pull_request_body(&summary)
            .await
            .unwrap();

        assert!(body.contains("> [!IMPORTANT]"));
        assert!(body.contains("requirement (3 days)"));
        assert!(body.contains("`minimumReleaseAgeExclude` in `pnpm-workspace.yaml`"));

        let next = body.find("| `next` | `15.5.27` | unknown | unknown | security fix |");
        let env = body.find(&format!(
            "| `@next/env` | `15.5.27` | {} UTC | in 3 days | required by a fix |",
            published_at.format("%Y-%m-%d %H:%M")
        ));
        assert!(next.is_some() && env.is_some(), "{}", body);
        // Fixes come before the versions they pull in.
        assert!(next < env);
    }

    #[test]
    fn formats_durations() {
        assert_eq!(format_duration(chrono::Duration::minutes(1440)), "1 day");
        assert_eq!(format_duration(chrono::Duration::minutes(4320)), "3 days");
        assert_eq!(format_duration(chrono::Duration::minutes(120)), "2 hours");
        assert_eq!(format_duration(chrono::Duration::minutes(90)), "90 minutes");
    }
}
