//! Live integration tests against github.com. They need network access and
//! the `integration-tests` feature; CI enables it via `--all-features`.
//!
//! ```sh
//! cargo test -p release-notes --features integration-tests
//! ```
//!
//! Auth: `GITHUB_TOKEN` is used when set (CI passes the workflow's token);
//! otherwise the tests run unauthenticated, which is subject to the much
//! lower anonymous rate limit.
#![cfg(feature = "integration-tests")]

use std::sync::Arc;

use release_notes::{github, http, HttpClient, Options, PackageName, Resolver};

struct ReqwestClient(reqwest::Client);

impl ReqwestClient {
    fn new() -> anyhow::Result<Self> {
        let mut builder = reqwest::Client::builder().user_agent("syz-release-notes-tests");
        if let Ok(token) = std::env::var("GITHUB_TOKEN") {
            let mut headers = reqwest::header::HeaderMap::new();
            headers.insert(
                reqwest::header::AUTHORIZATION,
                reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))?,
            );
            builder = builder.default_headers(headers);
        }
        Ok(Self(builder.build()?))
    }
}

#[async_trait::async_trait]
impl HttpClient for ReqwestClient {
    async fn get(&self, url: &str) -> Result<http::HttpResponse, http::TransportError> {
        let resp = self
            .0
            .get(url)
            .send()
            .await
            .map_err(http::TransportError::new)?;

        let status = resp.status().as_u16();
        let mut header_map = std::collections::HashMap::new();
        for (name, value) in resp.headers() {
            if let Ok(v) = value.to_str() {
                header_map
                    .entry(name.as_str().to_string())
                    .and_modify(|existing: &mut String| {
                        existing.push_str(", ");
                        existing.push_str(v);
                    })
                    .or_insert_with(|| v.to_string());
            }
        }
        let body = resp.bytes().await.map_err(http::TransportError::new)?;

        Ok(http::HttpResponse {
            status,
            headers: http::Headers::new(header_map),
            body,
        })
    }
}

fn repo_url(url: &str) -> Arc<github::GitHubForge> {
    let repo = release_notes::Repository::parse(url).unwrap();
    let client: Arc<dyn HttpClient> = Arc::new(ReqwestClient::new().unwrap());
    Arc::new(github::GitHubForge::new(client, repo.owner, repo.name))
}

#[tokio::test]
async fn knip_release_notes() {
    let resolver = Resolver::new(
        repo_url("https://github.com/webpro-nl/knip"),
        Options::default(),
    );
    let notes = resolver
        .resolve(&PackageName::new("knip"), "6.12.2")
        .await
        .unwrap();

    let notes = notes.expect("Should find knip release notes");
    assert_eq!(
        notes.source.tag().unwrap(),
        &release_notes::Tag::new("knip@6.12.2")
    );
    assert!(matches!(
        notes.source,
        release_notes::Source::Release { .. }
    ));
    assert!(notes.markdown.contains("Fix symbol reporter file paths"));
}

#[tokio::test]
async fn sentry_core_release_notes() {
    let resolver = Resolver::new(
        repo_url("https://github.com/getsentry/sentry-javascript"),
        Options::default(),
    );
    let notes = resolver
        .resolve(&PackageName::new("@sentry/core"), "7.114.0")
        .await
        .unwrap();

    let notes = notes.expect("Should find @sentry/core release notes");
    assert_eq!(
        notes.source.tag().unwrap(),
        &release_notes::Tag::new("7.114.0")
    );
    assert!(matches!(
        notes.source,
        release_notes::Source::Release { .. }
    ));
    assert!(notes.markdown.contains("fix(browser/v7)"));
}

#[tokio::test]
async fn exifreader_release_notes() {
    let resolver = Resolver::new(
        repo_url("https://github.com/mattiasw/ExifReader"),
        Options::default(),
    );
    let notes = resolver
        .resolve(&PackageName::new("exifreader"), "4.37.0")
        .await
        .unwrap();

    let notes = notes.expect("Should find exifreader release notes");
    assert_eq!(
        notes.source.tag().unwrap(),
        &release_notes::Tag::new("v4.37.0")
    );
    assert!(matches!(
        notes.source,
        release_notes::Source::Release { .. }
    ));
}
