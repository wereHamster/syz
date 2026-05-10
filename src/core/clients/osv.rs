use anyhow::Result;
use serde_json::{json, Value};

use crate::core::http_agent::HttpAgent;

#[derive(Clone)]
pub struct OsvClient {
    http_agent: HttpAgent,
}

impl OsvClient {
    pub fn new(http_agent: HttpAgent) -> Self {
        Self { http_agent }
    }

    pub async fn query(&self, ecosystem: &str, name: &str, version: &str) -> Result<Vec<Value>> {
        let osv_eco = match ecosystem {
            "cargo" => "crates.io",
            "npm" => "npm",
            "github-actions" => "GitHub Actions",
            _ => ecosystem,
        };

        let body = json!({
            "version": version,
            "package": {
                "ecosystem": osv_eco,
                "name": name
            }
        });

        let res: Value = self
            .http_agent
            .post_json_body("https://api.osv.dev/v1/query", &body)
            .await?;

        let vulns = res
            .get("vulns")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        Ok(vulns)
    }
}
