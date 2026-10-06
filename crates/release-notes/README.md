# release-notes

A small, self-contained crate for resolving release notes (changelog fragments
and release bodies) for package versions. It is not published to crates.io, but
is written as a standalone library: it knows nothing about `syz`'s database,
scheduler, or HTTP server, and consumers bring their own HTTP transport and
credentials.

## What it does

Given a package name, a version, and the repository the package is published
from, the [`Resolver`] finds the release notes for that version and returns
them as post-processed markdown, ready to embed in a GitHub PR body.

Two sources are tried, in order:

1. **Release bodies** — candidate git tags (`knip@1.2.3`, `v1.2.3`, …) are
   matched against the forge's release list, with a per-tag probe fallback for
   versions older than the fetched pages.
2. **Changelog files** — the version's section is extracted from ranked
   changelog files (`CHANGELOG.md`, `packages/foo/CHANGELOG.md`, …) in the
   repository's default branch.

Resolved notes are post-processed: headings are re-leveled to a target depth,
version headings stripped, and the markdown sanitized for embedding.

## Design points

- **Forge-agnostic.** The [`Forge`] trait abstracts releases, tags, and file
  contents. GitHub (`github::GitHubForge`) is the first implementation.
- **Transport-injected.** The crate never builds an HTTP client and never sees
  credentials. Consumers implement the [`HttpClient`] trait over their own
  stack (auth, installation tokens, retries are all their concern).
- **`Ok(None)` is a normal outcome.** No notes found is not an error. `Err` is
  reserved for real failures: rate limiting (with `retry_after`) and transport
  errors.
- **Single-flight caching.** Concurrent lookups share one fetch per resource;
  errors are cached briefly for burst protection. Resolver clones share the
  cache.
- **Sticky strategy memory.** Once a source/tag shape succeeds for a
  repository, later lookups try it first, skipping pointless probes.

## Testing

Unit tests cover tag-shape enumeration and markdown processing; `mock::MockForge`
(feature `test-util`) provides a scriptable in-memory forge for downstream
tests. Integration tests that hit live GitHub are gated behind the
`integration-tests` feature.

[`Resolver`]: src/resolver.rs
[`Forge`]: src/forge.rs
[`HttpClient`]: src/http.rs
