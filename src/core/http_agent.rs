use bytes::Bytes;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};
use reqwest_tracing::TracingMiddleware;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{RwLock, Semaphore};

#[derive(Clone)]
pub struct HttpAgent {
    client: ClientWithMiddleware,

    /// This semaphore limits how many concurrent HTTP requests this agent is allowed
    /// to send out. We do this to not overload the services we talk to.
    semaphore: Arc<Semaphore>,

    /// A simple response cache, key is the URL and value is the raw response body.
    cache: Arc<RwLock<HashMap<String, Bytes>>>,
}

impl HttpAgent {
    pub fn new() -> Self {
        let reqwest_client = reqwest::Client::builder()
            .user_agent(format!("Syz/{}", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        let client = ClientBuilder::new(reqwest_client)
            .with(TracingMiddleware::default())
            .with(RetryTransientMiddleware::new_with_policy(
                ExponentialBackoff::builder().build_with_max_retries(3),
            ))
            .build();

        Self {
            client,
            semaphore: Arc::new(Semaphore::new(10)),
            cache: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    pub async fn purge(&self) {
        let mut cache = self.cache.write().await;
        cache.clear();
    }

    async fn get(&self, url: &str) -> anyhow::Result<reqwest::Response> {
        let _permit = self.semaphore.acquire().await?;

        tracing::info!("GET {}", url);

        let response = self.client.get(url).send().await?;

        Ok(response)
    }

    pub async fn text(&self, url: &str) -> anyhow::Result<String> {
        {
            let cache = self.cache.read().await;
            if let Some(bytes) = cache.get(url) {
                let text = String::from_utf8_lossy(bytes).to_string();
                return Ok(text);
            }
        }

        let response = self.get(url).await?;
        let bytes = response.bytes().await?;

        let mut cache = self.cache.write().await;
        cache.insert(url.to_string(), bytes.clone());

        let text = String::from_utf8_lossy(&bytes).to_string();
        Ok(text)
    }

    pub async fn json<T: serde::de::DeserializeOwned>(&self, url: &str) -> anyhow::Result<T> {
        {
            let cache = self.cache.read().await;
            if let Some(bytes) = cache.get(url) {
                let json = serde_json::from_slice(bytes)?;
                return Ok(json);
            }
        }

        let response = self.get(url).await?;
        let status = response.status();
        let bytes = response.bytes().await?;

        let mut cache = self.cache.write().await;
        cache.insert(url.to_string(), bytes.clone());

        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "HTTP error: {} - {}",
                status,
                String::from_utf8_lossy(&bytes)
            ));
        }

        let json = serde_json::from_slice(&bytes)?;

        Ok(json)
    }

    pub async fn post_json_body<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> anyhow::Result<T> {
        let body_str = serde_json::to_string(body)?;
        let cache_key = format!("POST {} {}", url, body_str);

        {
            let cache = self.cache.read().await;
            if let Some(bytes) = cache.get(&cache_key) {
                let json = serde_json::from_slice(bytes)?;
                return Ok(json);
            }
        }

        let _permit = self.semaphore.acquire().await?;
        tracing::info!("POST {}", url);

        let response = self
            .client
            .post(url)
            .header("Content-Type", "application/json")
            .body(body_str.clone())
            .send()
            .await?;
        let bytes = response.bytes().await?;

        let mut cache = self.cache.write().await;
        cache.insert(cache_key, bytes.clone());

        let json = serde_json::from_slice(&bytes)?;

        Ok(json)
    }
}

impl Default for HttpAgent {
    fn default() -> Self {
        Self::new()
    }
}
