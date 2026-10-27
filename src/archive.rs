//! Verify configured VM archives before a model choice is considered ready.

use crate::config::{ArchiveHash, VmArchive};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Read;
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("reading VM archive {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("VM archive {0} must be a regular file, not a symlink")]
    NotRegular(PathBuf),
    #[error("VM archive {path} has SHA-256 {actual}, expected {expected}")]
    ChecksumMismatch {
        path: PathBuf,
        expected: ArchiveHash,
        actual: String,
    },
}

/// Evidence that the configured archive's opened file matched its checksum.
#[must_use = "pass this evidence to model validation"]
pub struct VerifiedArchive<'a> {
    archive: &'a VmArchive,
}

impl VerifiedArchive<'_> {
    pub fn matches(&self, archive: &VmArchive) -> bool {
        self.archive == archive
    }
}

#[cfg(test)]
impl<'a> VerifiedArchive<'a> {
    pub(crate) fn for_test(archive: &'a VmArchive) -> Self {
        Self { archive }
    }
}

/// Verify the opened file; execution must verify its own private copy again.
pub fn verify_archive(archive: &VmArchive) -> Result<VerifiedArchive<'_>, ArchiveError> {
    let path = archive.path().to_path_buf();
    let metadata = fs::symlink_metadata(&path).map_err(|source| ArchiveError::Read {
        path: path.clone(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(ArchiveError::NotRegular(path));
    }
    let mut file = open_no_follow(&path).map_err(|source| ArchiveError::Read {
        path: path.clone(),
        source,
    })?;
    if !file
        .metadata()
        .map_err(|source| ArchiveError::Read {
            path: path.clone(),
            source,
        })?
        .file_type()
        .is_file()
    {
        return Err(ArchiveError::NotRegular(path));
    }
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let size = file
            .read(&mut buffer)
            .map_err(|source| ArchiveError::Read {
                path: path.clone(),
                source,
            })?;
        if size == 0 {
            break;
        }
        digest.update(&buffer[..size]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != archive.checksum().as_str() {
        return Err(ArchiveError::ChecksumMismatch {
            path,
            expected: archive.checksum().clone(),
            actual,
        });
    }
    Ok(VerifiedArchive { archive })
}

fn open_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        // Open the reparse point itself so file.metadata() can reject it.
        const FILE_FLAG_OPEN_REPARSE_POINT: u32 = 0x0020_0000;
        options.custom_flags(FILE_FLAG_OPEN_REPARSE_POINT);
    }
    options.open(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verifies_regular_archive_and_rejects_changed_contents() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviewer.tar");
        fs::write(&path, b"archive bytes").unwrap();
        let checksum = ArchiveHash::new(format!("{:x}", Sha256::digest(b"archive bytes"))).unwrap();
        let archive = VmArchive::new(path.clone(), checksum).unwrap();
        assert!(verify_archive(&archive).unwrap().matches(&archive));
        fs::write(&path, b"changed bytes").unwrap();
        assert!(matches!(
            verify_archive(&archive),
            Err(ArchiveError::ChecksumMismatch { .. })
        ));
        fs::remove_file(&path).unwrap();
        assert!(matches!(
            verify_archive(&archive),
            Err(ArchiveError::Read { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_archive() {
        use std::os::unix::fs::symlink;

        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.tar");
        let link = dir.path().join("reviewer.tar");
        fs::write(&target, b"archive bytes").unwrap();
        symlink(&target, &link).unwrap();
        let archive = VmArchive::new(link, ArchiveHash::new("a".repeat(64)).unwrap()).unwrap();
        assert!(open_no_follow(archive.path()).is_err());
        assert!(matches!(
            verify_archive(&archive),
            Err(ArchiveError::NotRegular(_))
        ));
    }
}
