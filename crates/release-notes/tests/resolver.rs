//! Resolver tests over `MockForge` (behind the `test-util` feature):
//! strategy precedence, empty-body hit semantics (legacy quirk), caching and
//! single-flight behavior, rate-limit propagation + error caching, source/tag
//! fidelity, probe fallback, and strategy memory (incl. `resolve_batch`).
//!
//! Run with: `cargo test -p release-notes --features test-util`
#![cfg(feature = "test-util")]

use std::sync::Arc;

use release_notes::{
    candidate_tags, MockForge, Options, PackageName, Resolver, Source, Strategy, Tag, TagShape,
};

fn rel(tag: &str, body: Option<&str>) -> release_notes::ReleaseInfo {
    release_notes::ReleaseInfo {
        tag: Tag::new(tag),
        published_at: None,
        body: body.map(String::from),
    }
}

fn pkg(name: &str) -> PackageName {
    PackageName::new(name)
}

async fn resolve_of(
    forge: MockForge,
    pkg_name: &str,
    version: &str,
) -> Result<Option<release_notes::ReleaseNotes>, release_notes::Error> {
    let resolver = Resolver::new(Arc::new(forge), Options::default());
    resolver.resolve(&pkg(pkg_name), version).await
}

// ---------------------------------------------------------------------------
// Strategy precedence and sources
// ---------------------------------------------------------------------------

#[tokio::test]
async fn release_body_wins_over_changelog_file() {
    let forge = MockForge::new()
        .with_releases(vec![rel("v1.0.0", Some("### Fixed\n\n- release body"))])
        .with_file(
            "CHANGELOG.md",
            Some("## 1.0.0\n\n### Fixed\n\n- file body\n"),
        );
    let notes = resolve_of(forge, "knip", "1.0.0").await.unwrap().unwrap();

    assert!(matches!(notes.source, Source::Release { .. }));
    assert_eq!(notes.source.tag().unwrap(), &Tag::new("v1.0.0"));
    assert!(notes.markdown.contains("release body"));
    assert!(!notes.markdown.contains("file body"));
}

#[tokio::test]
async fn changelog_file_used_when_no_release() {
    let forge = MockForge::new().with_releases(vec![]).with_file(
        "CHANGELOG.md",
        Some("## 1.0.0\n\n### Fixed\n\n- file body\n"),
    );
    let notes = resolve_of(forge, "knip", "1.0.0").await.unwrap().unwrap();

    match &notes.source {
        Source::ChangelogFile { path, tag } => {
            assert_eq!(path, "CHANGELOG.md");
            assert_eq!(tag, &None); // no releases cached → no fabricated tag
        }
        _ => panic!("expected ChangelogFile source"),
    }
    assert!(notes.markdown.contains("file body"));
    assert!(notes.markdown.starts_with("### Fixed")); // nested under ## header
    assert_eq!(
        notes.release_url("https://github.com/o/r"),
        None,
        "no matched release → no link"
    );
}

#[tokio::test]
async fn changelog_file_tag_when_list_already_cached() {
    // Release exists but with a null body (miss), so notes come from the
    // file — but the tag is known from the list.
    let forge = MockForge::new()
        .with_releases(vec![rel("v1.0.0", None)])
        .with_file(
            "CHANGELOG.md",
            Some("## 1.0.0\n\n### Fixed\n\n- file body\n"),
        );
    let notes = resolve_of(forge, "knip", "1.0.0").await.unwrap().unwrap();

    match &notes.source {
        Source::ChangelogFile { tag, .. } => {
            assert_eq!(tag, &Some(Tag::new("v1.0.0")));
        }
        _ => panic!("expected ChangelogFile source"),
    }
    assert!(notes.release_url("https://github.com/o/r").is_some());
}

#[tokio::test]
async fn changelog_first_keeps_matched_tag_for_later_versions() {
    // Releases exist but with null bodies (misses), so notes come from the
    // file. The first version learns ChangelogFile; later versions take the
    // changelog-first path — their matched tags must still be read from the
    // cached list, not dropped.
    let forge = MockForge::new()
        .with_releases(vec![rel("v1.0.0", None), rel("v2.0.0", None)])
        .with_file(
            "CHANGELOG.md",
            Some("## 1.0.0\n\n### Fixed\n\n- one\n\n## 2.0.0\n\n### Fixed\n\n- two\n"),
        );
    let resolver = Resolver::new(Arc::new(forge), Options::default());

    let requests = vec![
        release_notes::Request {
            package: pkg("knip"),
            version: "1.0.0".to_string(),
        },
        release_notes::Request {
            package: pkg("knip"),
            version: "2.0.0".to_string(),
        },
    ];
    let results = resolver.resolve_batch(&requests).await;

    let first = results[0].as_ref().unwrap().as_ref().unwrap();
    assert_eq!(first.source.tag(), Some(&Tag::new("v1.0.0")));
    let second = results[1].as_ref().unwrap().as_ref().unwrap();
    assert_eq!(second.source.tag(), Some(&Tag::new("v2.0.0")));
}

