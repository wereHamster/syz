//! The [`Resolver`]: strategy resolution with instance-scoped memoization.
//!
//! Memoization state lives inside the [`Resolver`], guarded by one lock:
//! [`Resolver::resolve`] takes the lock, does the work (fetching via the
//! forge on misses), updates the state and returns — callers never touch
//! shared state; the resolver instance *is* the shared state, and its
//! lifetime bounds the memoization (instantiate one per scope). Consecutive
//! calls share the fetched releases list, the changelog tree (per
//! package), changelog-file contents and per-tag probes, so resolving a
//! package's N versions costs one list fetch and one tree fetch — not N.
//! Errors are memoized too (short-lived burst protection). Calls serialize
//! on the state lock: a long in-flight lookup delays concurrent callers of
//! the same resolver. The state is repository-scoped by construction — the
//! forge, and with it the repository, is fixed at `Resolver::new`, so state
//! cannot leak across repositories.

use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::forge::Forge;
use crate::markdown;
use crate::types::{
    base_version, candidate_tags, Candidate, Error, PackageName, ReleaseInfo, ReleaseNotes, Result,
    Source, Tag, TagShape,
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

/// The strategy that produced notes, remembered across a batch's lookups
/// (sticky first success; see module docs).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Strategy {
    /// Release-body lookup; the tag *shape* that matched.
    Release(TagShape),
    /// Changelog-file lookup.
    ChangelogFile,
}

/// Resolves release notes for package versions against one repository.
/// Owns the memoization state: the instance is the shared state, and its
/// lifetime bounds the caching (see module docs).
pub struct Resolver {
    forge: Arc<dyn Forge>,
    options: Options,
    /// Instance-scoped memoization. The lock is held across the work
    /// (fetches included), so calls on one resolver serialize.
    state: Mutex<BatchState>,
}

impl Resolver {
    pub fn new(forge: Arc<dyn Forge>, options: Options) -> Self {
        Self {
            forge,
            options,
            state: Mutex::new(BatchState::default()),
        }
    }

    /// Resolve notes for one version, memoizing into the instance's state.
    /// Consecutive calls on the same resolver share the fetched resources,
    /// the sticky strategy and memoized errors; a fresh resolver starts
    /// empty. `Ok(None)` = no notes found. `Err` only for real failures
    /// (rate limit, transport).
    pub async fn resolve(&self, pkg: &PackageName, version: &str) -> Result<Option<ReleaseNotes>> {
        let mut state = self.state.lock().await;
        self.resolve_locked(pkg, version, &mut state).await
    }

    /// The sticky first-success strategy learned so far, if any. Reads the
    /// state lock; empty until a lookup in the instance's scope succeeded.
    pub async fn learned_strategy(&self) -> Option<Strategy> {
        self.state.lock().await.strategy
    }

    /// Multi-package entry point (group PRs): several packages of the same
    /// repository, resolved **sequentially in request order** so that
    /// strategy learning is deterministic. Returns one result per request,
    /// aligned by index. One lock acquisition for the whole list: every
    /// request after the first reuses the instance's releases list,
    /// changelog tree, file contents and learned strategy.
    pub async fn resolve_batch(&self, requests: &[Request]) -> Vec<Result<Option<ReleaseNotes>>> {
        let mut state = self.state.lock().await;
        let mut results = Vec::with_capacity(requests.len());
        for request in requests {
            results.push(
                self.resolve_locked(&request.package, &request.version, &mut state)
                    .await,
            );
        }
        results
    }

