use anyhow::Result;
use async_trait::async_trait;

use crate::core::engine::Advisory;

#[async_trait]
pub trait AdvisoryResolver: Send + Sync {
    async fn resolve_advisories(
        &self,
        ecosystem: &str,
        name: &str,
        current_version: &str,
        target_version: &str,
    ) -> Result<Vec<Advisory>>;
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
    ) -> Result<Vec<Advisory>> {
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
                            if let Some(t) = r.get("type").and_then(|t| t.as_str()) {
                                if t == "ADVISORY" {
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
