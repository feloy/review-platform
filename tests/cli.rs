//! CLI integration tests for persisted decisions and platform file paths.

use review_platform::{HeadSha, ObservedDiscussion, PrKey, State};
use std::num::NonZeroU64;
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

fn observe_empty_discussion(state_file: &Path, head: &str) -> u64 {
    let mut state: State = serde_json::from_slice(&std::fs::read(state_file).unwrap()).unwrap();
    let key = PrKey::new("owner/repo", 42).unwrap();
    let head = HeadSha::new(head).unwrap();
    let discussion = ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap();
    state.refresh(&key, &head, &discussion).unwrap();
    let revision = state.get(&key).unwrap().revision().get();
    std::fs::write(state_file, serde_json::to_vec_pretty(&state).unwrap()).unwrap();
    revision
}

#[test]
fn config_path_uses_platform_config_independently_of_state_override() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");
    let expected = directories::ProjectDirs::from("", "", "review-platform")
        .unwrap()
        .config_dir()
        .join("config.json");
    let printed = run(&state_file, &["config-path"]);
    assert_eq!(printed.trim(), expected.to_str().unwrap());
    assert!(!state_file.exists());

    let override_path = dir.path().join("operator-config.json");
    let output = Command::new(env!("CARGO_BIN_EXE_review-platform"))
        .args([
            "--config-file",
            override_path.to_str().unwrap(),
            "config-path",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert_eq!(
        String::from_utf8(output.stdout).unwrap().trim(),
        override_path.to_str().unwrap()
    );
    assert!(!override_path.exists());
}

#[test]
fn local_authorization_survives_process_restart_and_new_head_clears_one_round() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");

    run(&state_file, &["request", "owner/repo", "42", HEAD_A]);
    assert_eq!(observe_empty_discussion(&state_file, HEAD_A), 2);
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
            "--revision",
            "2",
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
    assert!(status.contains("revision: 1"));
    assert!(status.contains("author discussion: not yet observed"));

    let error = run_failing(
        &state_file,
        &["authorize", "owner/repo", "42", "--scope", "one-round"],
    );
    assert!(error.contains("--head"), "{error}");
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
    assert!(error.contains("--revision"), "{error}");
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
            "--revision",
            "0",
        ],
    );
    assert!(
        error.contains("revision must be greater than zero"),
        "{error}"
    );
    let before = std::fs::read(&state_file).unwrap();
    for scope in ["one-round", "pr-cycle"] {
        let error = run_failing(
            &state_file,
            &[
                "authorize",
                "owner/repo",
                "42",
                "--scope",
                scope,
                "--head",
                HEAD_A,
                "--revision",
                "1",
            ],
        );
        assert!(
            error.contains("author discussion has not been observed"),
            "{error}"
        );
    }
    assert_eq!(std::fs::read(&state_file).unwrap(), before);

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
            "--revision",
            "1",
        ],
    );
    assert!(error.contains(HEAD_A), "{error}");
    assert!(error.contains(HEAD_B), "{error}");
    let status = run(&state_file, &["status", "owner/repo", "42"]);
    assert!(status.contains(HEAD_B));
    assert!(status.contains("awaiting lead authorization"));
    assert!(status.contains("revision: 2"));

    let error = run_failing(
        &state_file,
        &[
            "authorize",
            "owner/repo",
            "42",
            "--scope",
            "one-round",
            "--head",
            HEAD_B,
            "--revision",
            "2",
        ],
    );
    assert!(
        error.contains("author discussion has not been observed"),
        "{error}"
    );
    assert_eq!(observe_empty_discussion(&state_file, HEAD_B), 3);
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
            "--revision",
            "3",
        ],
    );
    assert!(run(&state_file, &["status", "owner/repo", "42"]).contains("authorized for one round"));
    run(&state_file, &["revoke", "owner/repo", "42"]);
    assert!(
        run(&state_file, &["status", "owner/repo", "42"]).contains("awaiting lead authorization")
    );
}

#[test]
fn authorize_rejects_an_old_revision_even_when_the_head_returns() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");
    run(&state_file, &["request", "owner/repo", "42", HEAD_A]);
    run(&state_file, &["request", "owner/repo", "42", HEAD_B]);
    run(&state_file, &["request", "owner/repo", "42", HEAD_A]);

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
            "--revision",
            "1",
        ],
    );
    assert!(error.contains("input revision 1"), "{error}");
    assert!(error.contains("recorded revision 3"), "{error}");
    assert!(
        run(&state_file, &["status", "owner/repo", "42"]).contains("awaiting lead authorization")
    );
}

#[test]
fn refresh_requires_a_request_and_failed_fetch_preserves_the_state_file() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");
    let missing = run_failing(&state_file, &["refresh", "owner/repo", "42"]);
    assert!(missing.contains("review request not found"), "{missing}");
    assert!(!state_file.exists());

    run(&state_file, &["request", "owner/repo", "42", HEAD_A]);
    let before = std::fs::read(&state_file).unwrap();
    let secret = "secret\nwith-newline";
    let output = Command::new(env!("CARGO_BIN_EXE_review-platform"))
        .arg("--state-file")
        .arg(&state_file)
        .args(["refresh", "owner/repo", "42"])
        .env("REVIEW_PLATFORM_GITHUB_TOKEN_RO", secret)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("invalid header characters"), "{stderr}");
    assert!(!stderr.contains(secret));
    assert_eq!(std::fs::read(&state_file).unwrap(), before);
}

#[test]
fn new_state_uses_current_schema_and_older_schema_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");
    run(&state_file, &["request", "owner/repo", "42", HEAD_A]);
    let saved: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    assert_eq!(saved["schema_version"], 5);
    assert_eq!(saved["requests"][0]["revision"], 1);
    assert!(saved["requests"][0]["discussion"].is_null());
    assert!(saved["requests"][0]["model_validation"].is_null());
    let mut older = saved;
    older["schema_version"] = 4.into();
    std::fs::write(&state_file, serde_json::to_vec(&older).unwrap()).unwrap();
    let error = run_failing(&state_file, &["status", "owner/repo", "42"]);
    assert!(
        error.contains("unsupported state schema version: 4"),
        "{error}"
    );
}

#[test]
fn status_reports_observed_discussion_counts_without_comment_text() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");
    let hash = "a".repeat(64);
    let state = serde_json::json!({
        "schema_version": 5,
        "requests": [{
            "key": {"repository": "owner/repo", "number": 42},
            "head_sha": HEAD_A,
            "authorization": "awaiting_lead",
            "revision": 4,
            "discussion": {
                "author_id": 7,
                "comments": [
                    {"source": "issue_comment", "id": 1, "sha256": hash},
                    {"source": "review_comment", "id": 2, "sha256": hash},
                    {"source": "review", "id": 3, "sha256": hash}
                ]
            },
            "model_validation": null
        }]
    });
    std::fs::write(&state_file, serde_json::to_vec(&state).unwrap()).unwrap();
    let status = run(&state_file, &["status", "owner/repo", "42"]);
    assert!(status.contains("revision: 4"));
    assert!(status.contains("1 issue comments, 1 review comments, 1 review bodies"));
    assert!(!status.contains(&hash));
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
            "--revision",
            "1",
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