    async fn resolve_locked(
        &self,
        pkg: &PackageName,
        version: &str,
        state: &mut BatchState,
    ) -> Result<Option<ReleaseNotes>> {
        let base = base_version(version);
        let mut needles: Vec<&str> = vec![version];
        if base != version {
            needles.push(base);
        }

        let remembered = state.strategy;
        let cands = reorder_by_strategy(&candidate_tags(pkg, version), remembered);

        let try_releases = self.options.prefer_release_bodies;
        let try_changelog = self.options.use_changelog_file;

        // A remembered changelog-file success means release-body probing
        // was fruitless for this repository — try B first so later versions
        // skip A's probe phase entirely (order only, nothing is skipped).
        let changelog_first = remembered == Some(Strategy::ChangelogFile);
        if changelog_first && try_changelog {
            // ChangelogFile can only be learned after A ran the releases
            // list (with `prefer_release_bodies`), so reading the list here
            // is a memoized hit. It keeps the matched tag attached for
            // versions whose release body is absent — the list is how
            // `matched_tag` is found.
            let list = if try_releases {
                Some(
                    state
                        .releases(|| self.forge.releases(self.options.release_pages))
                        .await?,
                )
            } else {
                None
            };
            if let Some(notes) = self
                .resolve_via_changelog_file(pkg, version, &cands, &needles, list.as_deref(), state)
                .await?
            {
                return Ok(Some(notes));
            }
        }

        let mut list: Option<Vec<ReleaseInfo>> = None;
        if try_releases {
            let releases = state
                .releases(|| self.forge.releases(self.options.release_pages))
                .await?;
            if let Some(notes) = self
                .resolve_via_releases(&cands, &needles, releases.as_slice(), state)
                .await?
            {
                return Ok(Some(notes));
            }
            list = Some(releases);
        }

        if try_changelog && !(changelog_first) {
            if let Some(notes) = self
                .resolve_via_changelog_file(pkg, version, &cands, &needles, list.as_deref(), state)
                .await?
            {
                return Ok(Some(notes));
            }
        }

        Ok(None)
    }

    /// Strategy A: match candidates against the (memoized) releases
    /// list, then (only for versions not covered by the fetched pages) fall
    /// back to per-tag probes — legacy deep-history parity.
    async fn resolve_via_releases(
        &self,
        cands: &[Candidate],
        needles: &[&str],
        releases: &[ReleaseInfo],
        state: &mut BatchState,
    ) -> Result<Option<ReleaseNotes>> {
        if let Some((hit, shape)) = match_in_list(cands, releases, true) {
            let notes = self.finish_release(hit, needles);
            state.remember(Strategy::Release(shape));
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
            let probe = state.probe(&tag, || self.forge.release_by_tag(&tag)).await?;
            if let Some(release) = probe.filter(|r| r.body.is_some()) {
                let notes = self.finish_release(&release, needles);
                state.remember(Strategy::Release(cand.shape));
                return Ok(Some(notes));
            }
        }

        Ok(None)
    }

