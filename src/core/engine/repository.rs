use anyhow::Result;
use async_trait::async_trait;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileState {
    Write(String),
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileModification {
    pub path: String,
    pub state: FileState,
}

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
    fn snapshot(&self, revision: &str) -> Box<dyn ProjectRepositorySnapshot>;
}

/// An atomic view on a project (at a specific revision).
#[async_trait]
pub trait ProjectRepositorySnapshot: Send + Sync {
    async fn list_files(&self) -> Result<Vec<String>>;

    /// Returns Err() if the file doesn't exist.
    async fn read_file(&self, path: &str) -> Result<String>;
}

/// An abstracted view for mutating a project repository.
#[async_trait]
pub trait ProjectRepositoryMutator: Send + Sync {
    /// Commits the given file modifications to a new branch off the base revision.
    /// Returns the SHA of the new commit, or None if the modifications resulted in no changes.
    async fn commit_changes(
        &self,
        base_revision: &str,
        branch_name: &str,
        commit_message: &str,
        modifications: Vec<FileModification>,
    ) -> Result<Option<String>>;

    /// Creates a Pull Request for the newly created branch.
    async fn create_pull_request(
        &self,
        title: &str,
        branch_name: &str,
        base_branch: &str,
        body: &str,
    ) -> Result<String>;

    /// Closes an empty or abandoned Pull Request branch.
    async fn close_pull_request(&self, branch_name: &str, base_branch: &str) -> Result<()>;
}
