//! Platform state and config paths plus local JSON state persistence.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use review_platform::{PrKey, State};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

/// Advisory process lock kept on a stable sibling file while state is read and saved.
pub struct StateLock {
    _file: File,
}

#[cfg(windows)]
fn lock_is_busy(error: &std::io::Error) -> bool {
    // Windows reports exclusive-open contention as sharing or lock violations.
    matches!(error.raw_os_error(), Some(32 | 33))
}

pub fn lock(path: &Path) -> Result<StateLock> {
    let parent = parent_dir(path);
    fs::create_dir_all(&parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
        for _ in 0..200 {
            match options.open(&lock_path) {
                Ok(file) => return Ok(StateLock { _file: file }),
                Err(error) if lock_is_busy(&error) => {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(error) => {
                    return Err(error).with_context(|| format!("locking {}", lock_path.display()));
                }
            }
        }
        anyhow::bail!("timed out locking {}", lock_path.display());
    }
    #[cfg(unix)]
    {
        let file = options
            .open(&lock_path)
            .with_context(|| format!("opening lock {}", lock_path.display()))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .with_context(|| format!("locking {}", lock_path.display()))?;
        Ok(StateLock { _file: file })
    }
}

/// Serialize operations with external side effects for one PR across processes.
pub fn pr_lock(state_path: &Path, key: &PrKey) -> Result<StateLock> {
    let digest = Sha256::digest(key.to_string().as_bytes());
    let name = format!("pr-{:x}", digest);
    lock(&parent_dir(state_path).join("operations").join(name))
}

/// Nonblocking process lock used to keep recovery from stopping a live runner.
pub fn try_lock(path: &Path) -> Result<StateLock> {
    let parent = parent_dir(path);
    fs::create_dir_all(&parent).with_context(|| format!("creating {}", parent.display()))?;
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(0);
        let file = options.open(&lock_path).map_err(|error| {
            if lock_is_busy(&error) {
                anyhow::anyhow!("review runner is still active")
            } else {
                anyhow::Error::new(error).context(format!("locking {}", lock_path.display()))
            }
        })?;
        Ok(StateLock { _file: file })
    }
    #[cfg(unix)]
    {
        let file = options
            .open(&lock_path)
            .with_context(|| format!("opening lock {}", lock_path.display()))?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
            |error| {
                if std::io::Error::from(error).kind() == std::io::ErrorKind::WouldBlock {
                    anyhow::anyhow!("review runner is still active")
                } else {
                    anyhow::Error::new(error).context(format!("locking {}", lock_path.display()))
                }
            },
        )?;
        Ok(StateLock { _file: file })
    }
}

pub fn resolve_path(override_path: Option<PathBuf>) -> Result<PathBuf> {
    let path = match override_path {
        Some(path) => path,
        None => default_path()?,
    };
    absolute_path(path, "--state-file")
}

pub fn resolve_config_path(override_path: Option<PathBuf>) -> Result<PathBuf> {
    let path = match override_path {
        Some(path) => path,
        None => default_config_path()?,
    };
    absolute_path(path, "--config-file")
}

fn absolute_path(path: PathBuf, option: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        !path.as_os_str().is_empty(),
        "{option} path cannot be empty"
    );
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .with_context(|| format!("determining the current directory for {option}"))?
            .join(path))
    }
}

fn default_config_path() -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("", "", "review-platform")
        .context("cannot determine the user config directory; provide --config-file PATH")?;
    Ok(project_dirs.config_dir().join("config.json"))
}

fn default_path() -> Result<PathBuf> {
    let project_dirs = ProjectDirs::from("", "", "review-platform")
        .context("cannot determine the user state directory; provide --state-file PATH")?;
    let directory = project_dirs
        .state_dir()
        .unwrap_or_else(|| project_dirs.data_local_dir());
    Ok(directory.join("state.json"))
}

pub fn load(path: &Path) -> Result<State> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(error) => return Err(error).with_context(|| format!("reading {}", path.display())),
    };
    let mut state: State = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing state file {}", path.display()))?;
    state
        .migrate_v5()
        .with_context(|| format!("validating state file {}", path.display()))?;
    Ok(state)
}

pub fn save(path: &Path, state: &State) -> Result<()> {
    state.validate().context("validating state before save")?;
    let parent = parent_dir(path);
    fs::create_dir_all(&parent).with_context(|| format!("creating {}", parent.display()))?;

    let mut file = NamedTempFile::new_in(&parent)
        .with_context(|| format!("creating temporary file in {}", parent.display()))?;
    serde_json::to_writer_pretty(&mut file, state).context("serializing state")?;
    file.write_all(b"\n").context("finishing state file")?;
    file.flush().context("flushing state file")?;
    file.as_file().sync_all().context("syncing state file")?;
    file.persist(path)
        .with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

fn parent_dir(path: &Path) -> PathBuf {
    path.parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn state_lock_excludes_concurrent_mutations_and_releases_on_drop() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let held = lock(&path).unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _next = lock(&path).unwrap();
            sender.send(()).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(held);
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
    }

    #[test]
    fn nonblocking_round_lock_rejects_an_active_runner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runner");
        let held = try_lock(&path).unwrap();
        let error = try_lock(&path).err().unwrap();
        assert_eq!(error.to_string(), "review runner is still active");
        drop(held);
        assert!(try_lock(&path).is_ok());
    }

    #[test]
    fn pr_operation_locks_serialize_only_the_same_request() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");
        let key = PrKey::new("owner/repo", 42).unwrap();
        let other = PrKey::new("owner/repo", 43).unwrap();
        let held = pr_lock(&path, &key).unwrap();
        let _other = pr_lock(&path, &other).unwrap();
        let (sender, receiver) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            let _next = pr_lock(&path, &key).unwrap();
            sender.send(()).unwrap();
        });
        assert!(receiver.recv_timeout(Duration::from_millis(100)).is_err());
        drop(held);
        receiver.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
    }
}
