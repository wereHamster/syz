//! Scriptable [`Forge`] for tests, exposed under the `test-util` feature.
//! Counts calls so tests can assert fetch counts and probe order.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::forge::{rank_changelog_paths, Forge};
use crate::types::{Error, PackageName, ReleaseInfo, Result, Tag};

/// In-memory [`Forge`]: canned release pages, changelog files and call
/// counters; can be told to emit rate-limit errors.
#[derive(Clone, Default)]
pub struct MockForge {
    inner: Arc<Mutex<MockState>>,
}

#[derive(Default)]
struct MockState {
    releases: Vec<ReleaseInfo>,
    /// Overrides the by-tag view when set (simulating releases older than
    /// the fetched list pages).
    by_tag_releases: Option<Vec<ReleaseInfo>>,
    files: HashMap<String, Option<String>>,
    release_pages_calls: usize,
    by_tag_calls: usize,
    probed_tags: Vec<String>,
    tree_calls: usize,
    file_calls: usize,
    rate_limit_on: bool,
}

impl MockForge {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_releases(self, releases: Vec<ReleaseInfo>) -> Self {
        self.inner.lock().unwrap().releases = releases;
        self
    }

    pub fn with_file(self, path: &str, content: Option<&str>) -> Self {
        self.inner
            .lock()
            .unwrap()
            .files
            .insert(path.to_string(), content.map(String::from));
        self
    }

    /// Separate by-tag view; defaults to the releases list.
    pub fn with_by_tag_releases(self, releases: Vec<ReleaseInfo>) -> Self {
        self.inner.lock().unwrap().by_tag_releases = Some(releases);
        self
    }

    pub fn rate_limited(self) -> Self {
        self.inner.lock().unwrap().rate_limit_on = true;
        self
    }

    pub fn release_pages_calls(&self) -> usize {
        self.inner.lock().unwrap().release_pages_calls
    }

    pub fn by_tag_calls(&self) -> usize {
        self.inner.lock().unwrap().by_tag_calls
    }

    /// Tags passed to `release_by_tag`, in call order.
    pub fn probed_tags(&self) -> Vec<String> {
        self.inner.lock().unwrap().probed_tags.clone()
    }

    pub fn tree_calls(&self) -> usize {
        self.inner.lock().unwrap().tree_calls
    }

    pub fn file_calls(&self) -> usize {
        self.inner.lock().unwrap().file_calls
    }
}

fn rate_limited() -> Error {
    Error::RateLimited { retry_after: None }
}

#[async_trait]
impl Forge for MockForge {
    async fn releases(&self, pages: usize) -> Result<Vec<ReleaseInfo>> {
        let mut st = self.inner.lock().unwrap();
        st.release_pages_calls += 1;
        if st.rate_limit_on {
            return Err(rate_limited());
        }
        // Simulate pagination: `pages` of up to 2 releases each.
        Ok(st.releases.iter().take(pages * 2).cloned().collect())
    }

    async fn changelog_candidates(&self, pkg: &PackageName) -> Result<Vec<String>> {
        let mut st = self.inner.lock().unwrap();
        st.tree_calls += 1;
        if st.rate_limit_on {
            return Err(rate_limited());
        }
        let paths: Vec<String> = st
            .files
            .keys()
            .filter(|p| crate::forge::is_changelog_path(p))
            .cloned()
            .collect();
        Ok(rank_changelog_paths(paths, pkg))
    }

    async fn read_file(&self, path: &str) -> Result<Option<String>> {
        let mut st = self.inner.lock().unwrap();
        st.file_calls += 1;
        if st.rate_limit_on {
            return Err(rate_limited());
        }
        Ok(st.files.get(path).cloned().flatten())
    }

    async fn release_by_tag(&self, tag: &Tag) -> Result<Option<ReleaseInfo>> {
        let mut st = self.inner.lock().unwrap();
        st.by_tag_calls += 1;
        st.probed_tags.push(tag.as_str().to_string());
        if st.rate_limit_on {
            return Err(rate_limited());
        }
        let view = st.by_tag_releases.as_ref().unwrap_or(&st.releases);
        Ok(view.iter().find(|r| &r.tag == tag).cloned())
    }
}
