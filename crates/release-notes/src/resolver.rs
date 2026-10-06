//! The [`Resolver`]: strategy resolution, single-flight caches, and sticky
//! strategy memory shared across `resolve` and `resolve_batch`.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::cache::Cache;
use crate::forge::Forge;
use crate::markdown;
use crate::types::{
    base_version, candidate_tags, Candidate, PackageName, ReleaseInfo, ReleaseNotes, Result,
    Source, Tag, TagShape,
};

/// One multi-package lookup request (group PRs).
#[derive(Clone, Debug)]
pub struct Request {
    pub package: PackageName,
    pub version: String,
}

/// The strategy that produced notes, remembered across lookups on one
/// resolver (sticky first success; see module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Release-body lookup; the tag *shape* that matched.
    Release(TagShape),
    /// Changelog-file lookup.
    ChangelogFile,
}

/// Resolution options; defaults preserve legacy behavior.
#[derive(Clone, Copy, Debug)]
pub struct Options {
    /// Heading level the PR section expects notes to start at. Legacy: 3.
    pub heading_target: usize,
    /// Apply [`markdown::sanitize`] after shifting. Legacy: true.
    pub sanitize: bool,
    /// Max changelog file candidates fetched. Legacy: 3.
    pub max_changelog_candidates: usize,
    /// Pages of `/releases` fetched per list call (×100 releases/page for
    /// GitHub). Legacy parity: 3.
    pub release_pages: usize,
    /// Try release bodies before changelog files. Legacy: true.
    pub prefer_release_bodies: bool,
    /// Try the changelog file when no release matched. Legacy: true.
    pub use_changelog_file: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            heading_target: 3,
            sanitize: true,
            max_changelog_candidates: 3,
            release_pages: 3,
            prefer_release_bodies: true,
            use_changelog_file: true,
        }
    }
}

/// Resolves release notes for package versions against one repository.
/// Cloneable; clones share the caches and the learned strategy (Arc
/// interior).
pub struct Resolver {
    forge: Arc<dyn Forge>,
    options: Options,
    cache: Arc<Cache>,
    strategy: Mutex<Option<Strategy>>,
}

impl Resolver {
    pub fn new(forge: Arc<dyn Forge>, options: Options) -> Self {
        Self {
            forge,
            options,
            cache: Arc::new(Cache::new()),
            strategy: Mutex::new(None),
        }
    }

    /// The strategy learned so far, if any (sticky first success).
    pub async fn learned_strategy(&self) -> Option<Strategy> {
        *self.strategy.lock().await
    }

    /// Direct access to the (cached) releases list.
    pub async fn releases(&self) -> Result<Vec<ReleaseInfo>> {
        let pages = self.options.release_pages;
        self.cache.releases(|| self.forge.releases(pages)).await
    }

    /// Resolve notes for one version. `Ok(None)` = no notes found.
    /// `Err` only for real failures (rate limit, transport).
    /// Reads and writes the learned strategy.
    pub async fn resolve(&self, pkg: &PackageName, version: &str) -> Result<Option<ReleaseNotes>> {
        let requests = [Request {
            package: pkg.clone(),
            version: version.to_string(),
        }];
        let mut out = self.resolve_batch(&requests).await;
        out.pop().expect("one result per request")
    }

    /// Multi-package entry point (group PRs): several packages of the same
    /// repository, resolved sequentially in request order so that strategy
    /// learning is deterministic. Returns one result per request, aligned by
    /// index.
    pub async fn resolve_batch(&self, requests: &[Request]) -> Vec<Result<Option<ReleaseNotes>>> {
        let mut results = Vec::with_capacity(requests.len());
        for request in requests {
            let result = self.resolve_one(&request.package, &request.version).await;
            results.push(result);
        }
        results
    }

    async fn resolve_one(&self, pkg: &PackageName, version: &str) -> Result<Option<ReleaseNotes>> {
        let base = base_version(version);
        let mut needles: Vec<&str> = vec![version];
        if base != version {
            needles.push(base);
        }

        let remembered = *self.strategy.lock().await;
        let cands = candidate_tags(pkg, version);
        let cands = reorder_by_strategy(&cands, remembered);

        // Strategy A — release bodies.
        if self.options.prefer_release_bodies {
            if let Some(notes) = self
                .resolve_via_releases(pkg, version, &cands, &needles)
                .await?
            {
                return Ok(Some(notes));
            }
        }

        // Strategy B — changelog file.
        if self.options.use_changelog_file {
            if let Some(notes) = self
                .resolve_via_changelog_file(pkg, version, &cands, &needles)
                .await?
            {
                return Ok(Some(notes));
            }
        }

        Ok(None)
    }

