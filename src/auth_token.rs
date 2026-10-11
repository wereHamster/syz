//! The shared auth token, persisted in a file so that clients running on the
//! same host as the server can pick it up without any configuration.

use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use anyhow::{bail, Context, Result};
use tempfile::NamedTempFile;

/// Where `syzd` keeps the token, relative to its working directory.
pub const SERVER_PATH: &str = "data/auth-token";

/// Where `syzctl` looks for the token if none was given explicitly. This is
/// `SERVER_PATH` for a `syzd` running with `/var/lib/syz` as working directory.
pub const CLIENT_PATH: &str = "/var/lib/syz/data/auth-token";

/// Read the token from `path`.
pub fn read(path: &Path) -> Result<String> {
    let token = std::fs::read_to_string(path)
        .with_context(|| format!("Failed to read auth token from {}", path.display()))?;
    let token = token.trim();
    if token.is_empty() {
        bail!("Auth token file {} is empty", path.display());
    }
    Ok(token.to_owned())
}

/// Read the token from `path`, creating the file with a new random token if it
/// does not exist. An existing file is never modified.
///
/// The file is readable by the owner and the group, so that users in the group
/// of the server can use it.
pub fn load_or_create(path: &Path) -> Result<String> {
    match read(path) {
        Err(e) if is_not_found(&e) => {}
        result => return result,
    }

    let dir = path.parent().context("Auth token path has no parent")?;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("Failed to create directory {}", dir.display()))?;

    let token = crate::core::database::pk();

    // Write the file to its final content and permissions before it appears at
    // `path`, so a client never sees a partial token.
    let mut file = NamedTempFile::new_in(dir).context("Failed to create auth token file")?;
    file.write_all(token.as_bytes())
        .context("Failed to write auth token file")?;
    file.as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o640))
        .context("Failed to set permissions on auth token file")?;

    match file.persist_noclobber(path) {
        Ok(_) => {
            tracing::info!("Generated auth token at {}", path.display());
            Ok(token)
        }
        // Somebody else created it in the meantime, use theirs.
        Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => read(path),
        Err(e) => Err(e.error)
            .with_context(|| format!("Failed to create auth token file {}", path.display())),
    }
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_the_file_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data/auth-token");

        let token = load_or_create(&path).unwrap();

        assert!(!token.is_empty());
        assert_eq!(read(&path).unwrap(), token);
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o640);
    }

    #[test]
    fn keeps_an_existing_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth-token");
        std::fs::write(&path, "existing\n").unwrap();

        assert_eq!(load_or_create(&path).unwrap(), "existing");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "existing\n");
    }

    #[test]
    fn returns_the_same_token_on_every_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth-token");

        assert_eq!(
            load_or_create(&path).unwrap(),
            load_or_create(&path).unwrap()
        );
    }

    #[test]
    fn rejects_an_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("auth-token");
        std::fs::write(&path, "\n").unwrap();

        assert!(load_or_create(&path).is_err());
        assert!(read(&path).is_err());
    }

    #[test]
    fn reading_a_missing_file_fails() {
        let dir = tempfile::tempdir().unwrap();
        assert!(read(&dir.path().join("auth-token")).is_err());
    }
}
