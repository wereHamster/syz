pub mod advisories;
pub mod ecosystems;
pub mod groups;
pub mod pull_request_generator;
pub mod releases;
pub mod repository;

pub use crate::core::types::{DiscoveredDependency, PURL};

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

pub struct TransitiveUpdateSummary {
    pub added: Vec<(String, String)>,
    pub removed: Vec<(String, String)>,
    pub major_bumps: Vec<(String, String)>,
    pub minor_bumps: Vec<(String, String)>,
}

pub struct TransitiveUpdateResult {
    pub modifications: Vec<crate::core::engine::repository::FileModification>,
    pub summary: TransitiveUpdateSummary,
}

pub struct ResolvedAdvisoryBump {
    pub before_versions: Vec<String>,
    pub after_versions: Vec<String>,
    pub advisories: Vec<serde_json::Value>,
}

pub struct SecurityUpdateSummary {
    pub resolved_advisories: std::collections::HashMap<String, ResolvedAdvisoryBump>,
    pub blocked_by_age:
        std::collections::HashMap<String, Vec<(semver::Version, chrono::DateTime<chrono::Utc>)>>,
    pub unfixable_vulnerabilities: std::collections::HashMap<String, Vec<serde_json::Value>>,
    pub minimum_release_age: Option<chrono::Duration>,
}

pub struct SecurityUpdateResult {
    pub modifications: Vec<crate::core::engine::repository::FileModification>,
    pub summary: SecurityUpdateSummary,
}
