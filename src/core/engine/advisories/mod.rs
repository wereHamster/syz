use async_trait::async_trait;

/// Represents a single security advisory/vulnerability.
#[derive(Clone, Debug, PartialEq)]
pub struct Advisory {
    /// Unique identifier for the advisory (e.g., OSV ID).
    ///
    /// Example: `"GHSA-xxxx-xxxx-xxxx"` or `"OSV-ID-123"`.
    pub id: String,

    /// A short summary or title of the vulnerability.
    ///
    /// Example: `"Improper Input Validation in package-name"`.
    pub title: String,

    /// A URL pointing to more details about the advisory.
    ///
    /// Example: `"https://osv.dev/vulnerability/GHSA-xxxx-xxxx-xxxx"`.
    pub url: String,

    /// The severity level of the vulnerability (e.g., CVSS score or descriptive string).
    ///
    /// Example: `"7.5"` or `"HIGH"`.
    pub severity: String,
}

#[derive(Clone, Debug, PartialEq)]
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

#[async_trait]
pub trait AdvisoryResolver: Send + Sync {
    async fn resolve_advisories(
        &self,
        ecosystem: &str,
        name: &str,
        current_version: &str,
        target_version: &str,
    ) -> anyhow::Result<Vec<Advisory>>;
}

pub struct OsvAdvisoryResolver {
    client: crate::core::clients::osv::OsvClient,
}

impl OsvAdvisoryResolver {
    pub fn new(client: crate::core::clients::osv::OsvClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl AdvisoryResolver for OsvAdvisoryResolver {
    async fn resolve_advisories(
        &self,
        ecosystem: &str,
        name: &str,
        current_version: &str,
        target_version: &str,
    ) -> anyhow::Result<Vec<Advisory>> {
        let current_vulns = match self.client.query(ecosystem, name, current_version).await {
            Ok(v) => v,
            Err(_) => return Ok(Vec::new()),
        };

        if current_vulns.is_empty() {
            return Ok(Vec::new());
        }

        let target_vulns = match self.client.query(ecosystem, name, target_version).await {
            Ok(v) => v,
            Err(_) => return Ok(Vec::new()),
        };

        let mut target_ids = std::collections::HashSet::new();
        for v in target_vulns {
            if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                target_ids.insert(id.to_string());
            }
        }

        let mut resolved = Vec::new();
        for v in current_vulns {
            if let Some(id) = v.get("id").and_then(|i| i.as_str()) {
                if !target_ids.contains(id) {
                    let title = v
                        .get("summary")
                        .and_then(|s| s.as_str())
                        .unwrap_or("No summary provided")
                        .to_string();

                    let mut url = format!("https://osv.dev/vulnerability/{}", id);
                    if let Some(refs) = v.get("references").and_then(|r| r.as_array()) {
                        for r in refs {
                            if let Some(type_val) = r.get("type").and_then(|t| t.as_str()) {
                                if type_val == "ADVISORY" {
                                    if let Some(u) = r.get("url").and_then(|u| u.as_str()) {
                                        url = u.to_string();
                                        break;
                                    }
                                }
                            }
                        }
                    }

                    let mut severity = "UNKNOWN".to_string();
                    if let Some(sevs) = v.get("severity").and_then(|s| s.as_array()) {
                        for sev in sevs {
                            if let Some(t) = sev.get("type").and_then(|t| t.as_str()) {
                                if t == "CVSS_V3" {
                                    if let Some(score) = sev.get("score").and_then(|s| s.as_str()) {
                                        severity = score.to_string();
                                    }
                                }
                            }
                        }
                    }

                    resolved.push(Advisory {
                        id: id.to_string(),
                        title,
                        url,
                        severity,
                    });
                }
            }
        }

        Ok(resolved)
    }
}
