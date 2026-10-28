use serde::Deserialize;

use crate::core::clients::github::GitHub;

pub fn extract_repo(action: &str) -> String {
    let parts: Vec<&str> = action.splitn(3, '/').collect();
    if parts.len() >= 2 {
        format!("{}/{}", parts[0], parts[1])
    } else {
        action.to_string()
    }
}

#[derive(Deserialize)]
struct GithubTag {
    object: GithubTagObject,
}

#[derive(Deserialize)]
struct GithubTagObject {
    sha: String,
    #[serde(rename = "type")]
    object_type: String,
}

pub async fn get_sha_for_tag(
    github_client: &GitHub,
    action_repo: &str,
    tag: &str,
) -> Option<String> {
    let route = format!("/repos/{}/git/ref/tags/{}", action_repo, tag);
    if let Ok(res) = github_client.get_json(&route).await {
        if let Ok(tag_data) = serde_json::from_value::<GithubTag>(res) {
            let sha = if tag_data.object.object_type == "tag" {
                let tag_route = format!("/repos/{}/git/tags/{}", action_repo, tag_data.object.sha);
                if let Ok(tag_res) = github_client.get_json(&tag_route).await {
                    if let Some(obj) = tag_res.get("object").and_then(|o| o.as_object()) {
                        obj.get("sha")
                            .and_then(|s| s.as_str())
                            .unwrap_or(&tag_data.object.sha)
                            .to_string()
                    } else {
                        tag_data.object.sha
                    }
                } else {
                    tag_data.object.sha
                }
            } else {
                tag_data.object.sha
            };
            return Some(sha);
        }
    }
    None
}

pub async fn get_tag_for_sha(
    github_client: &GitHub,
    action_repo: &str,
    sha: &str,
) -> Option<String> {
    let route = format!("/repos/{}/tags?per_page=100", action_repo);
    if let Ok(tags) = github_client.get_json(&route).await {
        if let Some(tags_array) = tags.as_array() {
            for tag in tags_array {
                if let Some(commit_sha) = tag
                    .get("commit")
                    .and_then(|c| c.get("sha"))
                    .and_then(|s| s.as_str())
                {
                    if commit_sha == sha {
                        if let Some(name) = tag.get("name").and_then(|n| n.as_str()) {
                            return Some(name.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

#[cfg(all(test, feature = "integration-tests"))]
mod tests {
    use super::*;

    // `actions/setup-ruby` is archived, so its tags don't change.

    #[tokio::test]
    async fn test_get_tag_for_sha() {
        let github = GitHub::for_tests().await.unwrap();

        let tag = get_tag_for_sha(
            &github,
            "actions/setup-ruby",
            "5f29a1cd8dfebf420691c4c9a0e832e2fae5a526",
        )
        .await;

        assert_eq!(tag.as_deref(), Some("v1.1.2"));
    }

    #[tokio::test]
    async fn test_get_sha_for_tag() {
        let github = GitHub::for_tests().await.unwrap();

        let sha = get_sha_for_tag(&github, "actions/setup-ruby", "v1.1.2").await;

        assert_eq!(
            sha.as_deref(),
            Some("5f29a1cd8dfebf420691c4c9a0e832e2fae5a526")
        );
    }
}
