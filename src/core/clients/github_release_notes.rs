use anyhow::Result;
use async_trait::async_trait;
use regex::Regex;

use crate::core::clients::github::GitHub;
use crate::core::engine::releases::ReleaseNotesResolver;

pub struct GithubReleaseNotesResolver {
    client: GitHub,
    package_name: String,
    owner: String,
    repo: String,
}

impl GithubReleaseNotesResolver {
    pub fn new(client: GitHub, package_name: String, repo_url: String) -> Self {
        // Strip out "https://github.com/" and ".git"
        let clean_url = repo_url
            .replace("https://github.com/", "")
            .replace("http://github.com/", "")
            .replace("git://github.com/", "");
        let clean_url = clean_url.trim_end_matches(".git");

        let parts: Vec<&str> = clean_url.split('/').collect();
        let owner = parts.first().unwrap_or(&"").to_string();
        let repo = parts.get(1).unwrap_or(&"").to_string();

        Self {
            client,
            package_name,
            owner,
            repo,
        }
    }

    fn percent_encode(s: &str) -> String {
        let mut out = String::with_capacity(s.len() * 3);
        for byte in s.bytes() {
            match byte {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(byte as char);
                }
                _ => {
                    out.push_str(&format!("%{:02X}", byte));
                }
            }
        }
        out
    }

    async fn try_github_releases(&self, version: &str) -> Result<Option<(String, String)>> {
        if self.owner.is_empty() || self.repo.is_empty() {
            return Ok(None);
        }

        let mut candidate_tags = vec![
            format!("{}@{}", self.package_name, version),
            format!("v{}", version),
            version.to_string(),
            format!("{}-{}", self.package_name, version),
            format!("{}-v{}", self.package_name, version),
        ];

        let base_version = version.split('+').next().unwrap_or(version);
        if base_version != version {
            candidate_tags.extend(vec![
                format!("{}@{}", self.package_name, base_version),
                format!("v{}", base_version),
                base_version.to_string(),
                format!("{}-{}", self.package_name, base_version),
                format!("{}-v{}", self.package_name, base_version),
            ]);
        }

        if self.package_name.contains('/') {
            let pkg_parts: Vec<&str> = self.package_name.split('/').collect();
            let short_name = pkg_parts
                .last()
                .copied()
                .unwrap_or(self.package_name.as_str());

            candidate_tags = vec![
                format!("{}@{}", self.package_name, version),
                format!("{}-{}", self.package_name, version),
                format!("{}-v{}", self.package_name, version),
                format!("{}@{}", short_name, version),
                format!("{}-{}", short_name, version),
                format!("{}-v{}", short_name, version),
                format!("v{}", version),
                version.to_string(),
            ];

            if base_version != version {
                candidate_tags.extend(vec![
                    format!("{}@{}", self.package_name, base_version),
                    format!("{}-{}", self.package_name, base_version),
                    format!("{}-v{}", self.package_name, base_version),
                    format!("{}@{}", short_name, base_version),
                    format!("{}-{}", short_name, base_version),
                    format!("{}-v{}", short_name, base_version),
                    format!("v{}", base_version),
                    base_version.to_string(),
                ]);
            }
        }

        for tag in candidate_tags {
            let encoded_tag = Self::percent_encode(&tag);
            let route = format!(
                "/repos/{}/{}/releases/tags/{}",
                self.owner, self.repo, encoded_tag
            );

            match self.client.get_json(&route).await {
                Ok(release) => {
                    if let Some(body) = release.get("body").and_then(|b| b.as_str()) {
                        return Ok(Some((tag.clone(), body.to_string())));
                    }
                }
                Err(e) => {
                    let err_str = e.to_string().to_lowercase();
                    if err_str.contains("rate limit")
                        || (err_str.contains("forbidden") && !err_str.contains("404"))
                        || (err_str.contains("403") && !err_str.contains("404"))
                    {
                        return Err(anyhow::anyhow!("GitHub API rate limit exceeded (403 Forbidden) via authenticated client"));
                    }
                    if err_str.contains("not found") || err_str.contains("404") {
                        continue;
                    }
                    continue;
                }
            }
        }

        Ok(None)
    }

    async fn try_markdown_file(&self, version: &str) -> Result<Option<(String, String)>> {
        if self.owner.is_empty() || self.repo.is_empty() {
            return Ok(None);
        }

        let route = format!(
            "/repos/{}/{}/git/trees/HEAD?recursive=1",
            self.owner, self.repo
        );
        let tree_data = match self.client.get_json(&route).await {
            Ok(data) => data,
            Err(e) => {
                let err_str = e.to_string().to_lowercase();
                if err_str.contains("rate limit")
                    || err_str.contains("forbidden")
                    || err_str.contains("403")
                {
                    return Err(anyhow::anyhow!("GitHub API rate limit exceeded (403 Forbidden) fetching tree via authenticated client"));
                }
                return Ok(None);
            }
        };

        let mut changelog_paths = Vec::new();
        if let Some(tree) = tree_data.get("tree").and_then(|t| t.as_array()) {
            for item in tree {
                if let Some(path) = item.get("path").and_then(|p| p.as_str()) {
                    let lower_path = path.to_lowercase();
                    if lower_path.ends_with("changelog.md") || lower_path.ends_with("changelog") {
                        changelog_paths.push(path.to_string());
                    }
                }
            }
        }

        if changelog_paths.is_empty() {
            return Ok(None);
        }

        let pkg_parts: Vec<&str> = self.package_name.split('/').collect();
        let short_name = pkg_parts
            .last()
            .copied()
            .unwrap_or(self.package_name.as_str())
            .to_lowercase();

        changelog_paths.sort_by(|a, b| {
            let a_contains = a.to_lowercase().contains(&short_name);
            let b_contains = b.to_lowercase().contains(&short_name);
            if a_contains == b_contains {
                a.len().cmp(&b.len())
            } else {
                b_contains.cmp(&a_contains)
            }
        });

        let paths_to_check: Vec<String> = changelog_paths.into_iter().take(3).collect();

        for path in paths_to_check {
            let markdown = match self
                .client
                .get_foreign_file(&self.owner, &self.repo, &path)
                .await
            {
                Ok(Some(content)) => content,
                Ok(None) => continue,
                Err(e) => return Err(e),
            };

            let base_version = version.split('+').next().unwrap_or(version);
            let lines = markdown.lines();
            let mut extracted = String::new();
            let mut found = false;
            let mut target_level = 0;

            for line in lines {
                let trimmed = line.trim();

                let mut is_heading = false;
                let mut count = 0;
                for c in trimmed.chars() {
                    if c == '#' {
                        count += 1;
                    } else if c == ' ' && count > 0 {
                        is_heading = true;
                        break;
                    } else {
                        break;
                    }
                }

                if is_heading {
                    if found {
                        if count <= target_level {
                            break;
                        }
                    } else if trimmed.contains(version) || trimmed.contains(base_version) {
                        found = true;
                        target_level = count;
                        continue;
                    }
                }

                if found {
                    extracted.push_str(line);
                    extracted.push('\n');
                }
            }

            let final_extracted = extracted.trim().to_string();
            if found && !final_extracted.is_empty() {
                let display_tag = if self.package_name.contains('/') {
                    format!("{}@{}", self.package_name, version)
                } else {
                    format!("v{}", version)
                };
                return Ok(Some((display_tag, final_extracted)));
            }
        }

        Ok(None)
    }
}

