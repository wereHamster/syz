pub mod ecosystems;
pub mod repository;

/// Pacakge URL
///
/// See https://packageurl.org/
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PURL {
    /// Called 'type' in Package-URL specification, but that's a reserved word
    /// so we have to use a different name.
    ///
    /// Examples: "npm", "cargo"
    pub ecosystem: String,

    /// The namespace of the package. Not all ecosystems have a concept of
    /// namespaces.
    ///
    ///  - NPM: scope (eg. "@babel")
    ///  - GitHub: username (eg. "wereHamster")
    pub namespace: Option<String>,

    /// The name of the dependency.
    pub name: String,

    /// An optional subpath within the dependency repository/package.
    pub subpath: Option<String>,

    /// The version of the package.
    pub version: Option<String>,
}

/// Represents a dependency discovered during a repository scan.
///
/// This struct only contains information that was extracted from the
/// repository. It does not contain data or metadata for which the
/// dependency ecosystem registry need to be queried.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
