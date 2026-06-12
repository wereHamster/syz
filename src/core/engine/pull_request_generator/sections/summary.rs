use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::pull_request_generator::context::PullRequestGenerationContext;
use crate::core::engine::pull_request_generator::sections::PullRequestSectionGenerator;

pub struct SummarySection;

fn npmx_diff_url(name: &str, old: &str, new: &str) -> String {
    format!("https://npmx.dev/diff/{}/v/{}...{}", name, old, new)
}

#[async_trait]
impl PullRequestSectionGenerator for SummarySection {
    async fn generate(
        &self,
        ctx: &PullRequestGenerationContext<'_>,
        _current_length: usize,
    ) -> Result<Option<String>> {
        let targets = ctx.targets;
        let ecosystem = ctx.ecosystem;
        let package_group = ctx.package_group;
        let mut body = String::new();

        if targets.len() == 1 {
            let target = &targets[0];
            let mut repo_url = target.package_info.repo_url.clone().unwrap_or_default();
            if repo_url.is_empty() {
                if let Ok(info) = ctx
                    .registry_router
                    .fetch_package_info(ecosystem, &target.name)
                    .await
                {
                    repo_url = info.repo_url.unwrap_or_default();
                }
            }

            if !repo_url.is_empty() {
                if ecosystem == "npm" {
                    let diff_url = npmx_diff_url(
                        &target.name,
                        &target.current_version.version,
                        &target.target_version.version,
                    );
                    body.push_str(&format!(
                        "This PR updates [{}]({}) from version [{} to {}]({}).\n\n",
                        target.name,
                        repo_url,
                        target.current_version.version,
                        target.target_version.version,
                        diff_url
                    ));
                } else {
                    body.push_str(&format!(
                        "This PR updates [{}]({}) from version {} to {}.\n\n",
                        target.name,
                        repo_url,
                        target.current_version.version,
                        target.target_version.version
                    ));
                }
            } else if ecosystem == "npm" {
                let diff_url = npmx_diff_url(
                    &target.name,
                    &target.current_version.version,
                    &target.target_version.version,
                );
                body.push_str(&format!(
                    "This PR updates `{}` from version [{} to {}]({}).\n\n",
                    target.name,
                    target.current_version.version,
                    target.target_version.version,
                    diff_url
                ));
            } else {
                body.push_str(&format!(
                    "This PR updates `{}` from version {} to {}.\n\n",
                    target.name, target.current_version.version, target.target_version.version
                ));
            }
        } else if !targets.is_empty() {
            let first_target = &targets[0];
            let mut first_repo_url = first_target
                .package_info
                .repo_url
                .clone()
                .unwrap_or_default();
            if first_repo_url.is_empty() {
                if let Ok(info) = ctx
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
                    if ecosystem == "npm" {
                        let diff_url = npmx_diff_url(
                            &t.name,
                            &first_target.current_version.version,
                            &first_target.target_version.version,
                        );
                        body.push_str(&format!("- `{}` ([diff]({}))\n", t.name, diff_url));
                    } else {
                        body.push_str(&format!("- `{}`\n", t.name));
                    }
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
                        if let Ok(info) = ctx
                            .registry_router
                            .fetch_package_info(ecosystem, &target.name)
                            .await
                        {
                            repo_url = info.repo_url.unwrap_or_default();
                        }
                    }

                    if !repo_url.is_empty() {
                        if ecosystem == "npm" {
                            let diff_url = npmx_diff_url(
                                &target.name,
                                &target.current_version.version,
                                &target.target_version.version,
                            );
                            body.push_str(&format!(
                                "- [{}]({}) from [`{}` to `{}`]({})\n",
                                target.name,
                                repo_url,
                                target.current_version.version,
                                target.target_version.version,
                                diff_url
                            ));
                        } else {
                            body.push_str(&format!(
                                "- [{}]({}) from `{}` to `{}`\n",
                                target.name,
                                repo_url,
                                target.current_version.version,
                                target.target_version.version
                            ));
                        }
                    } else if ecosystem == "npm" {
                        let diff_url = npmx_diff_url(
                            &target.name,
                            &target.current_version.version,
                            &target.target_version.version,
                        );
                        body.push_str(&format!(
                            "- `{}` from [`{}` to `{}`]({})\n",
                            target.name,
                            target.current_version.version,
                            target.target_version.version,
                            diff_url
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

        if body.is_empty() {
            Ok(None)
        } else {
            Ok(Some(body))
        }
    }
}