#[tokio::test]
async fn candidate_order_respected() {
    // A scoped package with both a full-name release and a plain release:
    // `NameAt` (first candidate) must win.
    let forge = MockForge::new().with_releases(vec![
        rel("1.0.0", Some("plain body")),
        rel("@sentry/core@1.0.0", Some("scoped body")),
    ]);
    let notes = resolve_of(forge, "@sentry/core", "1.0.0")
        .await
        .unwrap()
        .unwrap();

    assert_eq!(notes.source.tag().unwrap(), &Tag::new("@sentry/core@1.0.0"));
}

#[tokio::test]
async fn no_notes_at_all() {
    let forge = MockForge::new().with_releases(vec![]);
    let result = resolve_of(forge, "knip", "9.9.9").await.unwrap();
    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// Legacy empty-body quirk
// ---------------------------------------------------------------------------

#[tokio::test]
async fn empty_body_is_a_hit_with_empty_markdown() {
    let forge = MockForge::new()
        .with_releases(vec![rel("v1.0.0", Some(""))])
        .with_file("CHANGELOG.md", Some("## 1.0.0\n\n- file body\n"));
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());
    let notes = resolver
        .resolve(&pkg("knip"), "1.0.0")
        .await
        .unwrap()
        .unwrap();

    // The empty release body matches first; the changelog file is never read.
    assert!(matches!(notes.source, Source::Release { .. }));
    assert_eq!(notes.markdown, "");
    assert_eq!(forge.file_calls(), 0);
}

// ---------------------------------------------------------------------------
// Caching / single-flight
// ---------------------------------------------------------------------------

#[tokio::test]
async fn releases_list_fetched_once_across_versions() {
    let forge = MockForge::new().with_releases(vec![
        rel("v1.0.0", Some("b1")),
        rel("v1.1.0", Some("b2")),
        rel("v1.2.0", Some("b3")),
    ]);
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());

    let versions = ["1.0.0", "1.1.0", "1.2.0"];
    let requests: Vec<release_notes::Request> = versions
        .iter()
        .map(|v| release_notes::Request {
            package: pkg("knip"),
            version: v.to_string(),
        })
        .collect();
    let results = resolver.resolve_batch(&requests).await;
    assert!(results.iter().all(|r| r.is_ok()));

    // One list fetch for the whole batch; zero per-tag probes.
    assert_eq!(forge.release_pages_calls(), 1);
    assert_eq!(forge.by_tag_calls(), 0);
    assert_eq!(forge.tree_calls(), 0);
}

#[tokio::test]
async fn concurrent_resolve_single_flights_the_list() {
    let forge = MockForge::new().with_releases(vec![rel("v1.0.0", Some("b"))]);
    let resolver = Arc::new(Resolver::new(Arc::new(forge.clone()), Options::default()));

    let mut handles = Vec::new();
    for _ in 0..8 {
        let resolver = resolver.clone();
        let forge = forge.clone();
        handles.push(tokio::spawn(async move {
            let r = resolver.resolve(&pkg("knip"), "1.0.0").await;
            (r.is_ok(), forge.release_pages_calls())
        }));
    }
    let mut max_calls = 0;
    for handle in handles {
        let (ok, calls) = handle.await.unwrap();
        assert!(ok);
        max_calls = max_calls.max(calls);
    }
    // All concurrent resolvers observed the single-flight: the fetch ran once.
    assert_eq!(max_calls, 1);
}

// ---------------------------------------------------------------------------
// Probe fallback (deep history)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn probe_fallback_for_versions_older_than_pages() {
    // `v0.1.0` is older than the fetched pages: absent from the list view,
    // reachable only via by-tag probes.
    let forge = MockForge::new()
        .with_releases(vec![rel("v2.0.0", Some("new"))])
        .with_by_tag_releases(vec![rel("v0.1.0", Some("old body"))]);
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());
    let notes = resolver
        .resolve(&pkg("knip"), "0.1.0")
        .await
        .unwrap()
        .unwrap();

    assert!(matches!(notes.source, Source::Release { .. }));
    assert!(notes.markdown.contains("old body"));
    // 1 list fetch + probes for each candidate until the hit.
    assert_eq!(forge.release_pages_calls(), 1);
    assert!(forge.probed_tags().contains(&"v0.1.0".to_string()));
}

