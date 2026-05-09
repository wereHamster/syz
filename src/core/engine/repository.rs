use anyhow::Result;
use async_trait::async_trait;

/// An abstracted, read-only view of a project repository's source code.
///
/// This trait avoids Git-specific concepts (like Trees or Blobs) to support
/// alternate backends (e.g., Jujutsu, local filesystems) in the future.
#[async_trait]
pub trait ProjectRepositoryView: Send + Sync {
    /// Gets the name of the repository's default branch (e.g., "main" or "master").
    ///
    /// This is the branch that pull requests are created against.
    async fn get_default_branch(&self) -> Result<String>;

    /// Gets the latest revision identifier (e.g., a commit SHA or `jj` change ID)
    /// for a given branch or ref. Returns Err() if the branch doesn't exist.
    async fn get_revision(&self, branch_name: &str) -> Result<String>;

    /// Returns an atomic view of the repository at the given revision.
    fn snapshot(&self, revision: &str) -> Box<dyn ProjectRepositoryViewRepositorySnapshot>;
}

/// An atomic view on a project (at a specific revision).
#[async_trait]
pub trait ProjectRepositoryViewRepositorySnapshot: Send + Sync {
    async fn list_files(&self) -> Result<Vec<String>>;

    /// Returns Err() if the file doesn't exist.
    async fn read_file(&self, path: &str) -> Result<String>;
}
