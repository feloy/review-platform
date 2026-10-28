//! Build a disposable, non-executing checkout of the exact reviewed GitHub commit.

use crate::{HeadSha, PrKey};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum CheckoutError {
    #[error("creating checkout: {0}")]
    Create(#[source] std::io::Error),
    #[error("running git {step}: {source}")]
    Launch {
        step: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("git {0} failed while preparing the exact PR head and base")]
    Failed(&'static str),
    #[error("checkout HEAD differs from the inspected PR head")]
    WrongHead,
    #[error("the PR base commit is unavailable in the checkout")]
    MissingBase,
}

pub fn create_checkout(
    directory: &Path,
    key: &PrKey,
    head: &HeadSha,
    base: &HeadSha,
) -> Result<(), CheckoutError> {
    fs::create_dir(directory).map_err(CheckoutError::Create)?;
    let template = directory
        .parent()
        .ok_or(CheckoutError::WrongHead)?
        .join("empty-git-template");
    fs::create_dir(&template).map_err(CheckoutError::Create)?;
    let mut init = git(None);
    init.args(["init", "--quiet"]);
    init.arg(format!("--template={}", template.display()));
    init.arg(directory);
    require(init, "init")?;
    let remote = format!("https://github.com/{}.git", key.repository());
    let pr_ref = format!("refs/pull/{}/head", key.number());
    let mut fetch_head = git(Some(directory));
    fetch_head.args([
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-recurse-submodules",
        "--depth=1",
        &remote,
        &pr_ref,
    ]);
    require(fetch_head, "fetch head")?;
    let mut verify_fetched = git(Some(directory));
    verify_fetched.args(["rev-parse", "FETCH_HEAD"]);
    if String::from_utf8_lossy(&require(verify_fetched, "verify fetched head")?.stdout).trim()
        != head.as_str()
    {
        return Err(CheckoutError::WrongHead);
    }
    let mut fetch_base = git(Some(directory));
    fetch_base.args([
        "fetch",
        "--quiet",
        "--no-tags",
        "--no-recurse-submodules",
        "--depth=1",
        &remote,
        base.as_str(),
    ]);
    require(fetch_base, "fetch base")?;
    let mut verify_base = git(Some(directory));
    verify_base.args(["cat-file", "-t", base.as_str()]);
    if String::from_utf8_lossy(&require(verify_base, "verify base")?.stdout).trim() != "commit" {
        return Err(CheckoutError::MissingBase);
    }
    let mut checkout = git(Some(directory));
    checkout.args(["checkout", "--quiet", "--detach", "--force", head.as_str()]);
    require(checkout, "checkout")?;
    let mut verify_head = git(Some(directory));
    verify_head.args(["rev-parse", "HEAD"]);
    if String::from_utf8_lossy(&require(verify_head, "verify checkout")?.stdout).trim()
        != head.as_str()
    {
        return Err(CheckoutError::WrongHead);
    }
    Ok(())
}

fn git(directory: Option<&Path>) -> Command {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os() {
        if key.to_string_lossy().starts_with("GIT_") {
            command.env_remove(key);
        }
    }
    command
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env(
            "GIT_CONFIG_GLOBAL",
            if cfg!(windows) { "NUL" } else { "/dev/null" },
        )
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_LFS_SKIP_SMUDGE", "1")
        .env("GIT_ALLOW_PROTOCOL", "https");
    if let Some(directory) = directory {
        command.arg("-C").arg(directory);
    }
    command.args([
        "-c",
        "credential.helper=",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "submodule.recurse=false",
        "-c",
        "core.pager=cat",
        "-c",
        "diff.external=",
        "-c",
        "core.fsmonitor=false",
    ]);
    command
}

fn require(mut command: Command, step: &'static str) -> Result<Output, CheckoutError> {
    let output = command
        .output()
        .map_err(|source| CheckoutError::Launch { step, source })?;
    if !output.status.success() {
        return Err(CheckoutError::Failed(step));
    }
    Ok(output)
}
