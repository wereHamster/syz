use anyhow::anyhow;

/// Absence of notes is a normal outcome and is `Ok(None)`, never an error.
#[derive(Debug)]
pub enum Error {
    /// The forge refused the request (HTTP 403/429 with rate-limit semantics).
    RateLimited {
        retry_after: Option<std::time::Duration>,
    },
    /// Network/transport failure, or unexpected HTTP status.
    Transport(anyhow::Error),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::RateLimited {
                retry_after: Some(d),
            } => {
                write!(
                    f,
                    "rate limited by forge (retry after {:.0}s)",
                    d.as_secs_f64()
                )
            }
            Error::RateLimited { retry_after: None } => {
                write!(f, "rate limited by forge")
            }
            Error::Transport(e) => write!(f, "transport error: {e}"),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T> = std::result::Result<T, Error>;

/// A package name as it appears in the manifest, e.g. "knip", "@sentry/core".
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct PackageName(String);

impl PackageName {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Last `/` segment; equals `as_str` when unscoped.
    pub fn short(&self) -> &str {
        self.0.rsplit('/').next().unwrap_or(&self.0)
    }

    /// `true` when the name contains `/` (e.g. npm scoped packages).
    pub fn is_scoped(&self) -> bool {
        self.0.contains('/')
    }
}

impl std::fmt::Display for PackageName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A release/git tag, e.g. "v1.2.3", "knip@6.12.2", "@sentry/core@7.114.0".
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Tag(String);

impl Tag {
    pub fn new(tag: impl Into<String>) -> Self {
        Self(tag.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Percent-encoded for use in a release URL path segment.
    pub fn url_path(&self) -> String {
        // Encode everything except `A-Za-z0-9-._~` (legacy behavior).
        const SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
            .remove(b'-')
            .remove(b'_')
            .remove(b'.')
            .remove(b'~');
        percent_encoding::utf8_percent_encode(&self.0, SET).to_string()
    }
}

impl std::fmt::Display for Tag {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A repository identity on a forge, parsed from a clone/web URL.
/// Accepts: `https://github.com/o/r`, `http://github.com/o/r`,
/// `git://github.com/o/r`, `git+https://…`, `git+ssh://…`,
/// `ssh://git@github.com/o/r`, `git@github.com:o/r`; trailing `.git` and `/`
/// are stripped.
///
/// GitHub-only for now: the host segment is dropped (forge owners cannot
/// contain dots, hostnames can) and [`Repository::web_url`] formats a
/// github.com URL. Revisit when a second forge implementation arrives.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repository {
    pub owner: String,
    pub name: String,
}

impl Repository {
    /// Parse failures are input errors, not forge errors, so this returns
    /// [`anyhow::Result`] rather than the crate [`Result`].
    pub fn parse(url: &str) -> anyhow::Result<Self> {
        let url = url.trim();

        // Scheme-agnostic split; scp-like `git@host:owner/repo` and the
        // dropped userinfo of ssh URLs are normalized to host/owner/repo.
        let path_part: String = if let Some((_, rest)) = url.split_once("://") {
            rest.to_string()
        } else if let Some(rest) = url.strip_prefix("git@") {
            rest.replace(':', "/")
        } else {
            url.to_string()
        };
        let path_part = path_part
            .rsplit_once('@')
            .map(|(_, after)| after.to_string())
            .unwrap_or(path_part);
        let path_part = match path_part.split_once(':') {
            Some((host, rest)) if !path_part.contains('/') => format!("{host}/{rest}"),
            _ => path_part,
        };

        let mut segments: Vec<&str> = path_part
            .trim_matches('/')
            .trim_end_matches(".git")
            .split('/')
            .filter(|s| !s.is_empty())
            .collect();

        // Drop the host segment (e.g. `github.com`, `gitlab.example.com`):
        // forge owners cannot contain dots, hostnames can.
        if segments.first().is_some_and(|s| s.contains('.')) {
            segments.remove(0);
        }

        if segments.len() < 2 {
            return Err(anyhow!(
                "repository URL does not contain owner and repo: {url}"
            ));
        }

        Ok(Self {
            owner: segments[0].to_string(),
            name: segments[1].to_string(),
        })
    }

    /// `https://github.com/{owner}/{name}` — web base URL for the default
    /// forge (GitHub). Other forges format their own URLs.
    pub fn web_url(&self) -> String {
        format!("https://github.com/{}/{}", self.owner, self.name)
    }
}

impl std::fmt::Display for Repository {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// One release as reported by the forge. `body` is the raw release markdown.
/// `Some(_)` — even empty — makes a tag match (legacy rule); `None`
/// (absent/null body) misses.
#[derive(Clone, Debug)]
pub struct ReleaseInfo {
    pub tag: Tag,
    pub published_at: Option<chrono::DateTime<chrono::Utc>>,
    pub body: Option<String>,
}

/// Where the notes came from. `tag` is the *actually matched* release tag
/// when one exists; `None` for changelog-file notes whose version has no
/// matching release (prevents fabricated links).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Source {
    Release {
        tag: Tag,
        published_at: Option<chrono::DateTime<chrono::Utc>>,
    },
    ChangelogFile {
        path: String,
        tag: Option<Tag>,
    },
}

impl Source {
    /// The actually matched release tag, when one exists.
    pub fn tag(&self) -> Option<&Tag> {
        match self {
            Source::Release { tag, .. } => Some(tag),
            Source::ChangelogFile { tag, .. } => tag.as_ref(),
        }
    }
}

/// The resolved notes for one package version, post-processed per `Options`.
#[derive(Clone, Debug)]
pub struct ReleaseNotes {
    pub source: Source,
    pub markdown: String,
}

impl ReleaseNotes {
    /// Link to the release page, or `None` when no real tag is known.
    /// `repo_url` is the clone/web URL recorded for the package; normalized
    /// via [`Repository::parse`] when possible, falling back to string
    /// concatenation.
    pub fn release_url(&self, repo_url: &str) -> Option<String> {
        let tag = self.source.tag()?;
        match Repository::parse(repo_url) {
            Ok(repo) => Some(format!(
                "{}/releases/tag/{}",
                repo.web_url(),
                tag.url_path()
            )),
            Err(_) => Some(format!(
                "{}/releases/tag/{}",
                repo_url.trim_end_matches('/'),
                tag.url_path()
            )),
        }
    }
}

/// The display tag used when *no* notes were resolved at all. Centralizes the
/// heuristic duplicated in the legacy resolver and PR-history section:
/// scoped package → `{pkg}@{version}`, else `v{version}`.
pub fn fallback_tag(pkg: &PackageName, version: &str) -> Tag {
    if pkg.is_scoped() {
        Tag::new(format!("{}@{}", pkg.as_str(), version))
    } else {
        Tag::new(format!("v{}", version))
    }
}

/// Version with build metadata stripped: "1.2.3" / "1.2.3+abc" → "1.2.3".
pub fn base_version(version: &str) -> &str {
    version.split_once('+').map_or(version, |(base, _)| base)
}

/// The *shape* of a candidate tag, abstracted over the concrete package
/// name and version, so that a shape which matched for one package can be
/// re-instantiated for sibling packages (strategy memory).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TagShape {
    /// `{package}@{version}` — full package name
    NameAt,
    /// `{package}-{version}` — full package name
    NameDash,
    /// `{package}-v{version}` — full package name
    NameDashV,
    /// `{short}@{version}` — last `/` segment (scoped packages)
    ShortAt,
    /// `{short}-{version}`
    ShortDash,
    /// `{short}-v{version}`
    ShortDashV,
    /// `v{version}`
    V,
    /// `{version}` as-is
    Plain,
}

/// A candidate tag together with its shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    pub tag: Tag,
    pub shape: TagShape,
}

/// Render a tag shape for a package and version.
fn render_shape(shape: TagShape, pkg: &PackageName, version: &str) -> Tag {
    let name = pkg.as_str();
    let short = pkg.short();
    let tag = match shape {
        TagShape::NameAt => format!("{name}@{version}"),
        TagShape::NameDash => format!("{name}-{version}"),
        TagShape::NameDashV => format!("{name}-v{version}"),
        TagShape::ShortAt => format!("{short}@{version}"),
        TagShape::ShortDash => format!("{short}-{version}"),
        TagShape::ShortDashV => format!("{short}-v{version}"),
        TagShape::V => format!("v{version}"),
        TagShape::Plain => version.to_string(),
    };
    Tag::new(tag)
}

/// Legacy default candidate order, per shape.
const SHAPES_UNSCOPED: [TagShape; 5] = [
    TagShape::NameAt,
    TagShape::V,
    TagShape::Plain,
    TagShape::NameDash,
    TagShape::NameDashV,
];

/// For scoped packages, the legacy probe order: full-name forms first, then
/// short-name forms, then the generic `v`/plain forms.
const SHAPES_SCOPED: [TagShape; 8] = [
    TagShape::NameAt,
    TagShape::NameDash,
    TagShape::NameDashV,
    TagShape::ShortAt,
    TagShape::ShortDash,
    TagShape::ShortDashV,
    TagShape::V,
    TagShape::Plain,
];

/// All candidate tags for a version, in exact legacy probe order:
/// every shape for `version` first, then every shape for the base version
/// (when the build metadata differs). Public so it is unit-testable and
/// reusable.
pub fn candidate_tags(pkg: &PackageName, version: &str) -> Vec<Candidate> {
    let shapes: &[TagShape] = if pkg.is_scoped() {
        &SHAPES_SCOPED
    } else {
        &SHAPES_UNSCOPED
    };

    let mut out: Vec<Candidate> = Vec::new();
    let mut push_shapes = |ver: &str| {
        for shape in shapes {
            out.push(Candidate {
                tag: render_shape(*shape, pkg, ver),
                shape: *shape,
            });
        }
    };

    push_shapes(version);
    let base = base_version(version);
    if base != version {
        push_shapes(base);
    }

    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(name: &str) -> PackageName {
        PackageName::new(name)
    }

    #[test]
    fn base_version_strips_build_metadata() {
        assert_eq!(base_version("1.2.3"), "1.2.3");
        assert_eq!(base_version("1.2.3+abc"), "1.2.3");
        assert_eq!(base_version("1.2.3+abc.2010+05"), "1.2.3");
    }

    #[test]
    fn package_name_short_and_scoped() {
        let p = pkg("@sentry/core");
        assert!(p.is_scoped());
        assert_eq!(p.short(), "core");

        let p = pkg("knip");
        assert!(!p.is_scoped());
        assert_eq!(p.short(), "knip");
    }

    #[test]
    fn candidate_tags_unscoped_order() {
        let p = pkg("knip");
        let tags: Vec<String> = candidate_tags(&p, "1.2.3")
            .into_iter()
            .map(|c| c.tag.as_str().to_string())
            .collect();
        assert_eq!(
            tags,
            vec!["knip@1.2.3", "v1.2.3", "1.2.3", "knip-1.2.3", "knip-v1.2.3",]
        );
    }

    #[test]
    fn candidate_tags_unscoped_with_build_metadata() {
        let p = pkg("knip");
        let tags: Vec<String> = candidate_tags(&p, "1.2.3+abc")
            .into_iter()
            .map(|c| c.tag.as_str().to_string())
            .collect();
        assert_eq!(
            tags,
            vec![
                "knip@1.2.3+abc",
                "v1.2.3+abc",
                "1.2.3+abc",
                "knip-1.2.3+abc",
                "knip-v1.2.3+abc",
                // base version variants
                "knip@1.2.3",
                "v1.2.3",
                "1.2.3",
                "knip-1.2.3",
                "knip-v1.2.3",
            ]
        );
    }

    #[test]
    fn candidate_tags_scoped_order() {
        let p = pkg("@sentry/core");
        let tags: Vec<String> = candidate_tags(&p, "1.0.0")
            .into_iter()
            .map(|c| c.tag.as_str().to_string())
            .collect();
        assert_eq!(
            tags,
            vec![
                "@sentry/core@1.0.0",
                "@sentry/core-1.0.0",
                "@sentry/core-v1.0.0",
                "core@1.0.0",
                "core-1.0.0",
                "core-v1.0.0",
                "v1.0.0",
                "1.0.0",
            ]
        );
    }

    #[test]
    fn candidate_tags_shape_annotation() {
        let p = pkg("knip");
        let cands = candidate_tags(&p, "1.0.0");
        assert_eq!(cands[0].shape, TagShape::NameAt);
        assert_eq!(cands[1].shape, TagShape::V);
        assert_eq!(cands[2].shape, TagShape::Plain);
    }

    #[test]
    fn fallback_tag_heuristic() {
        assert_eq!(fallback_tag(&pkg("knip"), "1.2.3"), Tag::new("v1.2.3"));
        assert_eq!(
            fallback_tag(&pkg("@sentry/core"), "1.2.3"),
            Tag::new("@sentry/core@1.2.3")
        );
    }

    #[test]
    fn tag_url_path_percent_encoding() {
        assert_eq!(Tag::new("v1.2.3").url_path(), "v1.2.3");
        assert_eq!(
            Tag::new("@sentry/core@1.0.0").url_path(),
            "%40sentry%2Fcore%401.0.0"
        );
        assert_eq!(Tag::new("a b/c").url_path(), "a%20b%2Fc");
    }

    #[test]
    fn repository_parse_schemes() {
        let r = Repository {
            owner: "webpro-nl".to_string(),
            name: "knip".to_string(),
        };
        for url in [
            "https://github.com/webpro-nl/knip",
            "http://github.com/webpro-nl/knip",
            "git://github.com/webpro-nl/knip",
            "git+https://github.com/webpro-nl/knip",
            "ssh://git@github.com/webpro-nl/knip",
            "git@github.com:webpro-nl/knip",
            "https://github.com/webpro-nl/knip.git",
            "https://github.com/webpro-nl/knip/",
        ] {
            assert_eq!(Repository::parse(url).unwrap(), r, "url: {url}");
        }
    }

    #[test]
    fn repository_parse_rejects_single_segment() {
        assert!(Repository::parse("https://github.com/webpro-nl").is_err());
    }

    #[test]
    fn repository_web_url() {
        let r = Repository::parse("git+https://github.com/webpro-nl/knip.git").unwrap();
        assert_eq!(r.web_url(), "https://github.com/webpro-nl/knip");
    }

    #[test]
    fn release_notes_release_url() {
        let notes = ReleaseNotes {
            source: Source::Release {
                tag: Tag::new("knip@6.12.2"),
                published_at: None,
            },
            markdown: "md".to_string(),
        };
        assert_eq!(
            notes
                .release_url("https://github.com/webpro-nl/knip")
                .unwrap(),
            "https://github.com/webpro-nl/knip/releases/tag/knip%406.12.2"
        );
    }

    #[test]
    fn release_notes_no_tag_no_url() {
        let notes = ReleaseNotes {
            source: Source::ChangelogFile {
                path: "CHANGELOG.md".to_string(),
                tag: None,
            },
            markdown: "md".to_string(),
        };
        assert_eq!(notes.release_url("https://github.com/o/r"), None);
    }
}
