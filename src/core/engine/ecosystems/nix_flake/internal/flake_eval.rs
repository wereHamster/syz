//! Evaluating a flake with the lock before and after an update.
//!
//! Evaluating needs the whole repository on disk, since outputs may use any file of it, and
//! fetches the inputs of both locks. Only call it from a job.

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures::{stream, StreamExt};
use serde::de::DeserializeOwned;
use tokio::process::Command;

use crate::core::clients::github::GitHub;
use crate::core::engine::repository::{FileModification, FileState, ProjectRepositorySnapshot};

use super::apply_updates::{redact, NixAccess};
use super::flake_lock::{self, FlakeLock};

/// How many files of the repository are put on disk. A repository with more files than this
/// is not worth the requests it takes to read them.
const MAX_FILES: usize = 400;

/// How many bytes of the repository are put on disk.
const MAX_BYTES: usize = 32 * 1024 * 1024;

/// How many files are read at the same time.
const READ_CONCURRENCY: usize = 8;

/// How long one evaluation of the flake may take.
const EVAL_TIMEOUT: Duration = Duration::from_secs(600);

/// The repository on disk, with everything but its lock, and what it takes to evaluate it.
pub struct Prepared {
    pub dir: tempfile::TempDir,
    /// The lock of the repository before the update.
    pub old_lock: String,
    pub access: NixAccess,
}

/// Puts the files of `snapshot` into a directory in `work_dir`.
///
/// `None` when the `modifications` don't write `flake.lock`, or there is no lock to compare it
/// to. An error when the repository can't be put on disk.
pub async fn prepare(
    github: &GitHub,
    snapshot: &dyn ProjectRepositorySnapshot,
    work_dir: &Path,
    modifications: &[FileModification],
) -> Result<Option<Prepared>> {
    let Some(new_lock) = modifications
        .iter()
        .find_map(|modification| match &modification.state {
            FileState::Write(content) if modification.path == "flake.lock" => Some(content),
            _ => None,
        })
    else {
        return Ok(None);
    };
    let Ok(old_lock) = snapshot.read_file("flake.lock").await else {
        return Ok(None);
    };

    // The inputs of both locks are fetched, and those of the new one may belong to owners the
    // old one doesn't mention.
    let mut owners = lock_owners(&old_lock)?;
    owners.extend(lock_owners(new_lock)?);
    owners.sort();
    owners.dedup();
    let access = NixAccess::for_owners(github, &owners).await;

    std::fs::create_dir_all(work_dir)
        .with_context(|| format!("Failed to create work directory {}", work_dir.display()))?;
    let dir = tempfile::TempDir::new_in(work_dir)
        .with_context(|| format!("Failed to create a directory in {}", work_dir.display()))?;

    materialize(snapshot, dir.path()).await?;

    Ok(Some(Prepared {
        dir,
        old_lock,
        access,
    }))
}

fn lock_owners(lock: &str) -> Result<Vec<String>> {
    let lock: FlakeLock = serde_json::from_str(lock).context("Failed to parse flake.lock")?;
    Ok(flake_lock::github_owners(&lock))
}

/// Puts the files of `snapshot` into `dir`, but not the lock.
pub async fn materialize(snapshot: &dyn ProjectRepositorySnapshot, dir: &Path) -> Result<()> {
    let files: Vec<String> = snapshot
        .list_files()
        .await?
        .into_iter()
        .filter(|path| path != "flake.lock")
        .collect();
    if files.len() > MAX_FILES {
        bail!(
            "The repository has {} files, more than the {} that are put on disk",
            files.len(),
            MAX_FILES
        );
    }

    let contents: Vec<(String, Option<String>)> = stream::iter(files)
        .map(|path| async move {
            let content = snapshot.read_file(&path).await.ok();
            (path, content)
        })
        .buffer_unordered(READ_CONCURRENCY)
        .collect()
        .await;

    // Files that aren't text can't be read, and are left out.
    let mut bytes = 0;
    for (path, content) in contents {
        let Some(content) = content else { continue };
        bytes += content.len();
        if bytes > MAX_BYTES {
            bail!(
                "The repository is larger than the {} bytes that are put on disk",
                MAX_BYTES
            );
        }
        write_file(dir, &path, &content)?;
    }

    Ok(())
}

