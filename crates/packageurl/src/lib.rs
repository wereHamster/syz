//! Package identity types shared across the workspace.
//!
//! [`PURL`] models a Package-URL (https://packageurl.org/) and
//! [`PackageName`] is a validated-ish newtype for names as they appear in
//! manifests (including scoped names like `@babel/core`).

use serde::{Deserialize, Serialize};

/// Pacakge URL
///
/// See https://packageurl.org/
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn purl_package_name_with_namespace() {
        let purl = PURL {
            ecosystem: "npm".to_string(),
            namespace: Some("@babel".to_string()),
            name: "core".to_string(),
            subpath: None,
            version: Some("7.0.0".to_string()),
        };
        assert_eq!(purl.package_name(), "@babel/core");
        assert_eq!(purl.to_string(), "pkg:npm/@babel/core@7.0.0");
    }

    #[test]
    fn purl_package_name_github_actions_subpath() {
        let purl = PURL {
            ecosystem: "github-actions".to_string(),
            namespace: Some("actions".to_string()),
            name: "checkout".to_string(),
            subpath: Some("action".to_string()),
            version: Some("v4".to_string()),
        };
        assert_eq!(purl.package_name(), "actions/checkout/action");
    }

    #[test]
    fn purl_package_name_unnamespaced() {
        let purl = PURL {
            ecosystem: "cargo".to_string(),
            namespace: None,
            name: "serde".to_string(),
            subpath: None,
            version: None,
        };
        assert_eq!(purl.package_name(), "serde");
    }

    #[test]
    fn package_name_short_and_scoped() {
        let p = PackageName::new("@sentry/core");
        assert!(p.is_scoped());
        assert_eq!(p.short(), "core");

        let p = PackageName::new("knip");
        assert!(!p.is_scoped());
        assert_eq!(p.short(), "knip");
    }
}
