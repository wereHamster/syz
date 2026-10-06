//! Adapter: syz's installation-aware [`GitHub`] client as the
//! `release-notes` crate's [`HttpClient`].
//!
//! The release-notes crate produces absolute `https://api.github.com/…`
//! URLs; the adapter maps them back onto octocrab routes so that the
//! per-owner App-installation token selection inside `GitHub::get_response`
//! is preserved.

use async_trait::async_trait;

use release_notes::http::{Headers, HttpClient, HttpResponse, TransportError};

use crate::core::clients::github::GitHub;

#[derive(Clone)]
pub struct GitHubHttp {
    github: GitHub,
}

impl GitHubHttp {
    pub fn new(github: GitHub) -> Self {
        Self { github }
    }
}

#[async_trait]
impl HttpClient for GitHubHttp {
    async fn get(&self, url: &str) -> Result<HttpResponse, TransportError> {
        // The crate emits URLs against its published API base; the prefix
        // must match by construction (same constant the forge uses).
        let route = url
            .strip_prefix(release_notes::github::API_BASE)
            .ok_or_else(|| TransportError::new(anyhow::anyhow!("unexpected GitHub URL: {url}")))?
            .to_string();

        let response = self
            .github
            .get_response(&route)
            .await
            .map_err(TransportError::new)?;

        Ok(HttpResponse {
            status: response.status,
            headers: Headers::new(response.headers),
            body: bytes::Bytes::from(response.body),
        })
    }
}
