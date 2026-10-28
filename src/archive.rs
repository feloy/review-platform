//! Verify and stage a VM rootfs tar together with its separate OCI configuration.

use crate::config::{ArchiveHash, VmArchive, valid_environment_entry, valid_vm_absolute_path};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ArchiveError {
    #[error("reading VM image input {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("VM image input {0} must be a regular file")]
    NotRegular(PathBuf),
    #[error("VM image input {path} has SHA-256 {actual}, expected {expected}")]
    ChecksumMismatch {
        path: PathBuf,
        expected: ArchiveHash,
        actual: String,
    },
    #[error("storing verified VM image at {path}: {source}")]
    Store {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("VM image configuration exceeds 1 MiB")]
    ConfigTooLarge,
    #[error("VM image configuration JSON is invalid: {0}")]
    InvalidConfig(#[from] serde_json::Error),
    #[error("VM image configuration has an invalid or duplicate Env entry at index {0}")]
    InvalidEnvironment(usize),
    #[error("VM image environment exceeds 64 KiB")]
    EnvironmentTooLarge,
    #[error("VM image configuration lacks an absolute CLAUDE_CODE_EXECUTABLE path")]
    MissingClaudeExecutable,
}

const CONFIG_LIMIT: u64 = 1024 * 1024;

/// Evidence that both opened VM image files matched their configured checksums.
#[must_use = "pass this evidence to model validation"]
pub struct VerifiedArchive<'a> {
    archive: &'a VmArchive,
    actual_sha256: String,
    actual_config_sha256: String,
    environment: BTreeMap<String, String>,
}

impl VerifiedArchive<'_> {
    pub fn matches(&self, archive: &VmArchive) -> bool {
        self.archive == archive
            && self.actual_sha256 == archive.checksum().as_str()
            && self.actual_config_sha256 == archive.config_checksum().as_str()
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }
}

/// Private verified files used to create a sandbox and supply its image environment.
pub struct StagedArchive {
    path: PathBuf,
    environment: BTreeMap<String, String>,
}

impl StagedArchive {
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn environment(&self) -> &BTreeMap<String, String> {
        &self.environment
    }

    pub fn claude_executable(&self) -> Result<&str, ArchiveError> {
        let value = self
            .environment
            .get("CLAUDE_CODE_EXECUTABLE")
            .ok_or(ArchiveError::MissingClaudeExecutable)?;
        if !valid_vm_absolute_path(value) {
            return Err(ArchiveError::MissingClaudeExecutable);
        }
        Ok(value)
    }
}

#[cfg(test)]
impl StagedArchive {
    pub(crate) fn for_test(path: PathBuf, environment: BTreeMap<String, String>) -> Self {
        Self { path, environment }
    }
}

#[cfg(test)]
impl<'a> VerifiedArchive<'a> {
    pub(crate) fn for_test(archive: &'a VmArchive) -> Self {
        Self {
            archive,
            actual_sha256: archive.checksum().as_str().to_owned(),
            actual_config_sha256: archive.config_checksum().as_str().to_owned(),
            environment: BTreeMap::new(),
        }
    }
}

/// Verify the opened file; execution must verify its own private copy again.
pub fn verify_archive(archive: &VmArchive) -> Result<VerifiedArchive<'_>, ArchiveError> {
    let (actual, _) = verify_file(archive.path(), archive.checksum(), None)?;
    let (actual_config, config_bytes) = verify_file(
        archive.config_path().as_path(),
        archive.config_checksum(),
        Some(CONFIG_LIMIT),
    )?;
    let environment = parse_environment(&config_bytes)?;
    Ok(VerifiedArchive {
        archive,
        actual_sha256: actual,
        actual_config_sha256: actual_config,
        environment,
    })
}

