use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait ReleaseNotesResolver: Send + Sync {
    /// Resolves the release notes/changelog for a specific version.
    /// Returns Ok(Some((display_tag, markdown_content))) if found.
    async fn resolve_release_notes(&self, version: &str) -> Result<Option<(String, String)>>;
}