    /// Strategy B: extract the version's section from ranked changelog
    /// files, fetching them lazily (memoized per path).
    async fn resolve_via_changelog_file(
        &self,
        pkg: &PackageName,
        version: &str,
        cands: &[Candidate],
        needles: &[&str],
        releases: Option<&[ReleaseInfo]>,
        state: &mut BatchState,
    ) -> Result<Option<ReleaseNotes>> {
        let paths = state
            .candidates(pkg, || self.forge.changelog_candidates(pkg))
            .await?;

        // A matched release tag is only known if the list was already
        // loaded — never fetched for it. Any release matching a candidate
        // tag counts (body irrelevant); this prevents fabricated links.
        let matched_tag = releases
            .and_then(|releases| match_in_list(cands, releases, false))
            .map(|(release, _)| release.tag.clone());

        for path in paths.iter().take(self.options.max_changelog_candidates) {
            let Some(content) = state.file(path, || self.forge.read_file(path)).await? else {
                continue;
            };
            let Some(section) = markdown::extract_section(&content, version) else {
                continue;
            };

            let md = self.postprocess(&section, needles);
            state.remember(Strategy::ChangelogFile);
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
}

/// A memoized error: cheap to clone (`anyhow::Error` itself is not) and
/// reconstructible into a fresh [`Error`] per read. Transport errors
/// round-trip through their message.
#[derive(Clone, Debug)]
enum MemoErr {
    RateLimited(Option<std::time::Duration>),
    Transport(String),
}

impl From<&Error> for MemoErr {
    fn from(e: &Error) -> Self {
        match e {
            Error::RateLimited { retry_after } => MemoErr::RateLimited(*retry_after),
            Error::Transport(err) => MemoErr::Transport(err.to_string()),
        }
    }
}

impl From<MemoErr> for Error {
    fn from(e: MemoErr) -> Self {
        match e {
            MemoErr::RateLimited(retry_after) => Error::RateLimited { retry_after },
            MemoErr::Transport(msg) => Error::Transport(anyhow::anyhow!(msg)),
        }
    }
}

type Memo<T> = std::result::Result<T, MemoErr>;

fn memo<T>(r: Result<T>) -> Memo<T> {
    match r {
        Ok(v) => Ok(v),
        Err(e) => Err(MemoErr::from(&e)),
    }
}

/// Read a memoized slot: clone the value or reconstruct the error.
fn unmemo<T: Clone>(slot: &Memo<T>) -> Result<T> {
    match slot {
        Ok(v) => Ok(v.clone()),
        Err(e) => Err(e.clone().into()),
    }
}

/// Instance-scoped memoization, guarded by the resolver's state lock.
/// Repository-scoped by construction: keys are repo-relative (changelog
/// paths, tags, package names) and the forge — hence the repository — is
/// fixed at `Resolver::new`.
#[derive(Default)]
struct BatchState {
    /// Releases list: fetched at most once per state (single slot, not a map).
    releases: Option<Memo<Vec<ReleaseInfo>>>,
    /// Changelog candidate paths per package: the tree fetch is repo-level,
    /// the ranking is per package.
    candidates: HashMap<String, Memo<Vec<String>>>,
    /// Per-tag deep-history probes.
    probes: HashMap<Tag, Memo<Option<ReleaseInfo>>>,
    /// Changelog-file contents, fetched lazily per path.
    files: HashMap<String, Memo<Option<String>>>,
    /// Sticky first-success strategy (first-wins within the batch).
    strategy: Option<Strategy>,
}

impl BatchState {
    fn remember(&mut self, strategy: Strategy) {
        if self.strategy.is_none() {
            self.strategy = Some(strategy);
        }
    }

    /// Releases list, fetched via `fetch` on first access.
    async fn releases(
        &mut self,
        fetch: impl AsyncFnOnce() -> Result<Vec<ReleaseInfo>>,
    ) -> Result<Vec<ReleaseInfo>> {
        if self.releases.is_none() {
            self.releases = Some(memo(fetch().await));
        }
        unmemo(self.releases.as_ref().expect("filled above"))
    }

    /// Changelog candidate paths for one package, fetched via `fetch` on
    /// first access.
    async fn candidates(
        &mut self,
        pkg: &PackageName,
        fetch: impl AsyncFnOnce() -> Result<Vec<String>>,
    ) -> Result<Vec<String>> {
        let key = pkg.as_str().to_string();
        if !self.candidates.contains_key(&key) {
            let fetched = memo(fetch().await);
            self.candidates.insert(key.clone(), fetched);
        }
        unmemo(&self.candidates[&key])
    }

    /// Per-tag probe, fetched via `fetch` on first access.
    async fn probe(
        &mut self,
        tag: &Tag,
        fetch: impl AsyncFnOnce() -> Result<Option<ReleaseInfo>>,
    ) -> Result<Option<ReleaseInfo>> {
        if !self.probes.contains_key(tag) {
            let fetched = memo(fetch().await);
            self.probes.insert(tag.clone(), fetched);
        }
        unmemo(&self.probes[tag])
    }

    /// Changelog-file content for one path, fetched via `fetch` on first
    /// access. Keyed per path: sibling packages of one repository rank
    /// their own candidate paths, so entries accumulate independently.
    async fn file(
        &mut self,
        path: &str,
        fetch: impl AsyncFnOnce() -> Result<Option<String>>,
    ) -> Result<Option<String>> {
        if !self.files.contains_key(path) {
            let fetched = memo(fetch().await);
            self.files.insert(path.to_string(), fetched);
        }
        unmemo(&self.files[path])
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
