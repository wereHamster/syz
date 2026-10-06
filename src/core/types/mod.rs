use serde::{Deserialize, Serialize};

pub use packageurl::PURL;

/// Represents a dependency discovered during a repository scan.
///
/// This struct only contains information that was extracted from the
/// repository. It does not contain data or metadata for which the
/// dependency ecosystem registry need to be queried.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DiscoveredDependency {
    /// The dependency as defined by its Package-URL.
    pub purl: PURL,

    /// The version requirement specified in the manifest.
    ///
    /// Examples: "^1.0.0", "workspace:*", "~2.0"
    pub requirement: String,

    /// The minimum required age for a new release of this dependency
    /// to be considered a valid update target.
    pub minimum_release_age: Option<chrono::Duration>,
}