    /// Strategy A: match candidates against the cached releases list, then
    /// (only for versions not covered by the fetched pages) fall back to
    /// per-tag probes — legacy deep-history parity.
    async fn resolve_via_releases(
        &self,
        _pkg: &PackageName,
        _version: &str,
        cands: &[Candidate],
        needles: &[&str],
    ) -> Result<Option<ReleaseNotes>> {
        let releases = self.releases().await?;

        if let Some(hit) = match_in_list(cands, &releases) {
            let notes = self.finish_release(hit, needles);
            self.remember(Strategy::Release(hit_shape(cands, &hit.tag)))
                .await;
            return Ok(Some(notes));
        }

        // Probe phase: releases older than the fetched pages.
        let list_tags: std::collections::HashSet<&str> =
            releases.iter().map(|r| r.tag.as_str()).collect();
        for cand in cands {
            if list_tags.contains(cand.tag.as_str()) {
                continue; // already seen in the list (miss: no string body)
            }
            let tag = cand.tag.clone();
            let probe = self
                .cache
                .probe(&tag, || self.forge.release_by_tag(&tag))
                .await?;
            if let Some(release) = probe {
                let notes = self.finish_release(&release, needles);
                self.remember(Strategy::Release(cand.shape)).await;
                return Ok(Some(notes));
            }
        }

        Ok(None)
    }

    /// Strategy B: extract the version's section from ranked changelog
    /// files.
    async fn resolve_via_changelog_file(
        &self,
        pkg: &PackageName,
        version: &str,
        cands: &[Candidate],
        needles: &[&str],
    ) -> Result<Option<ReleaseNotes>> {
        let paths = self.forge.changelog_candidates(pkg).await?;
        let paths: Vec<String> = paths
            .into_iter()
            .take(self.options.max_changelog_candidates)
            .collect();
        if paths.is_empty() {
            return Ok(None);
        }

        let contents = self
            .cache
            .files(&paths, || async {
                let mut map = std::collections::HashMap::new();
                for path in &paths {
                    map.insert(path.clone(), self.forge.read_file(path).await?);
                }
                Ok(map)
            })
            .await?;

        // A matched release tag is only known if the list is already cached
        // — never fetch for it. Any release matching a candidate tag counts
        // (body irrelevant); this prevents fabricated links.
        let cached_list = self.cache.releases_if_cached();
        let matched_tag = cached_list
            .as_ref()
            .and_then(|list| find_tag_in_list(cands, list))
            .map(|r| r.tag.clone());

        for path in &paths {
            let Some(content) = contents.get(path) else {
                continue;
            };
            let Some(section) =
                markdown::extract_section(content.as_deref().unwrap_or(""), version)
            else {
                continue;
            };

            let md = self.postprocess(&section, needles);
            self.remember(Strategy::ChangelogFile).await;
            return Ok(Some(ReleaseNotes {
                source: Source::ChangelogFile {
                    path: path.clone(),
                    tag: matched_tag,
                },
                markdown: md,
            }));
        }

        Ok(None)
    }

    /// Post-process a release body per options and wrap it as notes.
    fn finish_release(&self, release: &ReleaseInfo, needles: &[&str]) -> ReleaseNotes {
        let body = release.body.as_deref().unwrap_or("");
        ReleaseNotes {
            source: Source::Release {
                tag: release.tag.clone(),
                published_at: release.published_at,
            },
            markdown: self.postprocess(body, needles),
        }
    }

    fn postprocess(&self, md: &str, needles: &[&str]) -> String {
        let shifted = markdown::shift_headings(md, self.options.heading_target, needles);
        if self.options.sanitize {
            markdown::sanitize(&shifted)
        } else {
            shifted
        }
    }

    async fn remember(&self, strategy: Strategy) {
        let mut slot = self.strategy.lock().await;
        if slot.is_none() {
            *slot = Some(strategy);
        }
    }
}

/// First candidate present in `releases` whose body is a string (`Some("")`
/// is a hit — legacy rule; `None` misses).
fn match_in_list<'a>(cands: &[Candidate], releases: &'a [ReleaseInfo]) -> Option<&'a ReleaseInfo> {
    for cand in cands {
        if let Some(rel) = releases.iter().find(|r| r.tag == cand.tag) {
            if rel.body.is_some() {
                return Some(rel);
            }
        }
    }
    None
}

/// First candidate present in `releases`, regardless of body — used by
/// strategy B to attach the actually-matched tag (no body requirement).
fn find_tag_in_list<'a>(
    cands: &[Candidate],
    releases: &'a [ReleaseInfo],
) -> Option<&'a ReleaseInfo> {
    cands
        .iter()
        .find_map(|cand| releases.iter().find(|r| r.tag == cand.tag))
}

fn hit_shape(cands: &[Candidate], tag: &Tag) -> TagShape {
    cands
        .iter()
        .find(|c| c.tag == *tag)
        .map(|c| c.shape)
        .unwrap_or(TagShape::Plain)
}

/// Reorder candidates so the remembered shape (its version variant, then the
/// base-version variant) comes first; the rest keep the default order.
/// Ordering only — nothing is dropped.
fn reorder_by_strategy(cands: &[Candidate], remembered: Option<Strategy>) -> Vec<Candidate> {
    let Some(Strategy::Release(shape)) = remembered else {
        return cands.to_vec();
    };
    let mut out: Vec<Candidate> = cands.iter().filter(|c| c.shape == shape).cloned().collect();
    out.extend(cands.iter().filter(|c| c.shape != shape).cloned());
    out
}
