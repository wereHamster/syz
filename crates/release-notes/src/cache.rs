//! Single-flight caches over the forge. Locks are held across the fetch
//! await: concurrent calls on the same [`crate::Resolver`] collapse into one
//! fetch per unique resource. Errors are cached too (short-lived burst
//! protection; resolver instances are per-PR-generation).

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::types::{Error, ReleaseInfo, Result, Tag};

/// A cached error, cheap to clone.
#[derive(Clone, Debug)]
pub(crate) enum CacheErr {
    RateLimited(Option<std::time::Duration>),
    Transport(Arc<anyhow::Error>),
}

impl From<&Error> for CacheErr {
    fn from(e: &Error) -> Self {
        match e {
            Error::RateLimited { retry_after } => CacheErr::RateLimited(*retry_after),
            Error::Transport(err) => {
                CacheErr::Transport(Arc::new(anyhow::Error::msg(err.to_string())))
            }
        }
    }
}

impl From<CacheErr> for Error {
    fn from(e: CacheErr) -> Self {
        match e {
            CacheErr::RateLimited(retry_after) => Error::RateLimited { retry_after },
            CacheErr::Transport(err) => Error::Transport(anyhow::Error::msg(err.to_string())),
        }
    }
}

type Slot<T> = Mutex<Option<std::result::Result<T, CacheErr>>>;

/// Shared cache state; a `Resolver` and its clones share one instance.
#[derive(Default)]
pub(crate) struct Cache {
    releases: Slot<Vec<ReleaseInfo>>,
    probes: Mutex<HashMap<Tag, std::result::Result<Option<ReleaseInfo>, CacheErr>>>,
    files: Slot<HashMap<String, Option<String>>>,
}

impl Cache {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Cached releases list, fetched via `fetch` on first access.
    pub(crate) async fn releases(
        &self,
        fetch: impl AsyncFnOnce() -> Result<Vec<ReleaseInfo>>,
    ) -> Result<Vec<ReleaseInfo>> {
        let mut slot = self.releases.lock().await;
        match &*slot {
            Some(Ok(v)) => return Ok(v.clone()),
            Some(Err(e)) => return Err(e.clone().into()),
            None => {}
        }
        let fetched = fetch().await;
        *slot = Some(match &fetched {
            Ok(v) => Ok(v.clone()),
            Err(e) => Err(CacheErr::from(e)),
        });
        fetched
    }

    /// Cached per-tag probe, fetched via `fetch` on first access.
    pub(crate) async fn probe(
        &self,
        tag: &Tag,
        fetch: impl AsyncFnOnce() -> Result<Option<ReleaseInfo>>,
    ) -> Result<Option<ReleaseInfo>> {
        let mut probes = self.probes.lock().await;
        if let Some(cached) = probes.get(tag) {
            return cached.clone().map_err(Into::into);
        }
        let fetched = fetch().await;
        probes.insert(
            tag.clone(),
            match &fetched {
                Ok(v) => Ok(v.clone()),
                Err(e) => Err(CacheErr::from(e)),
            },
        );
        fetched
    }

    /// Cached changelog-file contents, fetched via `fetch` on first access.
    /// Returns the map keyed by path for all requested candidates.
    pub(crate) async fn files(
        &self,
        _paths: &[String],
        fetch: impl AsyncFnOnce() -> Result<HashMap<String, Option<String>>>,
    ) -> Result<HashMap<String, Option<String>>> {
        let mut slot = self.files.lock().await;
        match &*slot {
            Some(Ok(map)) => return Ok(map.clone()),
            Some(Err(e)) => return Err(e.clone().into()),
            None => {}
        }
        let fetched = fetch().await;
        *slot = Some(match &fetched {
            Ok(v) => Ok(v.clone()),
            Err(e) => Err(CacheErr::from(e)),
        });
        fetched
    }

    /// The releases list only if already fetched successfully — never
    /// triggers a fetch.
    pub(crate) fn releases_if_cached(&self) -> Option<Vec<ReleaseInfo>> {
        match &*self.releases.try_lock().ok()? {
            Some(Ok(v)) => Some(v.clone()),
            _ => None,
        }
    }
}
