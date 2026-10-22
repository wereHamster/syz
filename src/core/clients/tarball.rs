//! Unpacking the `.tar.gz` of a repository, as GitHub serves it.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use tar::EntryType;

/// Unpacks the gzipped tar `archive` into `dir`, without the directory every entry is in
/// (`owner-repo-<sha>/`). Files are written as they are, whatever is in them.
///
/// Fails when the files add up to more than `max_bytes`. Entries that would end up outside of
/// `dir` are refused, and links that point outside of it are left out.
pub fn unpack(archive: impl Read, dir: &Path, max_bytes: usize) -> Result<()> {
    let mut archive = tar::Archive::new(GzDecoder::new(archive));
    archive.set_preserve_permissions(true);

    let mut bytes = 0u64;
    for entry in archive.entries().context("Failed to read the archive")? {
        let mut entry = entry.context("Failed to read an entry of the archive")?;
        let entry_type = entry.header().entry_type();
        if !matches!(
            entry_type,
            EntryType::Regular | EntryType::Continuous | EntryType::Directory | EntryType::Symlink
        ) {
            continue;
        }

        let path = entry.path()?.into_owned();
        let relative: PathBuf = path.components().skip(1).collect();
        if relative.as_os_str().is_empty() {
            continue;
        }
        if !relative
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
        {
            bail!(
                "Refusing to unpack {} outside of the directory",
                path.display()
            );
        }

        bytes += entry.size();
        if bytes > max_bytes as u64 {
            bail!("The repository is larger than the {max_bytes} bytes that are put on disk");
        }

        if entry_type == EntryType::Symlink {
            let target = entry.link_name()?.map(|target| target.into_owned());
            if !target.is_some_and(|target| stays_inside(&relative, &target)) {
                tracing::warn!(
                    "Leaving out the link {}, it points outside",
                    relative.display()
                );
                continue;
            }
        }

        let destination = dir.join(&relative);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)?;
        }
        entry
            .unpack(&destination)
            .with_context(|| format!("Failed to unpack {}", relative.display()))?;
    }

    Ok(())
}

/// Whether the link at `link`, which points to `target`, stays within the directory that
/// `link` is relative to.
fn stays_inside(link: &Path, target: &Path) -> bool {
    let mut depth = link.components().count() as i64 - 1;
    for component in target.components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::ParentDir => depth -= 1,
            Component::CurDir => {}
            _ => return false,
        }
        if depth < 0 {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn archive(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (path, content) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            builder.append_data(&mut header, path, *content).unwrap();
        }
        let tar = builder.into_inner().unwrap();

        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        std::io::Write::write_all(&mut encoder, &tar).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn files_are_unpacked_as_they_are_without_the_top_directory() {
        let binary: &[u8] = &[b'a', b'g', b'e', 0xff, 0xfe, 0x00, 0x80];
        let dir = tempfile::tempdir().unwrap();

        unpack(
            archive(&[
                ("owner-repo-abc/flake.nix", b"{}"),
                ("owner-repo-abc/secrets/token.age", binary),
            ])
            .as_slice(),
            dir.path(),
            1024,
        )
        .unwrap();

        assert_eq!(std::fs::read(dir.path().join("flake.nix")).unwrap(), b"{}");
        assert_eq!(
            std::fs::read(dir.path().join("secrets/token.age")).unwrap(),
            binary
        );
    }

    #[test]
    fn links_must_stay_inside() {
        assert!(stays_inside(Path::new("a/link"), Path::new("../b")));
        assert!(stays_inside(Path::new("link"), Path::new("a/b")));
        assert!(!stays_inside(Path::new("link"), Path::new("../b")));
        assert!(!stays_inside(Path::new("a/link"), Path::new("../../b")));
        assert!(!stays_inside(Path::new("link"), Path::new("/etc/passwd")));
    }
}
