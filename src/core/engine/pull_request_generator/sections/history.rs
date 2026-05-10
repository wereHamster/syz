use anyhow::Result;
use async_trait::async_trait;

use crate::core::clients::github::GitHub;
use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;
use crate::core::engine::pull_request_generator::formatting::{format_duration, format_time_ago};
use crate::core::engine::pull_request_generator::sections::PullRequestSectionGenerator;
use crate::core::engine::releases::ReleaseNotesResolver;

pub struct HistorySection;

impl HistorySection {
    fn release_notes_resolver(
        &self,
        package_name: &str,
        repo_url: &str,
        github: GitHub,
    ) -> Option<Box<dyn ReleaseNotesResolver>> {
        if repo_url.contains("github.com") {
            Some(Box::new(
                crate::core::clients::github_release_notes::GithubReleaseNotesResolver::new(
                    github,
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
impl PullRequestSectionGenerator for HistorySection {
    async fn generate(&self, ctx: &PullRequestGenerationContext<'_>) -> Result<Option<String>> {
        let targets = ctx.targets;
        if targets.is_empty() {
            return Ok(None);
        }

        let mut body = String::new();
        body.push_str("# Release History\n\n");

        let all_same = targets.iter().all(|t| {
            t.current_version.version == targets[0].current_version.version
                && t.target_version.version == targets[0].target_version.version
        });

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

        let mut added_history = false;

        for (target, hist_current, hist_target) in history_targets {
            let mut repo_url = target.package_info.repo_url.clone().unwrap_or_default();
            if repo_url.is_empty() {
                if let Ok(info) = ctx
                    .registry_router
                    .fetch_package_info(ctx.ecosystem, &target.name)
                    .await
                {
                    repo_url = info.repo_url.unwrap_or_default();
                }
            }

            if let Ok(history) = ctx
                .registry_router
                .fetch_release_history(ctx.ecosystem, &target.name, hist_current, hist_target)
                .await
            {
                if history.is_empty() {
                    continue;
                }

                added_history = true;

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
                    self.release_notes_resolver(&target.name, &repo_url, ctx.github.clone())
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
                        body.push_str(&md);
                        body.push_str("\n\n");
                        current_length = body.len();
                    }
                }
            }
        }

        if added_history {
            Ok(Some(body))
        } else {
            Ok(None)
        }
    }
}
