use crate::core::engine::UpdateTarget;
use anyhow::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

fn format_duration(duration: chrono::Duration) -> String {
    let seconds = duration.num_seconds().abs();

    if seconds < 60 {
        "less than a minute".to_string()
    } else if seconds < 3600 {
        let minutes = duration.num_minutes().abs();
        if minutes == 1 {
            "1 minute".to_string()
        } else {
            format!("{} minutes", minutes)
        }
    } else if seconds < 86400 {
        let hours = duration.num_hours().abs();
        if hours == 1 {
            "1 hour".to_string()
        } else {
            format!("{} hours", hours)
        }
    } else if seconds < 2592000 {
        let days = duration.num_days().abs();
        if days == 1 {
            "1 day".to_string()
        } else {
            format!("{} days", days)
        }
    } else if seconds < 31536000 {
        let months = duration.num_days().abs() / 30;
        if months == 1 {
            "1 month".to_string()
        } else {
            format!("{} months", months)
        }
    } else {
        let years = duration.num_days().abs() / 365;
        if years == 1 {
            "1 year".to_string()
        } else {
            format!("{} years", years)
        }
    }
}

fn format_time_ago(time: DateTime<Utc>) -> String {
    let now = Utc::now();
    let duration = now.signed_duration_since(time);
    if duration.num_seconds() < 60 {
        "Just now".to_string()
    } else {
        format!("{} ago", format_duration(duration))
    }
}

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
        let mut body = String::new();

        if targets.len() == 1 {
            let target = &targets[0];
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

            if !repo_url.is_empty() {
                body.push_str(&format!(
                    "This PR updates [{}]({}) from version {} to {}.\n\n",
                    target.name,
                    repo_url,
                    target.current_version.version,
                    target.target_version.version
                ));
            } else {
                body.push_str(&format!(
                    "This PR updates `{}` from version {} to {}.\n\n",
                    target.name, target.current_version.version, target.target_version.version
                ));
            }
        } else if !targets.is_empty() {
            let first_target = &targets[0];

            // For checking if they all share the same URL, we may need to resolve it first
            // But doing it for all targets might be slow. We'll just rely on what is cached in `package_info`
            // or resolve the first one if we need it.
            let mut first_repo_url = first_target
                .package_info
                .repo_url
                .clone()
                .unwrap_or_default();
            if first_repo_url.is_empty() {
                if let Ok(info) = self
                    .registry_router
                    .fetch_package_info(ecosystem, &first_target.name)
                    .await
                {
                    first_repo_url = info.repo_url.unwrap_or_default();
                }
            }

            let all_same = targets.iter().all(|t| {
                t.current_version.version == first_target.current_version.version
                    && t.target_version.version == first_target.target_version.version
            });

            if all_same {
                if !first_repo_url.is_empty() {
                    body.push_str(&format!(
                        "This PR updates [{}]({}) packages from {} to {}. The following packages are part of this set:\n\n",
                        package_group, first_repo_url, first_target.current_version.version, first_target.target_version.version
                    ));
                } else {
                    body.push_str(&format!(
                        "This PR updates `{}` packages from {} to {}. The following packages are part of this set:\n\n",
                        package_group, first_target.current_version.version, first_target.target_version.version
                    ));
                }

                for t in targets {
                    body.push_str(&format!("- `{}`\n", t.name));
                }
                body.push_str("\n");
            } else {
                body.push_str(&format!(
                    "This PR updates {} dependencies from the **{}** group.\n\n",
                    targets.len(),
                    package_group
                ));

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

                    if !repo_url.is_empty() {
                        body.push_str(&format!(
                            "- [{}]({}) from `{}` to `{}`\n",
                            target.name,
                            repo_url,
                            target.current_version.version,
                            target.target_version.version
                        ));
                    } else {
                        body.push_str(&format!(
                            "- `{}` from `{}` to `{}`\n",
                            target.name,
                            target.current_version.version,
                            target.target_version.version
                        ));
                    }
                }
                body.push_str("\n");
            }
        }

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

        let all_same = !targets.is_empty()
            && targets.iter().all(|t| {
                t.current_version.version == targets[0].current_version.version
                    && t.target_version.version == targets[0].target_version.version
            });

        let policy_targets = if all_same && targets.len() > 1 {
            vec![&targets[0]]
        } else {
            targets.iter().collect::<Vec<_>>()
        };

        for target in policy_targets {
            if target.latest_version != target.target_version.version
                && target.minimum_release_age.is_some()
            {
                if let Ok(history) = self
                    .registry_router
                    .fetch_release_history(
                        ecosystem,
                        &target.name,
                        &target.target_version.version,
                        &target.latest_version,
                    )
                    .await
                {
                    let count = history.len().saturating_sub(1);

                    let format_availability = |time: chrono::DateTime<chrono::Utc>| -> String {
                        let now = chrono::Utc::now();
                        let min_age = target
                            .minimum_release_age
                            .unwrap_or(chrono::Duration::zero());
                        let available_time = time + min_age;
                        let remaining = available_time.signed_duration_since(now).num_seconds();
                        let days = (remaining as f64 / 86400.0).ceil() as i64;
                        if days > 1 {
                            format!("in {} days", days)
                        } else {
                            format!("{} UTC", available_time.format("%A at %H:%M"))
                        }
                    };

                    let subject = if all_same && targets.len() > 1 {
                        package_group.to_string()
                    } else {
                        target.name.clone()
                    };

                    if count == 1 {
                        let availability = if let Some(head) = history.first() {
                            format_availability(head.publish_time)
                        } else {
                            "now".to_string()
                        };
                        body.push_str(&format!(
                            "> [!IMPORTANT]\n> 1 more recent version of `{}` is released ({}). However, that version is not approved yet by policy. It will become available {}.\n\n",
                            subject, target.latest_version, availability
                        ));
                    } else if count > 1 {
                        let latest_version = history
                            .first()
                            .map(|v| v.version.clone())
                            .unwrap_or_else(|| target.latest_version.clone());
                        let latest_availability = history
                            .first()
                            .map(|v| format_availability(v.publish_time))
                            .unwrap_or_else(|| "now".to_string());

                        let next_version_idx = history.len().saturating_sub(2);
                        let next_version = history
                            .get(next_version_idx)
                            .map(|v| v.version.clone())
                            .unwrap_or_default();
                        let next_availability = history
                            .get(next_version_idx)
                            .map(|v| format_availability(v.publish_time))
                            .unwrap_or_else(|| "now".to_string());

                        body.push_str(&format!(
                            "> [!IMPORTANT]\n> {} more recent versions of `{}` are released, the latest one being {}. However, these versions are not approved yet by policy. The next version ({}) becomes available {}, the latest version ({}) becomes available {}.\n\n",
                            count, subject, target.latest_version, next_version, next_availability, latest_version, latest_availability
                        ));
                    } else {
                        body.push_str(&format!(
                            "> [!IMPORTANT]\n> A more recent version of `{}` is released ({}). However, it is not approved yet by policy.\n\n",
                            subject, target.latest_version
                        ));
                    }
                } else {
                    let subject = if all_same && targets.len() > 1 {
                        package_group.to_string()
                    } else {
                        target.name.clone()
                    };
                    body.push_str(&format!(
                        "> [!IMPORTANT]\n> A more recent version of `{}` is released ({}). However, it is not approved yet by policy.\n\n",
                        subject, target.latest_version
                    ));
                }
            }
        }

        body.push_str("# Release History\n\n");

        let history_targets = if all_same && targets.len() > 1 {
            vec![(
                &targets[0],
                &targets[0].current_version.version,
                &targets[0].target_version.version,
            )]
        } else {
            let mut unique_urls = std::collections::HashMap::new();
            for t in targets {
                let url = t
                    .package_info
                    .repo_url
                    .clone()
                    .unwrap_or_else(|| t.name.clone());

                let entry = unique_urls.entry(url).or_insert((
                    t,
                    &t.current_version.version,
                    &t.target_version.version,
                ));

                if let (Ok(ver_curr), Ok(ver_oldest)) = (
                    semver::Version::parse(&t.current_version.version),
                    semver::Version::parse(entry.1),
                ) {
                    if ver_curr < ver_oldest {
                        entry.1 = &t.current_version.version;
                    }
                }

                if let (Ok(ver_new), Ok(ver_highest)) = (
                    semver::Version::parse(&t.target_version.version),
                    semver::Version::parse(entry.2),
                ) {
                    if ver_new > ver_highest {
                        entry.2 = &t.target_version.version;
                    }
                }
            }
            unique_urls.into_values().collect()
        };

        for (target, hist_current, hist_target) in history_targets {
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
                .fetch_release_history(ecosystem, &target.name, hist_current, hist_target)
                .await
            {
                if history.is_empty() {
                    continue;
                }

                if !all_same && targets.len() > 1 {
                    body.push_str(&format!("## `{}`\n\n", target.name));
                }

                let num_releases = history
                    .iter()
                    .filter(|v| {
                        v.version.trim_start_matches('v') != hist_current.trim_start_matches('v')
                    })
                    .count();
                let plural = if num_releases == 1 { "" } else { "s" };

                if let Some(latest) = history.first() {
                    let latest_age_str = format_time_ago(latest.publish_time);

                    if let Some(current) = history.iter().find(|v| {
                        v.version.trim_start_matches('v') == hist_current.trim_start_matches('v')
                    }) {
                        let diff = latest
                            .publish_time
                            .signed_duration_since(current.publish_time);
                        let diff_str = format_duration(diff);
                        body.push_str(&format!("The history covers {} release{}. The latest version was published {} ({} after the current version).\n\n", num_releases, plural, latest_age_str, diff_str));
                    } else {
                        body.push_str(&format!("The history covers {} release{}. The latest version was published {}.\n\n", num_releases, plural, latest_age_str));
                    }
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

                    // Attempt to resolve release notes (and the resolved tag)
                    let mut fetched_md = None;
                    let mut display_tag = None;

                    if let Some(res) = &resolver {
                        if let Ok(Some((tag, md))) =
                            res.resolve_release_notes(&release.version).await
                        {
                            display_tag = Some(tag);
                            fetched_md = Some(md);
                        }
                    }

                    let tag_for_url = display_tag.unwrap_or_else(|| {
                        if target.name.contains('/') {
                            format!("{}@{}", target.name, release.version)
                        } else {
                            format!("v{}", release.version)
                        }
                    });

                    // Percent-encode the tag for the GitHub URL
                    let mut encoded_tag = String::with_capacity(tag_for_url.len() * 3);
                    for byte in tag_for_url.bytes() {
                        match byte {
                            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                                encoded_tag.push(byte as char);
                            }
                            _ => {
                                encoded_tag.push_str(&format!("%{:02X}", byte));
                            }
                        }
                    }

                    if !repo_url.is_empty() && repo_url.contains("github.com") {
                        let github_url = format!("{}/releases/tag/{}", repo_url, encoded_tag);
                        body.push_str(&format!(
                            "## [{}]({})\n{}\n\n",
                            release.version, github_url, time_str
                        ));
                    } else {
                        body.push_str(&format!("## {}\n{}\n\n", release.version, time_str));
                    }

                    if current_length > 60_000 {
                        if !repo_url.is_empty() && repo_url.contains("github.com") {
                            let github_url = format!("{}/releases/tag/{}", repo_url, encoded_tag);
                            body.push_str(&format!("> *Changelog truncated due to GitHub PR size limits. [View release notes on GitHub]({})*\n\n", github_url));
                        } else {
                            body.push_str(
                                "> *Changelog truncated due to GitHub PR size limits.*\n\n",
                            );
                        }
                        continue;
                    }

                    if let Some(md) = fetched_md {
                        // The heading was shifted to level 3 inside resolve_release_notes, but we
                        // already emit `##` for the version above. The markdown resolver shift function
                        // expects a target top level (which is 3 in github_release_notes).
                        body.push_str(&md);
                        body.push_str("\n\n");
                        current_length = body.len();
                    }
                }
            }
        }

        Ok(body)
    }
}