#[tokio::test]
async fn tags_seen_in_list_are_not_probed() {
    // v1.0.0 exists in the list with a *null* body: a miss. It must NOT be
    // probed again (the list already told us there is no string body);
    // alternate shapes absent from the list ARE probed (legacy parity —
    // they could exist as separate releases).
    let forge = MockForge::new()
        .with_releases(vec![rel("v1.0.0", None)])
        .with_file("CHANGELOG.md", Some("## 1.0.0\n\n- file\n"));
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());
    let notes = resolver
        .resolve(&pkg("knip"), "1.0.0")
        .await
        .unwrap()
        .unwrap();

    match &notes.source {
        Source::ChangelogFile { tag, .. } => assert_eq!(tag, &Some(Tag::new("v1.0.0"))),
        _ => panic!("expected changelog file"),
    }
    assert!(
        !forge.probed_tags().contains(&"v1.0.0".to_string()),
        "tag seen in the list must not be probed; probed: {:?}",
        forge.probed_tags()
    );
}

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rate_limit_is_an_err_not_none() {
    let forge = MockForge::new().rate_limited();
    let err = resolve_of(forge, "knip", "1.0.0").await.unwrap_err();

    assert!(matches!(err, release_notes::Error::RateLimited { .. }));
}

#[tokio::test]
async fn rate_limit_error_is_cached() {
    let forge = MockForge::new().rate_limited();
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());

    let first = resolver.resolve(&pkg("knip"), "1.0.0").await.unwrap_err();
    let second = resolver.resolve(&pkg("knip"), "2.0.0").await.unwrap_err();
    assert!(matches!(first, release_notes::Error::RateLimited { .. }));
    assert!(matches!(second, release_notes::Error::RateLimited { .. }));
    assert_eq!(forge.release_pages_calls(), 1);
}

// ---------------------------------------------------------------------------
// Options
// ---------------------------------------------------------------------------

#[tokio::test]
async fn disabled_strategies_yield_none() {
    let forge = MockForge::new()
        .with_releases(vec![rel("v1.0.0", Some("body"))])
        .with_file("CHANGELOG.md", Some("## 1.0.0\n\n- file\n"));

    let options = Options {
        prefer_release_bodies: false,
        use_changelog_file: false,
        ..Options::default()
    };
    let resolver = Resolver::new(Arc::new(forge.clone()), options);
    let result = resolver.resolve(&pkg("knip"), "1.0.0").await.unwrap();
    assert!(result.is_none());
    assert_eq!(forge.release_pages_calls(), 0);
    assert_eq!(forge.tree_calls(), 0);
}

#[tokio::test]
async fn sanitize_can_be_disabled() {
    let forge = MockForge::new().with_releases(vec![rel("v1.0.0", Some("#123 fixed by @alice"))]);
    let options = Options {
        sanitize: false,
        ..Options::default()
    };
    let resolver = Resolver::new(Arc::new(forge), options);
    let notes = resolver
        .resolve(&pkg("knip"), "1.0.0")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notes.markdown, "#123 fixed by @alice");
}

#[tokio::test]
async fn changelog_candidate_cap() {
    let forge = MockForge::new()
        .with_releases(vec![])
        .with_file("CHANGELOG.md", Some("## 1.0.0\n\n- a\n"))
        .with_file("CHANGELOG2.md", Some("## 1.0.0\n\n- b\n"))
        .with_file("CHANGELOG3.md", Some("## 1.0.0\n\n- c\n"))
        .with_file("CHANGELOG4.md", Some("## 1.0.0\n\n- d\n"));
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());
    resolver.resolve(&pkg("knip"), "1.0.0").await.unwrap();

    assert!(forge.file_calls() <= 3);
}

// ---------------------------------------------------------------------------
// Strategy memory
// ---------------------------------------------------------------------------

#[tokio::test]
async fn strategy_memory_reorders_candidates() {
    // `Plain` matches; the learned shape must be probed first next time.
    let forge = MockForge::new()
        .with_releases(vec![rel("1.0.0", Some("plain body"))])
        .with_by_tag_releases(vec![rel("2.0.0", Some("old plain body"))]);
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());

    let notes = resolver
        .resolve(&pkg("knip"), "1.0.0")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notes.source.tag().unwrap(), &Tag::new("1.0.0"));
    assert_eq!(
        resolver.learned_strategy().await,
        Some(Strategy::Release(TagShape::Plain))
    );

    // Second version misses the list (older than pages); the remembered
    // `Plain` shape is probed FIRST and hits on the first probe.
    let notes = resolver
        .resolve(&pkg("knip"), "2.0.0")
        .await
        .unwrap()
        .unwrap();
    assert!(notes.markdown.contains("old plain body"));
    assert_eq!(forge.by_tag_calls(), 1);
}

