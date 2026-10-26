//! State-file path selection and local JSON persistence for the operator CLI.

use anyhow::{Context, Result};
use directories::ProjectDirs;
use review_platform::State;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use tempfile::NamedTempFile;

pub fn resolve_path(override_path: Option<PathBuf>) -> Result<PathBuf> {
    let path = match override_path {
        Some(path) => path,
        None => default_path()?,
    };
    anyhow::ensure!(
        !path.as_os_str().is_empty(),
        "state file path cannot be empty"
    );
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .context("determining the current directory for --state-file")?
            .join(path))
    }
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
    let state: State = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing state file {}", path.display()))?;
    state
        .validate()
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
