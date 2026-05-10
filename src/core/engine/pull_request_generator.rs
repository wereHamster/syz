use crate::core::engine::UpdateTarget;
use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait PullRequestGenerator: Send + Sync {
    async fn generate_pull_request_title(
        &self,
        package_group: &str,
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

    fn release_notes_resolver(
        &self,
        package_name: &str,
        repo_url: &str,
    ) -> Option<Box<dyn crate::core::engine::releases::ReleaseNotesResolver>> {
        if repo_url.contains("github.com") {
            Some(Box::new(
                crate::core::clients::github_release_notes::GithubReleaseNotesResolver::new(
                    self.github.clone(),
                    package_name.to_string(),
                    repo_url.to_string(),
                ),
            ))
        } else {
            None
        }
    }
}

#[async_trait]
impl PullRequestGenerator for DefaultPullRequestGenerator {
    async fn generate_pull_request_title(
        &self,
        package_group: &str,
        targets: &[UpdateTarget],
        is_major: bool,
    ) -> Result<String> {
        let mut title = if targets.len() == 1 {
            let target = &targets[0];
            let clean_new = target.target_version.version.clone();
            format!("Update {} to {}", package_group, clean_new)
        } else {
            let first_new_req = &targets[0].target_version;
            let mut highest_new = &first_new_req.version;
            let mut lowest_new = &first_new_req.version;

            for t in targets {
                let current_ver_str = &t.target_version.version;
                let highest_ver_str = highest_new;
                let lowest_ver_str = lowest_new;

                if let (Ok(ver_new), Ok(ver_highest)) = (
                    semver::Version::parse(current_ver_str),
                    semver::Version::parse(highest_ver_str),
                ) {
                    if ver_new > ver_highest {
                        highest_new = &t.target_version.version;
                    }
                }

                if let (Ok(ver_new), Ok(ver_lowest)) = (
                    semver::Version::parse(current_ver_str),
                    semver::Version::parse(lowest_ver_str),
                ) {
                    if ver_new < ver_lowest {
                        lowest_new = &t.target_version.version;
                    }
                }
            }

            let clean_highest = highest_new;
            let clean_lowest = lowest_new;

            if clean_highest == clean_lowest {
                format!("Update {} to {}", package_group, clean_highest)
            } else {
                format!(
                    "Update {} to {} ~ {}",
                    package_group, clean_lowest, clean_highest
                )
            }
        };

        if is_major {
            title.push_str(" (major)");
        }

        Ok(title)
    }

    async fn generate_pull_request_body(
        &self,
        package_group: &str,
        ecosystem: &str,
        targets: &[UpdateTarget],
        _is_major: bool,
    ) -> Result<String> {
        let mut body = format!("This PR updates {} dependencies.\n\n", package_group);

        for target in targets {
            body.push_str(&format!(
                "- `{}`: {} -> {}\n",
                target.name, target.current_version.version, target.target_version.version
            ));
        }
        body.push_str("\n");

        let mut advisories_md = String::new();
        let mut seen_advisories = std::collections::HashSet::new();

        for target in targets {
            if let Ok(advs) = self
                .advisory_resolver
                .resolve_advisories(
                    ecosystem,
                    &target.name,
                    &target.current_version.version,
                    &target.target_version.version,
                )
                .await
            {
                for adv in advs {
                    if seen_advisories.insert(adv.id.clone()) {
                        advisories_md
                            .push_str(&format!("> - [{}]({}): {}\n", adv.id, adv.url, adv.title));
                    }
                }
            }
        }

        if !advisories_md.is_empty() {
            body.push_str(
                "> [!CAUTION]\n> This update resolves the following security advisories:\n",
            );
            body.push_str(&advisories_md);
            body.push_str("\n");
        }

        for target in targets {
            if target.latest_version != target.target_version.version
                && target.minimum_release_age.is_some()
            {
                body.push_str(&format!(
                    "> [!IMPORTANT]\n> A more recent version of `{}` is released ({}). However, it is not approved yet by policy.\n\n",
                    target.name, target.latest_version
                ));
            }
        }

        body.push_str("# Release History\n\n");

        for target in targets {
            let mut repo_url = target.package_info.repo_url.clone().unwrap_or_default();
            if repo_url.is_empty() {
                if let Ok(info) = self
                    .registry_router
                    .fetch_package_info(ecosystem, &target.name)
                    .await
                {
                    repo_url = info.repo_url.unwrap_or_default();
                }
            }

            if let Ok(history) = self
                .registry_router
                .fetch_release_history(
                    ecosystem,
                    &target.name,
                    &target.current_version.version,
                    &target.target_version.version,
                )
                .await
            {
                if history.is_empty() {
                    continue;
                }

                if targets.len() > 1 {
                    body.push_str(&format!("## `{}`\n\n", target.name));
                }

                let resolver = if !repo_url.is_empty() {
                    self.release_notes_resolver(&target.name, &repo_url)
                } else {
                    None
                };

                let mut current_length = body.len();

                for release in history {
                    let mut time_str = "Published ".to_string();
                    let now = chrono::Utc::now();
                    let diff = now.signed_duration_since(release.publish_time);
                    let days = diff.num_days();
                    if days > 0 {
                        time_str.push_str(&format!("{} days ago", days));
                    } else {
                        time_str.push_str("today");
                    }

                    body.push_str(&format!("### {}\n{}\n\n", release.version, time_str));

                    if current_length > 60_000 {
                        body.push_str("> *Changelog truncated due to GitHub PR size limits.*\n\n");
                        continue;
                    }

                    if let Some(res) = &resolver {
                        if let Ok(Some((_, md))) = res.resolve_release_notes(&release.version).await
                        {
                            body.push_str(&md);
                            body.push_str("\n\n");
                            current_length = body.len();
                        }
                    }
                }
            }
        }

        Ok(body)
    }
}
