//! Platform state and config paths plus local JSON state persistence.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use review_platform::{PrKey, State};
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tempfile::NamedTempFile;

/// Advisory process lock kept on a stable sibling file while state is read and saved.
pub struct StateLock {
    _file: File,
}

/// Whether a nonblocking probe found an operation holding its lock.
pub enum LockProbe {
    Held,
    Available,
}

#[cfg(windows)]
fn lock_is_busy(error: &std::io::Error) -> bool {
    // Windows reports exclusive-open contention as sharing or lock violations.
    matches!(error.raw_os_error(), Some(32 | 33))
}

pub fn lock(path: &Path) -> Result<StateLock> {
    blocking_lock(path, "another operation may be active")
}

fn blocking_lock(path: &Path, timeout_detail: &'static str) -> Result<StateLock> {
    #[cfg(unix)]
    let _ = timeout_detail;
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
        anyhow::bail!(
            "timed out after 10 seconds waiting for lock {}: {timeout_detail}",
            lock_path.display(),
        );
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
    blocking_lock(
        &pr_lock_path(state_path, key),
        "another operation on this PR is active",
    )
}

/// Refuse recovery promptly when another operation owns this PR.
pub fn try_pr_lock(state_path: &Path, key: &PrKey) -> Result<StateLock> {
    try_lock_with_message(
        &pr_lock_path(state_path, key),
        "another operation on this PR is active",
    )
}

fn pr_lock_path(state_path: &Path, key: &PrKey) -> PathBuf {
    let digest = Sha256::digest(key.to_string().as_bytes());
    let name = format!("pr-{:x}", digest);
    parent_dir(state_path).join("operations").join(name)
}

/// Nonblocking process lock used to keep recovery from stopping a live runner.
pub fn try_lock(path: &Path) -> Result<StateLock> {
    try_lock_with_message(path, "review runner is still active")
}

/// Allow a short-lived GUI probe to finish before refusing recovery.
pub fn try_recovery_lock(path: &Path) -> Result<StateLock> {
    let deadline = Instant::now() + Duration::from_millis(250);
    loop {
        if let Some(lock) = try_lock_once(path)? {
            return Ok(lock);
        }
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            anyhow::bail!("review runner is still active");
        }
        std::thread::sleep(remaining.min(Duration::from_millis(10)));
    }
}

/// Inspect an existing operation lock without creating its file or directory.
pub fn probe_lock(path: &Path) -> std::io::Result<LockProbe> {
    let mut lock_name = path.as_os_str().to_os_string();
    lock_name.push(".lock");
    let lock_path = PathBuf::from(lock_name);
    #[cfg(windows)]
    {
        // The default sharing mode allows concurrent GUI probes; a runner's
        // exclusive open still prevents access until it releases the lock.
        let mut options = OpenOptions::new();
        options.read(true);
        match options.open(lock_path) {
            Ok(_file) => Ok(LockProbe::Available),
            Err(error) if lock_is_busy(&error) => Ok(LockProbe::Held),
            Err(error) => Err(error),
        }
    }
    #[cfg(unix)]
    {
        let file = File::open(lock_path)?;
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockShared) {
            Ok(()) => Ok(LockProbe::Available),
            Err(error) => {
                let error = std::io::Error::from(error);
                if error.kind() == std::io::ErrorKind::WouldBlock {
                    Ok(LockProbe::Held)
                } else {
                    Err(error)
                }
            }
        }
    }
}

fn try_lock_with_message(path: &Path, busy_message: &'static str) -> Result<StateLock> {
    try_lock_once(path)?.ok_or_else(|| anyhow::anyhow!(busy_message))
}

fn try_lock_once(path: &Path) -> Result<Option<StateLock>> {
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
        match options.open(&lock_path) {
            Ok(file) => Ok(Some(StateLock { _file: file })),
            Err(error) if lock_is_busy(&error) => Ok(None),
            Err(error) => Err(error).with_context(|| format!("locking {}", lock_path.display())),
        }
    }
    #[cfg(unix)]
    {
        let file = options
            .open(&lock_path)
            .with_context(|| format!("opening lock {}", lock_path.display()))?;
        match rustix::fs::flock(&file, rustix::fs::FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => Ok(Some(StateLock { _file: file })),
            Err(error) if std::io::Error::from(error).kind() == std::io::ErrorKind::WouldBlock => {
                Ok(None)
            }
            Err(error) => Err(anyhow::Error::new(error))
                .with_context(|| format!("locking {}", lock_path.display())),
        }
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
    fn recovery_waits_for_a_short_lived_gui_probe() {
        let dir = tempfile::tempdir().unwrap();
        let checked = dir.path().join("checked");
        fs::write(dir.path().join("checked.lock"), b"").unwrap();
        assert!(matches!(
            probe_lock(&checked).unwrap(),
            LockProbe::Available
        ));
        assert!(try_recovery_lock(&checked).is_ok());

        let path = dir.path().join("runner");
        fs::write(dir.path().join("runner.lock"), b"").unwrap();
        // Hold the same kind of short-lived handle that a GUI probe acquires.
        let probe = File::open(dir.path().join("runner.lock")).unwrap();
        #[cfg(unix)]
        rustix::fs::flock(&probe, rustix::fs::FlockOperation::LockShared).unwrap();
        let (started, started_rx) = mpsc::channel();
        let (result, result_rx) = mpsc::channel();
        let worker = std::thread::spawn(move || {
            started.send(()).unwrap();
            result.send(try_recovery_lock(&path)).unwrap();
        });
        started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(result_rx.recv_timeout(Duration::from_millis(25)).is_err());
        drop(probe);
        assert!(
            result_rx
                .recv_timeout(Duration::from_secs(2))
                .unwrap()
                .is_ok()
        );
        worker.join().unwrap();
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
