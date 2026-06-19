use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::{
    advisories::SecurityUpdateSummary, TransitiveUpdateSummary, UpdateTarget,
};

pub mod context;
pub mod formatting;
pub mod sections;
pub mod title;

use context::PullRequestGenerationContext;
use sections::{
    advisories::AdvisoriesSection, history::HistorySection, policy::PolicySection,
    summary::SummarySection, PullRequestSectionGenerator,
};

#[async_trait]
pub trait PullRequestGenerator: Send + Sync {
    async fn generate_pull_request_title(
        &self,
        package_group: &str,
        ecosystem: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String>;

    async fn generate_pull_request_body(
        &self,
        package_group: &str,
        ecosystem: &str,
        targets: &[UpdateTarget],
        is_major: bool,
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

        if !summary.blocked_by_age.is_empty() {
            pr_body.push_str("> [!WARNING]\n> The following vulnerable packages have fixes available, but they have not met the `minimumReleaseAge` requirement yet and were skipped:\n>\n");

            let mut sorted_blocked: Vec<String> = summary.blocked_by_age.keys().cloned().collect();
            sorted_blocked.sort();

            let now = chrono::Utc::now();
            let min_age = summary
                .minimum_release_age
                .unwrap_or(chrono::Duration::zero());

            for module in sorted_blocked {
                let blocked_versions = summary.blocked_by_age.get(&module).unwrap();
                for (ver, publish_time) in blocked_versions {
                    let available_time = *publish_time + min_age;
                    let remaining = available_time.signed_duration_since(now).num_seconds();
                    let days = (remaining as f64 / 86400.0).ceil() as i64;

                    let availability = if days > 1 {
                        format!("in {} days", days)
                    } else {
                        format!("{} UTC", available_time.format("%A at %H:%M"))
                    };

                    pr_body.push_str(&format!(
                        "> - `{}` (`{}`): available {}\n",
                        module, ver, availability
                    ));
                }
            }
            pr_body.push_str("\n---\n\n");
        }

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
    ) -> Result<String> {
        Ok(title::generate_title(
            package_group,
            ecosystem,
            targets,
            is_major,
            Some(&self.github),
        )
        .await)
    }

    async fn generate_pull_request_body(
        &self,
        package_group: &str,
        ecosystem: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String> {
        let mut display_targets = targets.to_vec();

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

        let ctx = PullRequestGenerationContext {
            package_group,
            ecosystem,
            targets: &display_targets,
            is_major,
            registry_router: &self.registry_router,
            advisory_resolver: self.advisory_resolver.as_ref(),
            github: &self.github,
        };

        let sections: Vec<Box<dyn PullRequestSectionGenerator>> = vec![
            Box::new(SummarySection),
            Box::new(PolicySection),
            Box::new(AdvisoriesSection),
            Box::new(HistorySection),
        ];

        let mut body = String::new();
        for section in sections {
            if let Ok(Some(content)) = section.generate(&ctx, body.len()).await {
                body.push_str(&content);
            }
        }

        Ok(body)
    }
}