/// A path that stays inside the directory it is joined to.
fn is_contained(path: &str) -> bool {
    use std::path::Component;
    Path::new(path)
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
}

pub fn write_file(dir: &Path, path: &str, content: &str) -> Result<()> {
    if !is_contained(path) {
        bail!("Refusing to write {path} outside of the work directory");
    }
    let target = dir.join(path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&target, content).with_context(|| format!("Failed to write {path}"))
}

/// Makes the files in `dir` what the `modifications` say.
pub fn apply(dir: &Path, modifications: &[FileModification]) -> Result<()> {
    for modification in modifications {
        match &modification.state {
            FileState::Write(content) => write_file(dir, &modification.path, content)?,
            FileState::Delete => {
                if !is_contained(&modification.path) {
                    bail!(
                        "Refusing to delete {} outside of the work directory",
                        modification.path
                    );
                }
                match std::fs::remove_file(dir.join(&modification.path)) {
                    Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
        }
    }
    Ok(())
}

/// Puts the files the `modifications` touch back to what `snapshot` has.
pub async fn restore(
    snapshot: &dyn ProjectRepositorySnapshot,
    dir: &Path,
    modifications: &[FileModification],
) -> Result<()> {
    let mut originals = Vec::new();
    for modification in modifications {
        let original = snapshot.read_file(&modification.path).await.ok();
        originals.push(FileModification {
            path: modification.path.clone(),
            state: match original {
                Some(content) => FileState::Write(content),
                None => FileState::Delete,
            },
        });
    }
    apply(dir, &originals)
}

/// Evaluates `body`, a nix expression in which `flake` is the flake in `dir`, to JSON.
pub async fn evaluate<T: DeserializeOwned>(
    dir: &Path,
    access: &NixAccess,
    body: &str,
) -> Result<T> {
    let mut command = Command::new("nix");
    command
        .args(["--extra-experimental-features", "nix-command flakes"])
        .args(["eval", "--impure", "--json", "--expr"])
        .arg(expression(dir, body))
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    if let Some(config) = &access.config {
        command.env("NIX_CONFIG", config);
    }

    let output = tokio::time::timeout(EVAL_TIMEOUT, command.output())
        .await
        .context("Evaluating the flake took too long")?
        .context("Failed to run nix")?;

    if !output.status.success() {
        let stderr = redact(&String::from_utf8_lossy(&output.stderr), &access.secrets);
        bail!("nix eval failed ({}): {}", output.status, stderr.trim());
    }

    serde_json::from_slice(&output.stdout).context("Unexpected output of nix eval")
}

/// A nix string with `text` in it.
pub fn nix_string(text: &str) -> String {
    let escaped = text
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace("${", "\\${")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t");
    format!("\"{escaped}\"")
}

/// A path, as it goes into a flake reference.
fn flake_url(dir: &Path) -> String {
    let mut url = String::from("path:");
    for byte in dir.to_string_lossy().bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                url.push(byte as char)
            }
            _ => url.push_str(&format!("%{byte:02X}")),
        }
    }
    url
}

/// `body` with the flake in `dir` as `flake`.
fn expression(dir: &Path, body: &str) -> String {
    format!(
        "let flake = builtins.getFlake {}; in {}",
        nix_string(&flake_url(dir)),
        body.trim_start()
    )
}

#[cfg(test)]
pub(super) fn nix_works() -> bool {
    std::process::Command::new("nix")
        .args([
            "--extra-experimental-features",
            "nix-command flakes",
            "eval",
            "--impure",
            "--expr",
            "builtins.currentSystem",
        ])
        .output()
        .is_ok_and(|output| output.status.success())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_and_strings_are_escaped() {
        assert_eq!(
            flake_url(Path::new("/tmp/work dir/a#b")),
            "path:/tmp/work%20dir/a%23b"
        );
        assert_eq!(nix_string(r#"a"b${c}\"#), r#""a\"b\${c}\\""#);
    }

    #[test]
    fn paths_outside_the_directory_are_refused() {
        assert!(is_contained("nix/default.nix"));
        assert!(!is_contained("../escape"));
        assert!(!is_contained("/etc/passwd"));
        assert!(!is_contained("a/../../b"));
    }
}