pub fn shift_markdown_headings(markdown: &str, target_top_level: usize, version: &str) -> String {
    let base_version = version.split('+').next().unwrap_or(version);
    let mut min_heading_level = usize::MAX;
    let mut in_code_block = false;
    let mut skipped_first_heading = false;

    let mut lines = markdown.lines();
    let mut remaining_markdown: Vec<&str> = Vec::new();

    for line in lines.by_ref() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let mut is_heading = false;
        let mut count = 0;
        for c in trimmed.chars() {
            if c == '#' {
                count += 1;
            } else if c == ' ' && count > 0 {
                is_heading = true;
                break;
            } else {
                break;
            }
        }

        if is_heading && (trimmed.contains(version) || trimmed.contains(base_version)) {
            skipped_first_heading = true;
        } else {
            remaining_markdown.push(line);
        }
        break;
    }

    remaining_markdown.extend(lines);

    for line in &remaining_markdown {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            continue;
        }

        if !in_code_block {
            let mut is_heading = false;
            let mut count = 0;
            for c in line.chars() {
                if c == '#' {
                    count += 1;
                } else if c == ' ' && count > 0 {
                    is_heading = true;
                    break;
                } else {
                    break;
                }
            }

            if is_heading {
                min_heading_level = min_heading_level.min(count);
            }
        }
    }

    let shift_up_by = if min_heading_level != usize::MAX && target_top_level > min_heading_level {
        target_top_level - min_heading_level
    } else {
        0
    };

    let shift_down_by = if min_heading_level != usize::MAX && min_heading_level > target_top_level {
        min_heading_level - target_top_level
    } else {
        0
    };

    let mut out = String::with_capacity(markdown.len() + 100);
    let mut in_code_block = false;
    let shift_str = "#".repeat(shift_up_by);

    for line in &remaining_markdown {
        if line.starts_with("```") {
            in_code_block = !in_code_block;
            out.push_str(line);
            out.push('\n');
            continue;
        }

        if !in_code_block {
            let mut is_heading = false;
            let mut count = 0;
            for c in line.chars() {
                if c == '#' {
                    count += 1;
                } else if c == ' ' && count > 0 {
                    is_heading = true;
                    break;
                } else {
                    break;
                }
            }

            if is_heading {
                if shift_up_by > 0 {
                    out.push_str(&shift_str);
                    out.push_str(line);
                } else if shift_down_by > 0 && count > shift_down_by {
                    let new_line = &line[shift_down_by..];
                    out.push_str(new_line);
                } else {
                    out.push_str(line);
                }
                out.push('\n');
                continue;
            }
        }

        out.push_str(line);
        out.push('\n');
    }

    let final_out = out.trim().to_string();
    if final_out.is_empty() && skipped_first_heading {
        return String::new();
    }

    final_out
}

