//! The [`Resolver`]: strategy resolution, single-flight caches, and sticky
//! strategy memory shared across `resolve` and `resolve_batch`.

use std::sync::Arc;

use tokio::sync::Mutex;

use crate::cache::Cache;
use crate::forge::Forge;
use crate::markdown;
use crate::types::{
    base_version, candidate_tags, Candidate, PackageName, ReleaseInfo, ReleaseNotes, Result,
    Source, TagShape,
};

/// One multi-package lookup request (group PRs).
#[derive(Clone, Debug)]
pub struct Request {
    pub package: PackageName,
    pub version: String,
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
    /// Pages of releases fetched per list call (page size is the forge's).
    /// Legacy parity: 3.
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

/// The strategy that produced notes, remembered across lookups on one
/// resolver (sticky first success; see module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Release-body lookup; the tag *shape* that matched.
    Release(TagShape),
    /// Changelog-file lookup.
    ChangelogFile,
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
            cache: Arc::new(Cache::default()),
            strategy: Mutex::new(None),
        }
    }

    /// The strategy learned so far, if any (sticky first success).
    pub async fn learned_strategy(&self) -> Option<Strategy> {
        *self.strategy.lock().await
    }

    /// Resolve notes for one version. `Ok(None)` = no notes found.
    /// `Err` only for real failures (rate limit, transport).
    /// Reads and writes the learned strategy.
    pub async fn resolve(&self, pkg: &PackageName, version: &str) -> Result<Option<ReleaseNotes>> {
        self.resolve_one(pkg, version).await
    }

    /// Multi-package entry point (group PRs): several packages of the same
    /// repository, resolved **sequentially in request order** so that
    /// strategy learning is deterministic. Returns one result per request,
    /// aligned by index.
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
        let cands = reorder_by_strategy(&candidate_tags(pkg, version), remembered);

        let try_releases = self.options.prefer_release_bodies;
        let try_changelog = self.options.use_changelog_file;

        // A remembered changelog-file success means release-body probing
        // was fruitless for this repository — try B first so later versions
        // skip A's probe phase entirely (order only, nothing is skipped).
        let changelog_first = remembered == Some(Strategy::ChangelogFile);
        if changelog_first && try_changelog {
            if let Some(notes) = self
                .resolve_via_changelog_file(pkg, version, &cands, &needles, None)
                .await?
            {
                return Ok(Some(notes));
            }
        }

        let mut list: Option<Vec<ReleaseInfo>> = None;
        if try_releases {
            let releases = self.cached_releases().await?;
            if let Some(notes) = self
                .resolve_via_releases(&cands, &needles, releases.as_slice())
                .await?
            {
                return Ok(Some(notes));
            }
            list = Some(releases);
        }

        if try_changelog && !(changelog_first) {
            if let Some(notes) = self
                .resolve_via_changelog_file(pkg, version, &cands, &needles, list.as_deref())
                .await?
            {
                return Ok(Some(notes));
            }
        }

        Ok(None)
    }

    /// Cached releases list, fetched at most once per resolver lifetime.
    async fn cached_releases(&self) -> Result<Vec<ReleaseInfo>> {
        let pages = self.options.release_pages;
        self.cache.releases(|| self.forge.releases(pages)).await
    }

    /// Strategy A: match candidates against the cached releases list, then
    /// (only for versions not covered by the fetched pages) fall back to
    /// per-tag probes — legacy deep-history parity.
    async fn resolve_via_releases(
        &self,
        cands: &[Candidate],
        needles: &[&str],
        releases: &[ReleaseInfo],
    ) -> Result<Option<ReleaseNotes>> {
        if let Some((hit, shape)) = match_in_list(cands, releases, true) {
            let notes = self.finish_release(hit, needles);
            self.remember(Strategy::Release(shape)).await;
            return Ok(Some(notes));
        }

        // Probe phase: releases older than the fetched pages. Tags already
        // seen in the list are conclusively known (present with a null body
        // or absent with a body) and are not probed again.
        let list_tags: std::collections::HashSet<&str> =
            releases.iter().map(|r| r.tag.as_str()).collect();
        for cand in cands {
            if list_tags.contains(cand.tag.as_str()) {
                continue;
            }
            let tag = cand.tag.clone();
            let probe = self
                .cache
                .probe(&tag, || self.forge.release_by_tag(&tag))
                .await?;
            if let Some(release) = probe.filter(|r| r.body.is_some()) {
                let notes = self.finish_release(&release, needles);
                self.remember(Strategy::Release(cand.shape)).await;
                return Ok(Some(notes));
            }
        }

        Ok(None)
    }

    /// Strategy B: extract the version's section from ranked changelog
    /// files, fetching them lazily (cached per path).
    async fn resolve_via_changelog_file(
        &self,
        pkg: &PackageName,
        version: &str,
        cands: &[Candidate],
        needles: &[&str],
        releases: Option<&[ReleaseInfo]>,
    ) -> Result<Option<ReleaseNotes>> {
        let paths = self.forge.changelog_candidates(pkg).await?;

        // A matched release tag is only known if the list was already
        // loaded — never fetched for it. Any release matching a candidate
        // tag counts (body irrelevant); this prevents fabricated links.
        let matched_tag = releases
            .and_then(|releases| match_in_list(cands, releases, false))
            .map(|(release, _)| release.tag.clone());

        for path in paths.iter().take(self.options.max_changelog_candidates) {
            let Some(content) = self.cached_file(path).await? else {
                continue;
            };
            let Some(section) = markdown::extract_section(&content, version) else {
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

    async fn cached_file(&self, path: &str) -> Result<Option<String>> {
        let path = path.to_string();
        self.cache.file(&path, || self.forge.read_file(&path)).await
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

/// First candidate present in `releases` (its shape returned alongside).
/// With `require_body`, only releases whose body is a string count
/// (`Some("")` is a hit — legacy rule; `None` misses). Without it, any
/// release matching a candidate tag counts (used to attach real tags to
/// changelog-file sources).
fn match_in_list<'a>(
    cands: &[Candidate],
    releases: &'a [ReleaseInfo],
    require_body: bool,
) -> Option<(&'a ReleaseInfo, TagShape)> {
    cands.iter().find_map(|cand| {
        releases
            .iter()
            .find(|r| r.tag == cand.tag)
            .filter(|r| !require_body || r.body.is_some())
            .map(|r| (r, cand.shape))
    })
}

/// Reorder candidates so the remembered shape (its version variant, then the
/// base-version variant) comes first; the rest keep the default order.
/// Ordering only — nothing is dropped.
fn reorder_by_strategy(cands: &[Candidate], remembered: Option<Strategy>) -> Vec<Candidate> {
    let Some(Strategy::Release(shape)) = remembered else {
        return cands.to_vec();
    };
    let mut out = cands.to_vec();
    out.sort_by_key(|c| c.shape != shape);
    out
}
