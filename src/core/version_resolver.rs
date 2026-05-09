use chrono::{DateTime, Duration, Utc};
use semver::{Version, VersionReq};

pub struct AvailableRelease {
    pub version: Version,
    pub published_at: DateTime<Utc>,
}

pub struct BoundedUpdateOption {
    pub target: Option<Version>,
    pub head: Option<Version>,
}

pub struct VersionData {
    pub minor: BoundedUpdateOption,
    pub major: BoundedUpdateOption,
}

pub struct MatureResolution {
    pub resolved: Vec<Version>,
    pub blocked: Vec<(Version, DateTime<Utc>)>,
}

/// Resolves the best available minor and major update targets.
///
/// `releases` must ONLY contain valid candidates. The caller is responsible for
/// stripping out pre-releases, bounding against `latest` tags, and handling
/// ecosystem specific hacks (like `@types/node` filtering).
pub fn resolve_updates(
    current_ver: &Version,
    releases: &[AvailableRelease],
    minimum_release_age: Option<Duration>,
) -> VersionData {
    let now = Utc::now();
    let min_age = minimum_release_age.unwrap_or(Duration::zero());

    let mut target_minor: Option<Version> = None;
    let mut head_minor: Option<Version> = None;

    let mut target_major: Option<Version> = None;
    let mut head_major: Option<Version> = None;

    for release in releases {
        if release.version <= *current_ver {
            continue;
        }

        let is_mature = min_age.is_zero() || (now - release.published_at) >= min_age;

        if release.version.major == current_ver.major {
            // Track highest possible head
            if head_minor.as_ref().map_or(true, |h| release.version > *h) {
                head_minor = Some(release.version.clone());
            }

            if is_mature {
                if target_minor.as_ref().map_or(true, |t| release.version > *t) {
                    target_minor = Some(release.version.clone());
                }
            }
        } else if release.version.major > current_ver.major {
            // Track highest possible head
            if head_major.as_ref().map_or(true, |h| release.version > *h) {
                head_major = Some(release.version.clone());
            }

            if is_mature {
                if target_major.as_ref().map_or(true, |t| release.version > *t) {
                    target_major = Some(release.version.clone());
                }
            }
        }
    }

    VersionData {
        minor: BoundedUpdateOption {
            target: target_minor,
            head: head_minor,
        },
        major: BoundedUpdateOption {
            target: target_major,
            head: head_major,
        },
    }
}

pub fn resolve_mature_versions(
    vulnerable_constraints: &[VersionReq],
    releases: &[AvailableRelease],
    minimum_release_age: Option<Duration>,
) -> MatureResolution {
    let now = Utc::now();
    let min_age = minimum_release_age.unwrap_or(Duration::zero());

    let mut best_matches: std::collections::HashMap<(u64, u64), Version> =
        std::collections::HashMap::new();
    let mut newest_blocked: std::collections::HashMap<(u64, u64), (Version, DateTime<Utc>)> =
        std::collections::HashMap::new();

    for release in releases {
        let is_vulnerable = vulnerable_constraints
            .iter()
            .any(|req| req.matches(&release.version));
        if is_vulnerable {
            continue;
        }

        let is_mature = min_age.is_zero() || (now - release.published_at) >= min_age;

        let key = if release.version.major == 0 {
            (0, release.version.minor)
        } else {
            (release.version.major, 0)
        };

        if is_mature {
            if best_matches
                .get(&key)
                .map_or(true, |best| release.version > *best)
            {
                best_matches.insert(key, release.version.clone());
            }
        } else {
            if newest_blocked
                .get(&key)
                .map_or(true, |(blocked, _)| release.version > *blocked)
            {
                newest_blocked.insert(key, (release.version.clone(), release.published_at));
            }
        }
    }

    let mut resolved: Vec<Version> = best_matches.into_values().collect();
    resolved.sort();

    let mut blocked: Vec<(Version, DateTime<Utc>)> = newest_blocked.into_values().collect();
    blocked.sort_by(|a, b| a.0.cmp(&b.0));

    MatureResolution { resolved, blocked }
}