#[tokio::test]
async fn probe_misses_still_try_every_candidate() {
    // Memory reorders but never skips: a total miss probes all 5 shapes.
    let forge = MockForge::new().with_releases(vec![rel("1.0.0", Some("b"))]);
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());

    resolver
        .resolve(&pkg("knip"), "1.0.0")
        .await
        .unwrap()
        .unwrap();
    let miss = resolver.resolve(&pkg("knip"), "2.0.0").await.unwrap();
    assert!(miss.is_none());
    assert_eq!(forge.by_tag_calls(), 5);
}

#[tokio::test]
async fn strategy_memory_changelog_first() {
    let forge = MockForge::new().with_releases(vec![]).with_file(
        "CHANGELOG.md",
        Some("## 1.0.0\n\n### Fixed\n\n- one\n\n## 2.0.0\n\n### Fixed\n\n- two\n"),
    );
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());

    resolver
        .resolve(&pkg("knip"), "1.0.0")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        resolver.learned_strategy().await,
        Some(Strategy::ChangelogFile)
    );
    // The first resolve legitimately fetched the list and probed every
    // candidate (A ran before B on a fresh resolver).
    assert_eq!(forge.release_pages_calls(), 1);
    let probes_after_first = forge.by_tag_calls();
    assert!(probes_after_first > 0);

    // Second lookup: B is tried first and hits — no additional probes.
    let notes = resolver
        .resolve(&pkg("knip"), "2.0.0")
        .await
        .unwrap()
        .unwrap();
    assert!(notes.markdown.contains("two"));
    assert_eq!(forge.release_pages_calls(), 1);
    assert_eq!(forge.by_tag_calls(), probes_after_first);
}

#[tokio::test]
async fn remembered_release_shape_wins_on_list_miss_probe() {
    // First: NameAt hits via the list.
    let forge = MockForge::new().with_releases(vec![rel("knip@1.0.0", Some("b"))]);
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());
    resolver
        .resolve(&pkg("knip"), "1.0.0")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        resolver.learned_strategy().await,
        Some(Strategy::Release(TagShape::NameAt))
    );

    // Second version misses the list; the remembered NameAt shape must be
    // probed FIRST.
    resolver.resolve(&pkg("knip"), "2.0.0").await.unwrap();
    assert_eq!(
        forge.probed_tags().first().map(String::as_str),
        Some("knip@2.0.0")
    );
}

#[tokio::test]
async fn memory_changes_order_not_completeness() {
    // Remembered NameAt, but a later version only matches `Plain` — the
    // reordered candidates must still contain and find it.
    let forge = MockForge::new().with_releases(vec![
        rel("knip@1.0.0", Some("first")),
        rel("2.0.0", Some("second")),
    ]);
    let resolver = Resolver::new(Arc::new(forge), Options::default());

    let first = resolver
        .resolve(&pkg("knip"), "1.0.0")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(first.source.tag().unwrap(), &Tag::new("knip@1.0.0"));

    let second = resolver
        .resolve(&pkg("knip"), "2.0.0")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(second.source.tag().unwrap(), &Tag::new("2.0.0"));
}

#[tokio::test]
async fn resolve_batch_sequential_learning() {
    let forge = MockForge::new().with_releases(vec![
        rel("a@1.0.0", Some("body a")),
        rel("b@1.0.0", Some("body b")),
    ]);
    let resolver = Resolver::new(Arc::new(forge.clone()), Options::default());

    let requests = vec![
        release_notes::Request {
            package: pkg("a"),
            version: "1.0.0".to_string(),
        },
        release_notes::Request {
            package: pkg("b"),
            version: "1.0.0".to_string(),
        },
    ];
    let results = resolver.resolve_batch(&requests).await;

    assert_eq!(results.len(), 2);
    assert!(results.iter().all(|r| r.is_ok()));
    let first = results[0].as_ref().unwrap().as_ref().unwrap();
    let second = results[1].as_ref().unwrap().as_ref().unwrap();
    assert_eq!(first.source.tag().unwrap(), &Tag::new("a@1.0.0"));
    assert_eq!(second.source.tag().unwrap(), &Tag::new("b@1.0.0"));

    // Single list fetch shared across both requests.
    assert_eq!(forge.release_pages_calls(), 1);
}

// ---------------------------------------------------------------------------
// Misc fidelity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn build_metadata_versions_match_base_tag() {
    // Version "1.0.0+build": base-version candidates are tried after the
    // version candidates; the plain-list has `v1.0.0`.
    let forge = MockForge::new().with_releases(vec![rel("v1.0.0", Some("base body"))]);
    let notes = resolve_of(forge, "knip", "1.0.0+build")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(notes.source.tag().unwrap(), &Tag::new("v1.0.0"));
    assert!(notes.markdown.contains("base body"));
}

#[test]
fn candidates_deduplicate_no_build_metadata() {
    let cands = candidate_tags(&pkg("knip"), "1.2.3");
    assert_eq!(cands.len(), 5);
}