/// Copy both verified image inputs to private, pair-addressed storage.
pub fn stage_archive(archive: &VmArchive, directory: &Path) -> Result<StagedArchive, ArchiveError> {
    fs::create_dir_all(directory).map_err(|source| ArchiveError::Store {
        path: directory.to_owned(),
        source,
    })?;
    let metadata = fs::symlink_metadata(directory).map_err(|source| ArchiveError::Store {
        path: directory.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_dir() {
        return Err(ArchiveError::NotRegular(directory.to_owned()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|source| {
            ArchiveError::Store {
                path: directory.to_owned(),
                source,
            }
        })?;
    }
    let parent = fs::canonicalize(directory).map_err(|source| ArchiveError::Store {
        path: directory.to_owned(),
        source,
    })?;
    let pair = parent.join(format!(
        "{}-{}",
        archive.checksum(),
        archive.config_checksum()
    ));
    if !pair.exists() {
        let temporary = tempfile::Builder::new()
            .prefix(".vm-image-")
            .tempdir_in(&parent)
            .map_err(|source| ArchiveError::Store {
                path: parent.clone(),
                source,
            })?;
        copy_verified(
            archive.path(),
            &temporary.path().join("rootfs.tar"),
            archive.checksum(),
            None,
        )?;
        copy_verified(
            &archive.config_path(),
            &temporary.path().join("rootfs.tar.config.json"),
            archive.config_checksum(),
            Some(CONFIG_LIMIT),
        )?;
        let provisional = VmArchive::new(
            temporary.path().join("rootfs.tar"),
            archive.checksum().clone(),
            archive.config_checksum().clone(),
        )
        .map_err(|_| ArchiveError::NotRegular(temporary.path().to_owned()))?;
        let _ = verify_archive(&provisional)?;
        match fs::rename(temporary.path(), &pair) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(source) => {
                return Err(ArchiveError::Store { path: pair, source });
            }
        }
        #[cfg(unix)]
        File::open(&parent)
            .and_then(|file| file.sync_all())
            .map_err(|source| ArchiveError::Store {
                path: parent.clone(),
                source,
            })?;
    }
    let pair_metadata = fs::symlink_metadata(&pair).map_err(|source| ArchiveError::Read {
        path: pair.clone(),
        source,
    })?;
    if !pair_metadata.file_type().is_dir() {
        return Err(ArchiveError::NotRegular(pair));
    }
    let saved = VmArchive::new(
        pair.join("rootfs.tar"),
        archive.checksum().clone(),
        archive.config_checksum().clone(),
    )
    .map_err(|_| ArchiveError::NotRegular(pair.clone()))?;
    let verified = verify_archive(&saved)?;
    Ok(StagedArchive {
        path: saved.path().to_owned(),
        environment: verified.environment().clone(),
    })
}

fn verify_file(
    path: &Path,
    expected: &ArchiveHash,
    limit: Option<u64>,
) -> Result<(String, Vec<u8>), ArchiveError> {
    let metadata = fs::symlink_metadata(path).map_err(|source| ArchiveError::Read {
        path: path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(ArchiveError::NotRegular(path.to_owned()));
    }
    if limit.is_some_and(|limit| metadata.len() > limit) {
        return Err(ArchiveError::ConfigTooLarge);
    }
    let mut file = open_no_follow(path).map_err(|source| ArchiveError::Read {
        path: path.to_owned(),
        source,
    })?;
    if !file
        .metadata()
        .map_err(|source| ArchiveError::Read {
            path: path.to_owned(),
            source,
        })?
        .file_type()
        .is_file()
    {
        return Err(ArchiveError::NotRegular(path.to_owned()));
    }
    let mut digest = Sha256::new();
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let size = file
            .read(&mut buffer)
            .map_err(|source| ArchiveError::Read {
                path: path.to_owned(),
                source,
            })?;
        if size == 0 {
            break;
        }
        if let Some(limit) = limit {
            if bytes.len() as u64 + size as u64 > limit {
                return Err(ArchiveError::ConfigTooLarge);
            }
            bytes.extend_from_slice(&buffer[..size]);
        }
        digest.update(&buffer[..size]);
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected.as_str() {
        return Err(ArchiveError::ChecksumMismatch {
            path: path.to_owned(),
            expected: expected.clone(),
            actual,
        });
    }
    Ok((actual, bytes))
}

fn copy_verified(
    source_path: &Path,
    destination: &Path,
    expected: &ArchiveHash,
    limit: Option<u64>,
) -> Result<(), ArchiveError> {
    let metadata = fs::symlink_metadata(source_path).map_err(|source| ArchiveError::Read {
        path: source_path.to_owned(),
        source,
    })?;
    if !metadata.file_type().is_file() {
        return Err(ArchiveError::NotRegular(source_path.to_owned()));
    }
    if limit.is_some_and(|limit| metadata.len() > limit) {
        return Err(ArchiveError::ConfigTooLarge);
    }
    let mut source = open_no_follow(source_path).map_err(|source| ArchiveError::Read {
        path: source_path.to_owned(),
        source,
    })?;
    if !source
        .metadata()
        .map_err(|source| ArchiveError::Read {
            path: source_path.to_owned(),
            source,
        })?
        .file_type()
        .is_file()
    {
        return Err(ArchiveError::NotRegular(source_path.to_owned()));
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut target = options
        .open(destination)
        .map_err(|source| ArchiveError::Store {
            path: destination.to_owned(),
            source,
        })?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    let mut total = 0_u64;
    loop {
        let size = source
            .read(&mut buffer)
            .map_err(|source| ArchiveError::Read {
                path: source_path.to_owned(),
                source,
            })?;
        if size == 0 {
            break;
        }
        total += size as u64;
        if limit.is_some_and(|limit| total > limit) {
            return Err(ArchiveError::ConfigTooLarge);
        }
        digest.update(&buffer[..size]);
        target
            .write_all(&buffer[..size])
            .map_err(|source| ArchiveError::Store {
                path: destination.to_owned(),
                source,
            })?;
    }
    let actual = format!("{:x}", digest.finalize());
    if actual != expected.as_str() {
        return Err(ArchiveError::ChecksumMismatch {
            path: source_path.to_owned(),
            expected: expected.clone(),
            actual,
        });
    }
    target.sync_all().map_err(|source| ArchiveError::Store {
        path: destination.to_owned(),
        source,
    })?;
    Ok(())
}

#[derive(Deserialize)]
struct ImageMetadata {
    config: ImageConfig,
}

#[derive(Deserialize)]
struct ImageConfig {
    #[serde(rename = "Env")]
    env: Vec<String>,
}

fn parse_environment(bytes: &[u8]) -> Result<BTreeMap<String, String>, ArchiveError> {
    let metadata: ImageMetadata = serde_json::from_slice(bytes)?;
    if metadata.config.env.len() > 256 {
        return Err(ArchiveError::InvalidEnvironment(256));
    }
    let mut environment = BTreeMap::new();
    let mut total = 0_usize;
    for (index, entry) in metadata.config.env.into_iter().enumerate() {
        total += entry.len();
        if total > 64 * 1024 {
            return Err(ArchiveError::EnvironmentTooLarge);
        }
        let Some((key, value)) = entry.split_once('=') else {
            return Err(ArchiveError::InvalidEnvironment(index));
        };
        if !valid_environment_entry(key, value)
            || environment
                .insert(key.to_owned(), value.to_owned())
                .is_some()
        {
            return Err(ArchiveError::InvalidEnvironment(index));
        }
    }
    Ok(environment)
}

fn open_no_follow(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // A FIFO swapped in after the metadata check must not block open.
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
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
        let config =
            br#"{"config":{"Env":["PATH=/usr/bin","CLAUDE_CODE_EXECUTABLE=/usr/bin/claude"]}}"#;
        fs::write(dir.path().join("reviewer.tar.config.json"), config).unwrap();
        let checksum = ArchiveHash::new(format!("{:x}", Sha256::digest(b"archive bytes"))).unwrap();
        let config_checksum = ArchiveHash::new(format!("{:x}", Sha256::digest(config))).unwrap();
        let archive = VmArchive::new(path.clone(), checksum, config_checksum).unwrap();
        let mut verified = verify_archive(&archive).unwrap();
        assert_eq!(
            verified.actual_sha256,
            format!("{:x}", Sha256::digest(b"archive bytes"))
        );
        assert!(verified.matches(&archive));
        assert_eq!(
            verified.environment().get("CLAUDE_CODE_EXECUTABLE"),
            Some(&"/usr/bin/claude".to_owned())
        );
        let staged = stage_archive(&archive, &dir.path().join("private")).unwrap();
        assert_eq!(staged.claude_executable().unwrap(), "/usr/bin/claude");
        assert_eq!(fs::read(staged.path()).unwrap(), b"archive bytes");
        verified.actual_sha256 = "0".repeat(64);
        assert!(!verified.matches(&archive));
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

    #[test]
    fn metadata_changes_and_ambiguous_environment_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviewer.tar");
        let config_path = dir.path().join("reviewer.tar.config.json");
        let config = br#"{"config":{"Env":["CLAUDE_CODE_EXECUTABLE=/usr/bin/claude"]}}"#;
        fs::write(&path, b"archive bytes").unwrap();
        fs::write(&config_path, config).unwrap();
        let archive = VmArchive::new(
            path,
            ArchiveHash::new(format!("{:x}", Sha256::digest(b"archive bytes"))).unwrap(),
            ArchiveHash::new(format!("{:x}", Sha256::digest(config))).unwrap(),
        )
        .unwrap();
        fs::write(&config_path, b"changed metadata").unwrap();
        assert!(matches!(
            verify_archive(&archive),
            Err(ArchiveError::ChecksumMismatch { path, .. }) if path == config_path
        ));
        let duplicate = br#"{"config":{"Env":["PATH=/usr/bin","PATH=/bin"]}}"#;
        assert!(matches!(
            parse_environment(duplicate),
            Err(ArchiveError::InvalidEnvironment(_))
        ));
        let secret = br#"{"config":{"Env":["API_TOKEN=secret"]}}"#;
        assert!(matches!(
            parse_environment(secret),
            Err(ArchiveError::InvalidEnvironment(_))
        ));
        let missing = StagedArchive::for_test(PathBuf::from("/tmp/image.tar"), BTreeMap::new());
        assert!(matches!(
            missing.claude_executable(),
            Err(ArchiveError::MissingClaudeExecutable)
        ));
        let relative = StagedArchive::for_test(
            PathBuf::from("/tmp/image.tar"),
            BTreeMap::from([("CLAUDE_CODE_EXECUTABLE".into(), "bin/claude".into())]),
        );
        assert!(matches!(
            relative.claude_executable(),
            Err(ArchiveError::MissingClaudeExecutable)
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
        let archive = VmArchive::new(
            link,
            ArchiveHash::new("a".repeat(64)).unwrap(),
            ArchiveHash::new("c".repeat(64)).unwrap(),
        )
        .unwrap();
        assert!(open_no_follow(archive.path()).is_err());
        assert!(matches!(
            verify_archive(&archive),
            Err(ArchiveError::NotRegular(_))
        ));

        let regular = dir.path().join("regular.tar");
        fs::write(&regular, b"archive bytes").unwrap();
        symlink(&target, dir.path().join("regular.tar.config.json")).unwrap();
        let image = VmArchive::new(
            regular,
            ArchiveHash::new(format!("{:x}", Sha256::digest(b"archive bytes"))).unwrap(),
            ArchiveHash::new("c".repeat(64)).unwrap(),
        )
        .unwrap();
        assert!(matches!(
            verify_archive(&image),
            Err(ArchiveError::NotRegular(path)) if path == image.config_path()
        ));
    }

    #[cfg(unix)]
    #[test]
    fn opening_archive_swapped_for_fifo_does_not_block() {
        use std::process::Command;
        use std::sync::mpsc;
        use std::time::Duration;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("reviewer.tar");
        fs::write(&path, b"archive bytes").unwrap();
        assert!(fs::symlink_metadata(&path).unwrap().file_type().is_file());

        fs::remove_file(&path).unwrap();
        let output = Command::new("mkfifo").arg(&path).output().unwrap();
        assert!(output.status.success(), "{output:?}");

        let (sender, receiver) = mpsc::channel();
        let opener = std::thread::spawn(move || {
            let result = open_no_follow(&path).and_then(|file| file.metadata());
            let _ = sender.send(result);
        });
        let metadata = receiver
            .recv_timeout(Duration::from_secs(5))
            .expect("opening a FIFO must not wait for a writer")
            .unwrap();
        opener.join().unwrap();
        assert!(!metadata.file_type().is_file());
    }
}
