//! CLI integration tests for persisted authorization and state-file paths.

use std::path::Path;
use std::process::{Command, Output};

const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

fn run_output(state_file: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_review-platform"))
        .arg("--state-file")
        .arg(state_file)
        .args(args)
        .output()
        .unwrap()
}

fn run(state_file: &Path, args: &[&str]) -> String {
    let output = run_output(state_file, args);
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
}

fn run_failing(state_file: &Path, args: &[&str]) -> String {
    let output = run_output(state_file, args);
    assert!(
        !output.status.success(),
        "command unexpectedly succeeded: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    String::from_utf8(output.stderr).unwrap()
}

#[test]
fn local_authorization_survives_process_restart_and_new_head_clears_one_round() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");

    run(&state_file, &["request", "owner/repo", "42", HEAD_A]);
    run(
        &state_file,
        &[
            "authorize",
            "owner/repo",
            "42",
            "--scope",
            "one-round",
            "--head",
            HEAD_A,
        ],
    );
    let status = run(&state_file, &["status", "owner/repo", "42"]);
    assert!(status.contains("authorized for one round"));
    assert!(status.contains(HEAD_A));

    run(&state_file, &["request", "owner/repo", "42", HEAD_B]);
    let status = run(&state_file, &["status", "owner/repo", "42"]);
    assert!(status.contains("awaiting lead authorization"));
    assert!(status.contains(HEAD_B));
}

#[test]
fn authorize_requires_the_head_seen_by_the_lead() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");

    run(&state_file, &["request", "owner/repo", "42", HEAD_A]);
    let status = run(&state_file, &["status", "owner/repo", "42"]);
    assert!(status.contains(HEAD_A));

    let error = run_failing(
        &state_file,
        &["authorize", "owner/repo", "42", "--scope", "one-round"],
    );
    assert!(error.contains("--head"), "{error}");

    run(&state_file, &["request", "owner/repo", "42", HEAD_B]);
    let error = run_failing(
        &state_file,
        &[
            "authorize",
            "owner/repo",
            "42",
            "--scope",
            "one-round",
            "--head",
            HEAD_A,
        ],
    );
    assert!(error.contains(HEAD_A), "{error}");
    assert!(error.contains(HEAD_B), "{error}");
    let status = run(&state_file, &["status", "owner/repo", "42"]);
    assert!(status.contains(HEAD_B));
    assert!(status.contains("awaiting lead authorization"));

    run(
        &state_file,
        &[
            "authorize",
            "owner/repo",
            "42",
            "--scope",
            "one-round",
            "--head",
            HEAD_B,
        ],
    );
    assert!(run(&state_file, &["status", "owner/repo", "42"]).contains("authorized for one round"));
    run(&state_file, &["revoke", "owner/repo", "42"]);
    assert!(
        run(&state_file, &["status", "owner/repo", "42"]).contains("awaiting lead authorization")
    );
}

#[test]
fn authorize_reports_missing_request() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");

    let error = run_failing(
        &state_file,
        &[
            "authorize",
            "owner/repo",
            "42",
            "--scope",
            "one-round",
            "--head",
            HEAD_A,
        ],
    );
    assert!(
        error.contains("review request not found: owner/repo#42"),
        "{error}"
    );
}

#[test]
fn state_path_resolves_relative_override_without_reading_state() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("broken.json");
    std::fs::write(&state_file, "not JSON").unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_review-platform"))
        .current_dir(dir.path())
        .args(["--state-file", "broken.json", "state-path"])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let printed_path = String::from_utf8(output.stdout).unwrap();
    let printed_path = Path::new(printed_path.trim_end());
    assert!(printed_path.is_absolute());
    assert_eq!(
        std::fs::canonicalize(printed_path).unwrap(),
        std::fs::canonicalize(&state_file).unwrap()
    );
    assert_eq!(std::fs::read_to_string(&state_file).unwrap(), "not JSON");

    let missing_file = dir.path().join("missing.json");
    assert_eq!(
        run(&missing_file, &["state-path"]),
        format!("{}\n", missing_file.display())
    );
    assert!(!missing_file.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn default_state_file_uses_xdg_state_home() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("review-platform/state.json");
    let path_output = Command::new(env!("CARGO_BIN_EXE_review-platform"))
        .env("XDG_STATE_HOME", dir.path())
        .arg("state-path")
        .output()
        .unwrap();
    assert!(path_output.status.success());
    assert_eq!(
        String::from_utf8(path_output.stdout).unwrap(),
        format!("{}\n", state_file.display())
    );
    assert!(!state_file.exists());

    let output = Command::new(env!("CARGO_BIN_EXE_review-platform"))
        .env("XDG_STATE_HOME", dir.path())
        .args(["request", "owner/repo", "42", HEAD_A])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let saved_state = std::fs::read_to_string(state_file).unwrap();
    assert!(saved_state.contains(HEAD_A));
}
