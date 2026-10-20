use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::{
    advisories::{ReleaseAgeExceptionKind, SecurityUpdateSummary},
    repository::{FileModification, ProjectRepositorySnapshot},
    TransitiveUpdateSummary, UpdateTarget,
};

pub mod budget;
pub mod context;
pub mod formatting;
pub mod sections;
pub mod title;

use budget::{SectionKind, BODY_LIMIT};
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

/// What the sections of one ecosystem's targets are run with.
struct Partition<'a> {
    ctx: PullRequestGenerationContext<'a>,
    sections: Vec<Box<dyn PullRequestSectionGenerator>>,
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

/// Runs the sections of all partitions and returns what they produced, in the order it goes
/// into the description. `limit` is how long the description may get.
///
/// The sections are run kind by kind, so the summaries of all ecosystems come first. A kind
/// gets a budget, which its sections share: each gets an equal part of what is left of it,
/// so what a section doesn't use goes to the ones after it.
///
/// A section that has nothing to report is left out. One that fails, or doesn't stay within
/// its budget, is logged and left out, so it can't fail the description.
async fn run_sections(partitions: &[Partition<'_>], limit: usize) -> Vec<String> {
    let mut produced = Vec::new();
    let mut length = 0;

    for kind in SectionKind::IN_ORDER {
        let queue: Vec<_> = partitions
            .iter()
            .flat_map(|partition| {
                partition
                    .sections
                    .iter()
                    .filter(|section| section.kind() == kind)
                    .map(move |section| (&partition.ctx, section))
            })
            .collect();

        let mut kind_budget = kind.budget(limit.saturating_sub(length));
        let count = queue.len();

        for (index, (ctx, section)) in queue.into_iter().enumerate() {
            let budget = kind_budget / (count - index);

            match section.generate(ctx, budget).await {
                Ok(Some(content)) if content.len() <= budget => {
                    kind_budget -= content.len();
                    length += content.len();
                    produced.push(content);
                }
                Ok(Some(content)) => tracing::warn!(
                    "Pull request section {} exceeded its budget for {} ({}): {} > {} bytes",
                    section.name(),
                    ctx.package_group,
                    ctx.ecosystem,
                    content.len(),
                    budget
                ),
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
    }

    produced
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
        let mut displayed = Vec::new();
        for (ecosystem, targets) in partition_by_ecosystem(targets) {
            let targets = self.display_targets(&ecosystem, targets).await;
            displayed.push((ecosystem, targets));
        }

        let partitions: Vec<Partition> = displayed
            .iter()
            .map(|(ecosystem, targets)| Partition {
                ctx: PullRequestGenerationContext {
                    package_group,
                    ecosystem,
                    targets,
                    is_major,
                    registry_router: &self.registry_router,
                    advisory_resolver: self.advisory_resolver.as_ref(),
                    github: Some(&self.github),
                    snapshot,
                    modifications,
                },
                sections: sections_for(ecosystem),
            })
            .collect();

        Ok(run_sections(&partitions, BODY_LIMIT).await.concat())
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
    struct FakeSection(&'static str, SectionKind, Option<&'static str>);

    #[async_trait]
    impl PullRequestSectionGenerator for FakeSection {
        fn name(&self) -> &'static str {
            self.0
        }

        fn kind(&self) -> SectionKind {
            self.1
        }

        async fn generate(
            &self,
            _ctx: &PullRequestGenerationContext<'_>,
            _budget: usize,
        ) -> Result<Option<String>> {
            match self.2 {
                Some(content) => Ok(Some(content.to_string())),
                None => Err(anyhow::anyhow!("boom")),
            }
        }
    }

    /// A section that reports the budget it was given.
    struct BudgetEcho(&'static str, SectionKind);

    #[async_trait]
    impl PullRequestSectionGenerator for BudgetEcho {
        fn name(&self) -> &'static str {
            self.0
        }

        fn kind(&self) -> SectionKind {
            self.1
        }

        async fn generate(
            &self,
            ctx: &PullRequestGenerationContext<'_>,
            budget: usize,
        ) -> Result<Option<String>> {
            Ok(Some(format!("{}:{};", ctx.ecosystem, budget)))
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

    type Sections = Vec<Box<dyn PullRequestSectionGenerator>>;

    /// Runs the sections of each ecosystem on its targets, with `limit` as the length of the
    /// description. Returns what they produced and what was logged.
    ///
    /// The contexts have no GitHub client, so sections can't make requests.
    async fn run(
        limit: usize,
        ecosystems: Vec<(&str, Vec<UpdateTarget>, Sections)>,
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
        let snapshot = MockSnapshot::empty();
        let partitions: Vec<Partition> = ecosystems
            .into_iter()
            .map(|(ecosystem, targets, sections)| Partition {
                ctx: PullRequestGenerationContext {
                    package_group: "group",
                    ecosystem,
                    targets: &*Box::leak(targets.into_boxed_slice()),
                    is_major: false,
                    registry_router: &router,
                    advisory_resolver: &NoAdvisories,
                    github: None,
                    snapshot: &snapshot,
                    modifications: &[],
                },
                sections,
            })
            .collect();

        let produced = run_sections(&partitions, limit).await;
        let log = String::from_utf8(log.0.lock().unwrap().clone()).unwrap();
        (produced, log)
    }

    async fn run_one(ecosystem: &str, targets: Vec<UpdateTarget>) -> Vec<String> {
        let sections = sections_for(ecosystem);
        run(BODY_LIMIT, vec![(ecosystem, targets, sections)])
            .await
            .0
    }

    #[tokio::test]
    async fn a_failing_section_is_logged_and_left_out() {
        let (produced, log) = run(
            BODY_LIMIT,
            vec![(
                "npm",
                vec![target("npm", "react", "17.0.0", "18.0.0")],
                vec![
                    Box::new(FakeSection("first", SectionKind::Summary, Some("one"))),
                    Box::new(FakeSection("broken", SectionKind::Summary, None)),
                    Box::new(FakeSection("second", SectionKind::Summary, Some("two"))),
                ],
            )],
        )
        .await;

        assert_eq!(produced, ["one", "two"]);
        assert!(log.contains("WARN"), "{}", log);
        assert!(log.contains("section broken failed"), "{}", log);
        assert!(log.contains("boom"), "{}", log);
    }

    #[tokio::test]
    async fn a_section_over_its_budget_is_logged_and_left_out() {
        let (produced, log) = run(
            10,
            vec![(
                "npm",
                Vec::new(),
                vec![
                    Box::new(FakeSection(
                        "long",
                        SectionKind::Summary,
                        Some("0123456789ab"),
                    )),
                    Box::new(FakeSection("short", SectionKind::Summary, Some("ok"))),
                ],
            )],
        )
        .await;

        assert_eq!(produced, ["ok"]);
        assert!(log.contains("section long exceeded its budget"), "{}", log);
    }

    #[tokio::test]
    async fn summaries_of_all_ecosystems_come_before_the_other_sections() {
        let echo = || -> Sections {
            vec![
                Box::new(BudgetEcho("summary", SectionKind::Summary)),
                Box::new(BudgetEcho("history", SectionKind::History)),
            ]
        };

        let (produced, _) = run(
            BODY_LIMIT,
            vec![("npm", Vec::new(), echo()), ("cargo", Vec::new(), echo())],
        )
        .await;

        let order: Vec<&str> = produced
            .iter()
            .map(|section| section.split(':').next().unwrap())
            .collect();
        assert_eq!(order, ["npm", "cargo", "npm", "cargo"]);
    }

    #[tokio::test]
    async fn a_kind_shares_its_budget_and_passes_on_what_is_left() {
        let echo = || -> Sections {
            vec![
                Box::new(BudgetEcho("summary", SectionKind::Summary)),
                Box::new(BudgetEcho("history", SectionKind::History)),
            ]
        };

        let (produced, _) = run(
            BODY_LIMIT,
            vec![("npm", Vec::new(), echo()), ("cargo", Vec::new(), echo())],
        )
        .await;

        // The summaries split their 5,000 bytes; the second also gets what the first left.
        assert_eq!(produced[0], "npm:2500;");
        assert_eq!(produced[1], "cargo:4991;");
        // History gets the rest of the description, split between the two ecosystems.
        let used = produced[0].len() + produced[1].len();
        assert_eq!(produced[2], format!("npm:{};", (BODY_LIMIT - used) / 2));
    }

    #[tokio::test]
    async fn nix_flake_body_is_just_the_summary() {
        let produced = run_one(
            "nix-flake",
            vec![target("nix-flake", "nixpkgs", "aaa", "bbb")],
        )
        .await;

        assert_eq!(
            produced,
            ["This PR updates `nixpkgs` from version aaa to bbb.\n\n"]
        );
    }

    #[tokio::test]
    async fn npm_summary_links_the_diff() {
        let produced = run_one("npm", vec![target("npm", "react", "17.0.0", "18.0.0")]).await;

        assert_eq!(
            produced[0],
            "This PR updates `react` from version [17.0.0 to 18.0.0](https://npmx.dev/diff/react/v/17.0.0...18.0.0).\n\n"
        );
    }

    #[tokio::test]
    async fn cargo_summary_marks_major_bumps() {
        let produced = run_one(
            "cargo",
            vec![
                target("cargo", "serde", "1.0.1", "1.0.2"),
                target("cargo", "tokio", "0.9.0", "1.0.0"),
                target("cargo", "anyhow", "0.1.0", "0.2.0"),
            ],
        )
        .await;

        assert_eq!(
            produced[0],
            "This PR updates 3 dependencies from the **group** group.\n\n\
             - `serde` from `1.0.1` to `1.0.2`\n\
             - `tokio` from `0.9.0` to `1.0.0` (major)\n\
             - `anyhow` from `0.1.0` to `0.2.0`\n\n"
        );
    }

    #[tokio::test]
    async fn a_long_summary_list_is_cut_between_items() {
        let targets: Vec<_> = (0..500)
            .map(|i| target("cargo", &format!("crate-{i}"), "1.0.0", "1.1.0"))
            .collect();

        let produced = run_one("cargo", targets).await;
        let summary = &produced[0];

        assert!(summary.len() <= SectionKind::Summary.budget(BODY_LIMIT));
        assert!(summary.ends_with("more\n\n"), "{}", summary);
        let kept = summary.matches("\n- `crate-").count();
        assert!(kept > 0 && kept < 500);
        assert!(
            summary.contains(&format!("- … and {} more\n", 500 - kept)),
            "{}",
            summary
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
