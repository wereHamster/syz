//! HTTP transport contract: the crate never builds its own client and never
//! sees credentials. Consumers implement [`HttpClient`] over their existing
//! (possibly authenticated, possibly installation-aware) HTTP stack.
//!
//! Contract:
//! - Auth is entirely the implementer's concern.
//! - One GET per call; no internal retries (the crate reports
//!   rate limits with `retry_after` so policy can be decided above it).
//! - Status, headers, and body are preserved: HTTP error *statuses* are
//!   `Ok(HttpResponse)` and classified by the forge; [`TransportError`] is
//!   only for failures before a response exists.

use std::collections::HashMap;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

/// Case-insensitive header lookup over a snapshot of response headers.
#[derive(Clone, Debug, Default)]
pub struct Headers(HashMap<String, String>);

impl Headers {
    pub fn new(map: impl IntoIterator<Item = (String, String)>) -> Self {
        Self(
            map.into_iter()
                .map(|(k, v)| (k.to_lowercase(), v))
                .collect(),
        )
    }

    /// Lookup is case-insensitive; returns the first value for the name.
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(&name.to_lowercase()).map(String::as_str)
    }

    /// All values for a name (some forges emit repeated headers, e.g. `Link`).
    pub fn get_all(&self, name: &str) -> Vec<&str> {
        self.0
            .get(&name.to_lowercase())
            .map(|v| vec![v.as_str()])
            .unwrap_or_default()
    }
}

/// A raw HTTP GET response. The status is preserved so the forge can
/// classify outcomes (404 → miss, 403/429 → rate limit) instead of
/// string-sniffing error text.
#[derive(Clone, Debug)]
pub struct HttpResponse {
    pub status: u16,
    pub headers: Headers,
    pub body: Bytes,
}

impl HttpResponse {
    pub fn status(&self) -> u16 {
        self.status
    }

    /// `Link` header, used by the GitHub forge for pagination.
    pub fn link_header(&self) -> Option<&str> {
        self.headers.get("link")
    }
}

/// Only transport-level failures (DNS, connect, TLS, timeout).
#[derive(Debug)]
pub struct TransportError(anyhow::Error);

impl TransportError {
    pub fn new(err: impl Into<anyhow::Error>) -> Self {
        Self(err.into())
    }

    pub fn into_inner(self) -> anyhow::Error {
        self.0
    }

    pub fn as_inner(&self) -> &anyhow::Error {
        &self.0
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl std::error::Error for TransportError {}

/// GET a URL. Must not retry internally (callers own retry policy).
#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn get(&self, url: &str) -> Result<HttpResponse, TransportError>;
}