pub fn sanitize_markdown(markdown: &str) -> String {
    let mut out = markdown
        .replace("https://github.com/", "https://redirect.github.com/")
        .replace("http://github.com/", "https://redirect.github.com/");

    let re_mention = Regex::new(r"@([a-zA-Z0-9-]+)").unwrap();
    out = re_mention
        .replace_all(&out, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap();
            let start = m.start();
            let end = m.end();

            let mut skip = false;
            if start > 0 {
                let prev_char = out[..start].chars().last().unwrap();
                if prev_char.is_ascii_alphanumeric() || prev_char == '_' {
                    skip = true;
                }
            }
            if end < out.len() {
                let next_char = out[end..].chars().next().unwrap();
                if next_char == '/' {
                    skip = true;
                }
            }

            if skip {
                caps[0].to_string()
            } else {
                format!("@&#8203;{}", &caps[1])
            }
        })
        .to_string();

    let re_issue = Regex::new(r"#([0-9]+)").unwrap();
    out = re_issue
        .replace_all(&out, |caps: &regex::Captures| {
            let m = caps.get(0).unwrap();
            let start = m.start();

            let mut skip = false;
            if start > 0 {
                let prefix = &out[..start];
                let prev_char = prefix.chars().last().unwrap();

                if prev_char == '_' || prev_char == '&' {
                    skip = true;
                }

                if prefix.ends_with("](")
                    || prefix.ends_with("href=\"")
                    || prefix.ends_with("href='")
                    || prefix.ends_with("=\"")
                    || prefix.ends_with("='")
                {
                    skip = true;
                }
            }

            if skip {
                caps[0].to_string()
            } else {
                format!("#&#8203;{}", &caps[1])
            }
        })
        .to_string();

    out
}

#[async_trait]
impl ReleaseNotesResolver for GithubReleaseNotesResolver {
    async fn resolve_release_notes(&self, version: &str) -> Result<Option<(String, String)>> {
        if let Ok(Some((tag, md))) = self.try_github_releases(version).await {
            let shifted = shift_markdown_headings(&md, 3, version);
            let rewritten = sanitize_markdown(&shifted);
            return Ok(Some((tag, rewritten)));
        }

        if let Ok(Some((tag, md))) = self.try_markdown_file(version).await {
            let shifted = shift_markdown_headings(&md, 3, version);
            let rewritten = sanitize_markdown(&shifted);
            return Ok(Some((tag, rewritten)));
        }

        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn get_test_client() -> GitHub {
        let _ = rustls::crypto::ring::default_provider().install_default();

        GitHub::new().await.unwrap()
    }

    #[tokio::test]
    async fn test_knip_release_notes() {
        let client = get_test_client().await;
        let resolver = GithubReleaseNotesResolver::new(
            client,
            "knip".to_string(),
            "https://github.com/webpro-nl/knip".to_string(),
        );

        let result = resolver.resolve_release_notes("6.12.2").await.unwrap();
        assert!(result.is_some(), "Should find knip release notes");
        let (tag, md) = result.unwrap();
        assert_eq!(tag, "knip@6.12.2");
        assert!(md.contains("Fix symbol reporter file paths"));
    }

    #[tokio::test]
    async fn test_sentry_release_notes() {
        let client = get_test_client().await;
        let resolver = GithubReleaseNotesResolver::new(
            client,
            "@sentry/core".to_string(),
            "https://github.com/getsentry/sentry-javascript".to_string(),
        );

        let result = resolver.resolve_release_notes("7.114.0").await.unwrap();
        assert!(result.is_some(), "Should find @sentry/core release notes");
        let (tag, md) = result.unwrap();
        println!("Sentry TAG: {}", tag);
        println!("Sentry MD:\n{}", md);
        assert_eq!(tag, "7.114.0");
        assert!(md.contains("fix(browser/v7)"));
    }

    #[tokio::test]
    async fn test_exifreader_release_notes() {
        let client = get_test_client().await;
        let resolver = GithubReleaseNotesResolver::new(
            client,
            "exifreader".to_string(),
            "https://github.com/mattiasw/ExifReader".to_string(),
        );

        let result = resolver.resolve_release_notes("4.37.0").await.unwrap();
        assert!(result.is_some(), "Should find exifreader release notes");
        let (tag, _md) = result.unwrap();
        assert_eq!(tag, "v4.37.0");
    }

    #[test]
    fn test_sanitize_markdown() {
        let input = "Fixed by @alice and @bob in #123. See also https://github.com/foo/bar. @apollo/server #456 [link](#789) <a href=\"#000\">#111</a> `code #222` `code @user` owner/repo#999";
        let expected = "Fixed by @&#8203;alice and @&#8203;bob in #&#8203;123. See also https://redirect.github.com/foo/bar. @apollo/server #&#8203;456 [link](#789) <a href=\"#000\">#&#8203;111</a> `code #&#8203;222` `code @&#8203;user` owner/repo#&#8203;999";
        assert_eq!(sanitize_markdown(input), expected);
    }
}
