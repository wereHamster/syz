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
            if head_minor.as_ref().is_none_or(|h| release.version > *h) {
                head_minor = Some(release.version.clone());
            }

            if is_mature && target_minor.as_ref().is_none_or(|t| release.version > *t) {
                target_minor = Some(release.version.clone());
            }
        } else if release.version.major > current_ver.major {
            // Track highest possible head
            if head_major.as_ref().is_none_or(|h| release.version > *h) {
                head_major = Some(release.version.clone());
            }

            if is_mature && target_major.as_ref().is_none_or(|t| release.version > *t) {
                target_major = Some(release.version.clone());
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

/// The release line a version belongs to: `(major, 0)` for `>=1.0.0`, `(0, minor)` for `0.x`.
fn release_line(version: &Version) -> (u64, u64) {
    if version.major == 0 {
        (0, version.minor)
    } else {
        (version.major, 0)
    }
}

/// Finds, per release line, the newest non-vulnerable release that satisfies the age policy
/// (`resolved`) and the newest one that does not (`blocked`).
///
/// Only lines that contain at least one of the `installed` versions are considered: a fix on
/// any other line would be a downgrade or an unrequested major bump.
pub fn resolve_mature_versions(
    vulnerable_constraints: &[VersionReq],
    installed: &[Version],
    releases: &[AvailableRelease],
    minimum_release_age: Option<Duration>,
) -> MatureResolution {
    let now = Utc::now();
    let min_age = minimum_release_age.unwrap_or(Duration::zero());

    let installed_lines: std::collections::HashSet<(u64, u64)> =
        installed.iter().map(release_line).collect();

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

        let key = release_line(&release.version);
        if !installed_lines.contains(&key) {
            continue;
        }

        let is_mature = min_age.is_zero() || (now - release.published_at) >= min_age;

        if is_mature {
            if best_matches
                .get(&key)
                .is_none_or(|best| release.version > *best)
            {
                best_matches.insert(key, release.version.clone());
            }
        } else if newest_blocked
            .get(&key)
            .is_none_or(|(blocked, _)| release.version > *blocked)
        {
            newest_blocked.insert(key, (release.version.clone(), release.published_at));
        }
    }

    let mut resolved: Vec<Version> = best_matches.into_values().collect();
    resolved.sort();

    let mut blocked: Vec<(Version, DateTime<Utc>)> = newest_blocked.into_values().collect();
    blocked.sort_by(|a, b| a.0.cmp(&b.0));

    MatureResolution { resolved, blocked }
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::{Version, VersionReq};

    fn make_release(ver: &str, days_ago: i64) -> AvailableRelease {
        AvailableRelease {
            version: Version::parse(ver).unwrap(),
            published_at: Utc::now() - Duration::try_days(days_ago).unwrap(),
        }
    }

    #[test]
    fn test_resolve_updates_basic() {
        let current = Version::parse("1.2.0").unwrap();
        let releases = vec![
            make_release("1.2.1", 10),
            make_release("1.3.0", 10),
            make_release("2.0.0", 10),
        ];

        let result = resolve_updates(&current, &releases, None);
        assert_eq!(result.minor.target.unwrap().to_string(), "1.3.0");
        assert_eq!(result.minor.head.unwrap().to_string(), "1.3.0");
        assert_eq!(result.major.target.unwrap().to_string(), "2.0.0");
        assert_eq!(result.major.head.unwrap().to_string(), "2.0.0");
    }

    #[test]
    fn test_resolve_updates_immature() {
        let current = Version::parse("1.2.0").unwrap();
        let releases = vec![
            make_release("1.3.0", 10), // Mature
            make_release("1.4.0", 2),  // Immature
            make_release("2.0.0", 2),  // Immature
        ];

        let result = resolve_updates(&current, &releases, Some(Duration::try_days(7).unwrap()));
        // Minor target should be 1.3.0 because 1.4.0 is too new
        assert_eq!(result.minor.target.unwrap().to_string(), "1.3.0");
        // Minor head should still track 1.4.0
        assert_eq!(result.minor.head.unwrap().to_string(), "1.4.0");

        // Major target should be None
        assert!(result.major.target.is_none());
        // Major head should be 2.0.0
        assert_eq!(result.major.head.unwrap().to_string(), "2.0.0");
    }

    #[test]
    fn test_resolve_mature_versions() {
        let reqs = vec![VersionReq::parse("< 1.2.2").unwrap()];

        let releases = vec![
            make_release("1.2.0", 10), // Vulnerable
            make_release("1.2.1", 10), // Vulnerable
            make_release("1.2.2", 10), // Safe & Mature
            make_release("1.3.0", 2),  // Safe & Immature
            make_release("2.0.0", 10), // Safe & Mature (different major)
        ];

        let installed = vec![
            Version::parse("1.2.0").unwrap(),
            Version::parse("2.0.0").unwrap(),
        ];
        let result = resolve_mature_versions(
            &reqs,
            &installed,
            &releases,
            Some(Duration::try_days(7).unwrap()),
        );

        assert_eq!(result.resolved.len(), 2);
        assert_eq!(result.resolved[0].to_string(), "1.2.2");
        assert_eq!(result.resolved[1].to_string(), "2.0.0");

        assert_eq!(result.blocked.len(), 1);
        assert_eq!(result.blocked[0].0.to_string(), "1.3.0");
    }

    #[test]
    fn test_resolve_mature_versions_ignores_lines_not_installed() {
        // Mirrors next@16.3.6: the only fix on the 16 line is immature. The immature fix on the
        // 15 line would be a downgrade and must not be reported.
        let reqs = vec![
            VersionReq::parse(">=15.0.0, <15.5.27").unwrap(),
            VersionReq::parse(">=16.0.0, <16.4.0").unwrap(),
        ];

        let releases = vec![
            make_release("15.5.26", 30), // Vulnerable
            make_release("15.5.27", 2),  // Safe & Immature, but on a line that isn't installed
            make_release("16.3.6", 30),  // Vulnerable
            make_release("16.4.0", 2),   // Safe & Immature
        ];

        let installed = vec![Version::parse("16.3.6").unwrap()];
        let result = resolve_mature_versions(
            &reqs,
            &installed,
            &releases,
            Some(Duration::try_days(7).unwrap()),
        );

        assert!(result.resolved.is_empty());
        assert_eq!(result.blocked.len(), 1);
        assert_eq!(result.blocked[0].0.to_string(), "16.4.0");
    }

    #[test]
    fn test_resolve_mature_versions_zero_major() {
        let reqs = vec![VersionReq::parse("< 0.2.2").unwrap()];

        let releases = vec![
            make_release("0.1.0", 10), // Vulnerable
            make_release("0.2.1", 10), // Vulnerable
            make_release("0.2.2", 10), // Safe & Mature
            make_release("0.2.3", 2),  // Safe & Immature
            make_release("0.3.0", 10), // Safe & Mature (different minor for 0.x)
        ];

        let installed = vec![
            Version::parse("0.2.1").unwrap(),
            Version::parse("0.3.0").unwrap(),
        ];
        let result = resolve_mature_versions(
            &reqs,
            &installed,
            &releases,
            Some(Duration::try_days(7).unwrap()),
        );

        // For 0.x, minor versions are treated like major versions.
        assert_eq!(result.resolved.len(), 2);
        assert_eq!(result.resolved[0].to_string(), "0.2.2");
        assert_eq!(result.resolved[1].to_string(), "0.3.0");

        assert_eq!(result.blocked.len(), 1);
        assert_eq!(result.blocked[0].0.to_string(), "0.2.3");
    }
}
