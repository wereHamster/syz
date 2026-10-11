use bytes::Bytes;
use reqwest_middleware::{ClientBuilder, ClientWithMiddleware};
use reqwest_retry::{policies::ExponentialBackoff, RetryTransientMiddleware};
use reqwest_tracing::TracingMiddleware;
use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::{OnceCell, RwLock, Semaphore};

/// A cache entry. It holds the outcome of the fetch, errors included, so every caller waiting on
/// the fetch sees the same result.
type Entry = OnceCell<Result<Bytes, Arc<anyhow::Error>>>;

#[derive(Clone)]
pub struct HttpAgent {
    client: ClientWithMiddleware,

    /// This semaphore limits how many concurrent HTTP requests this agent is allowed
    /// to send out. We do this to not overload the services we talk to.
    semaphore: Arc<Semaphore>,

    /// A simple response cache, key is the URL and value is the raw response body. Each entry
    /// is a cell so concurrent requests for the same key share a single fetch.
    cache: Arc<RwLock<HashMap<String, Arc<Entry>>>>,
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

    /// Returns the cached body for `key`, or runs `fetch` to produce it. Callers asking for the
    /// same key while a fetch is running wait for that fetch and share its outcome, success or
    /// failure, instead of starting their own. A failed fetch is evicted afterwards so only later
    /// calls retry it.
    async fn cached<F, Fut>(&self, key: &str, fetch: F) -> anyhow::Result<Bytes>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = anyhow::Result<Bytes>>,
    {
        let existing = self.cache.read().await.get(key).cloned();
        let cell = match existing {
            Some(cell) => cell,
            None => self
                .cache
                .write()
                .await
                .entry(key.to_string())
                .or_default()
                .clone(),
        };

        let outcome = cell
            .get_or_init(|| async { fetch().await.map_err(Arc::new) })
            .await;

        match outcome {
            Ok(bytes) => Ok(bytes.clone()),
            Err(error) => {
                let mut cache = self.cache.write().await;
                if cache
                    .get(key)
                    .is_some_and(|entry| Arc::ptr_eq(entry, &cell))
                {
                    cache.remove(key);
                }

                Err(anyhow::anyhow!("{:#}", error))
            }
        }
    }

    async fn fetch(&self, request: reqwest_middleware::RequestBuilder) -> anyhow::Result<Bytes> {
        let _permit = self.semaphore.acquire().await?;

        let response = request.send().await?;
        let status = response.status();
        let bytes = response.bytes().await?;

        if !status.is_success() {
            return Err(anyhow::anyhow!(
                "HTTP error: {} - {}",
                status,
                String::from_utf8_lossy(&bytes)
            ));
        }

        Ok(bytes)
    }

    async fn get(&self, url: &str) -> anyhow::Result<Bytes> {
        self.cached(url, || async {
            tracing::info!("GET {}", url);
            self.fetch(self.client.get(url)).await
        })
        .await
    }

    pub async fn text(&self, url: &str) -> anyhow::Result<String> {
        let bytes = self.get(url).await?;
        Ok(String::from_utf8_lossy(&bytes).to_string())
    }

    pub async fn json<T: serde::de::DeserializeOwned>(&self, url: &str) -> anyhow::Result<T> {
        let bytes = self.get(url).await?;
        Ok(serde_json::from_slice(&bytes)?)
    }

    pub async fn post_json_body<B: serde::Serialize, T: serde::de::DeserializeOwned>(
        &self,
        url: &str,
        body: &B,
    ) -> anyhow::Result<T> {
        let body_str = serde_json::to_string(body)?;
        let cache_key = format!("POST {} {}", url, body_str);

        let bytes = self
            .cached(&cache_key, || async {
                tracing::info!("POST {}", url);
                self.fetch(
                    self.client
                        .post(url)
                        .header("Content-Type", "application/json")
                        .body(body_str.clone()),
                )
                .await
            })
            .await?;

        Ok(serde_json::from_slice(&bytes)?)
    }
}

impl Default for HttpAgent {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// Serves a slow `GET /ok` and a slow `GET /fail` that returns a 400, counting the requests
    /// each of them receives.
    async fn serve() -> (String, Arc<AtomicUsize>, Arc<AtomicUsize>) {
        let hits = Arc::new(AtomicUsize::new(0));
        let counter = hits.clone();
        let fail_hits = Arc::new(AtomicUsize::new(0));
        let fail_counter = fail_hits.clone();
        let app = axum::Router::new()
            .route(
                "/ok",
                axum::routing::get(move || {
                    let counter = counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        "{\"a\":1}"
                    }
                }),
            )
            .route(
                "/fail",
                axum::routing::get(move || {
                    let counter = fail_counter.clone();
                    async move {
                        counter.fetch_add(1, Ordering::SeqCst);
                        tokio::time::sleep(Duration::from_millis(200)).await;
                        (axum::http::StatusCode::BAD_REQUEST, "nope")
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{}", addr), hits, fail_hits)
    }

    #[tokio::test]
    async fn concurrent_requests_for_one_url_share_a_fetch() {
        let (base, hits, _) = serve().await;
        let agent = HttpAgent::new();
        let url = format!("{}/ok", base);

        // More callers than semaphore permits, so some of them would otherwise wait for a
        // permit and then fetch again after the first response is already cached.
        let tasks: Vec<_> = (0..30)
            .map(|_| {
                let (agent, url) = (agent.clone(), url.clone());
                tokio::spawn(async move { agent.json::<serde_json::Value>(&url).await })
            })
            .collect();
        for task in tasks {
            task.await.unwrap().unwrap();
        }

        assert_eq!(hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn concurrent_requests_share_a_failure_and_later_calls_retry() {
        let (base, _, fail_hits) = serve().await;
        let agent = HttpAgent::new();
        let url = format!("{}/fail", base);

        let tasks: Vec<_> = (0..30)
            .map(|_| {
                let (agent, url) = (agent.clone(), url.clone());
                tokio::spawn(async move { agent.json::<serde_json::Value>(&url).await })
            })
            .collect();
        for task in tasks {
            let error = task.await.unwrap().unwrap_err();
            assert!(error.to_string().contains("400"));
        }

        // The callers that were waiting shared one failed attempt, and it was not cached.
        assert_eq!(fail_hits.load(Ordering::SeqCst), 1);
        assert!(agent.cache.read().await.get(&url).is_none());

        agent.json::<serde_json::Value>(&url).await.unwrap_err();
        assert_eq!(fail_hits.load(Ordering::SeqCst), 2);
    }
}
