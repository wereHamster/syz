pub mod ecosystems;
pub mod groups;
pub mod pull_request_generator;
pub mod repository;

/// Pacakge URL
///
/// See https://packageurl.org/
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

impl PURL {
    /// Returns the canonical package name within its ecosystem.
    /// For example, in NPM this would be "@babel/core", and in GitHub Actions "actions/checkout".
    pub fn package_name(&self) -> String {
        let mut full_name = match (&self.namespace, &self.ecosystem[..]) {
            (Some(ns), "npm") | (Some(ns), "github-actions") => {
                format!("{}/{}", ns, self.name)
            }
            _ => self.name.clone(),
        };
        if let Some(subpath) = &self.subpath {
            full_name = format!("{}/{}", full_name, subpath);
        }
        full_name
    }
}

impl std::fmt::Display for PURL {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "pkg:{}", self.ecosystem)?;
        if let Some(namespace) = &self.namespace {
            write!(f, "/{}", namespace)?;
        }
        write!(f, "/{}", self.name)?;
        if let Some(version) = &self.version {
            write!(f, "@{}", version)?;
        }
        if let Some(subpath) = &self.subpath {
            write!(f, "#{}", subpath)?;
        }
        Ok(())
    }
}

/// Represents a dependency discovered during a repository scan.
///
/// This struct only contains information that was extracted from the
/// repository. It does not contain data or metadata for which the
/// dependency ecosystem registry need to be queried.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
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

/// A pair of version (the exact version as known by the ecosystem pacakge registry)
/// as well as a requirement string that can be put into the manifest.
///
/// For example, NPM can have something like "^24.0.0" as requirement, and "24.10.1"
/// as version.
#[derive(Clone, Debug, PartialEq)]
pub struct RequirementVersion {
    pub requirement: String,
    pub version: String,
}

/// Meta information about a package.
#[derive(Clone, Debug, PartialEq)]
pub struct PackageInfo {
    pub repo_url: Option<String>,
}

pub struct DependencyUpdateTarget {
    pub target_version: Option<RequirementVersion>,
    pub latest_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum UpdateType {
    Patch,
    Minor,
    Major,
    UpToDate,
    Unknown,
}

pub struct VersionData {
    pub target_minor: Option<String>,
    pub target_major: Option<String>,
    pub head_minor: Option<String>,
    pub head_major: Option<String>,
    pub repo_url: Option<String>,
}

#[derive(Clone)]
pub struct ProposedBump {
    pub target_version: String,
    pub head_version: String,
    pub is_major: bool,
    pub update_type: UpdateType,
}

/// Describes how a dependency can be updated.
#[derive(Clone)]
pub struct DependencyUpdateOption {
    pub package_info: PackageInfo,
    pub bumps: Vec<ProposedBump>,
}

/// Represents a specific dependency update that has been selected for execution.
///
/// Unlike `DependencyUpdateOption` which represents possible updates during the evaluation phase,
/// `UpdateTarget` represents an actionable update containing the exact current and target version details
/// needed to modify manifests and generate a pull request.
#[derive(Clone, Debug, PartialEq)]
pub struct UpdateTarget {
    pub name: String,
    pub current_version: RequirementVersion,
    pub target_version: RequirementVersion,
    pub latest_version: String,
    pub package_info: PackageInfo,
    pub minimum_release_age: Option<chrono::Duration>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub publish_time: chrono::DateTime<chrono::Utc>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Advisory {
    pub id: String,
    pub title: String,
    pub url: String,
    pub severity: String,
}
