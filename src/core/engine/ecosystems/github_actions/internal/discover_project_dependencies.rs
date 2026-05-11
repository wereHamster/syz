use anyhow::Result;
use regex::Regex;

use crate::core::engine::repository::ProjectRepositorySnapshot;
use crate::core::engine::{DiscoveredDependency, PURL};

pub async fn run(repo: &dyn ProjectRepositorySnapshot) -> Result<Vec<DiscoveredDependency>> {
    let files = repo.list_files().await?;
    let mut deps = Vec::new();

    for path in files {
        if path.starts_with(".github/workflows/")
            && (path.ends_with(".yml") || path.ends_with(".yaml"))
        {
            if let Ok(content) = repo.read_file(&path).await {
                deps.extend(parse_workflow_content(&content));
            }
        }
    }

    Ok(deps)
}

pub fn parse_workflow_content(content: &str) -> Vec<DiscoveredDependency> {
    let mut deps = Vec::new();

    // We can't decode the content as YAML because serde_yml does not preserve comments, and we
    // need the comment to extract the DiscoveredDependency requirement.
    let re = Regex::new(r"uses:\s+(?P<action>[a-zA-Z0-9_.-]+/[a-zA-Z0_./-]+)@(?P<ref>[^#\s]+)(?:[ \t]+#[ \t]*(?P<comment>v?[\d\.]+.*))?").unwrap();

    for line in content.lines() {
        if let Some(caps) = re.captures(line) {
            let action_path = caps.name("action").unwrap().as_str();
            let _action_ref = caps.name("ref").unwrap().as_str();
            let comment = caps.name("comment").map(|m| m.as_str().to_string());

            // The action_path from regex is "owner/repo" or "owner/repo/subpath"
            let action_parts: Vec<&str> = action_path.splitn(3, '/').collect();
            if action_parts.len() < 2 {
                continue;
            }

            let namespace = Some(action_parts[0].to_string());
            let name = action_parts[1].to_string();
            let subpath = if action_parts.len() == 3 {
                Some(action_parts[2].to_string())
            } else {
                None
            };

            // Extract version from the part after @ in the line
            let parts: Vec<&str> = line.split('@').collect();
            if parts.len() < 2 {
                continue;
            }
            let version_part = parts[1]
                .split(|c: char| c.is_whitespace() || c == '#')
                .next()
                .unwrap_or("");
            let version = if version_part.is_empty() {
                None
            } else {
                Some(version_part.to_string())
            };

            // Requirement is the comment (v5.0.0) if present, otherwise the version part
            let requirement = comment.unwrap_or_else(|| version.clone().unwrap_or_default());

            deps.push(DiscoveredDependency {
                purl: PURL {
                    ecosystem: "github-actions".to_string(),
                    namespace,
                    name,
                    subpath,
                    version,
                },
                requirement,
                minimum_release_age: Some(chrono::Duration::days(15)),
            });
        }
    }

    deps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_workflow_content_standard() {
        let yaml = r#"
jobs:
  build:
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with:
          node-version: '20'
"#;
        let deps = parse_workflow_content(yaml);
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0].purl.name, "checkout");
        assert_eq!(deps[0].purl.version.as_deref(), Some("v4"));
    }

    #[test]
    fn test_parse_workflow_content_subpath() {
        let yaml = r#"
jobs:
  build:
    steps:
      - uses: actions/setup-python/action@v5
"#;
        let deps = parse_workflow_content(yaml);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].purl.name, "setup-python");
        assert_eq!(deps[0].purl.subpath.as_deref(), Some("action"));
    }

    #[test]
    fn test_parse_workflow_content_with_comment() {
        let yaml = r#"
jobs:
  build:
    steps:
      - uses: pnpm/action-supports@fc06bc1257f339d1d5d8b3a19a8cae5388b55320 # v5.0.0
"#;
        let deps = parse_workflow_content(yaml);
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].purl.name, "action-supports");
        assert_eq!(
            deps[0].purl.version.as_deref(),
            Some("fc06bc1257f339d1d5d8b3a19a8cae5388b55320")
        );
        assert_eq!(deps[0].requirement, "v5.0.0");
    }

    #[test]
    fn test_parse_workflow_content_invalid_yaml() {
        let yaml = "this is not yaml";
        let deps = parse_workflow_content(yaml);
        assert!(deps.is_empty());
    }
}
