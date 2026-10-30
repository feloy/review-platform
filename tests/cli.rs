//! CLI integration tests for persisted decisions and platform file paths.

use review_platform::publication::{PublicationHash, PublicationReceipt, SealedBody};
use review_platform::review::RoundId;
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

fn show_draft_offline(state_file: &Path) -> String {
    let output = Command::new(env!("CARGO_BIN_EXE_review-platform"))
        .arg("--state-file")
        .arg(state_file)
        .args(["show-draft", "owner/repo", "42", "--round", "r-draft"])
        .env("REVIEW_PLATFORM_GITHUB_TOKEN_RO", "bad\nheader")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "show-draft failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout).unwrap()
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
fn reject_draft_preserves_the_artifact_and_later_authorization() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");
    run(&state_file, &["request", "owner/repo", "42", HEAD_A]);
    assert_eq!(observe_empty_discussion(&state_file, HEAD_A), 2);
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    state["requests"][0]["authorization"] = "pr_cycle".into();
    let discussion = state["requests"][0]["discussion"].clone();
    state["rounds"] = serde_json::json!([{
        "id":"r-draft",
        "key":{"repository":"owner/repo","number":42},
        "status":"draft_available",
        "sandbox":"rp-722d6472616674",
        "head_sha":HEAD_A,
        "revision":2,
        "discussion":discussion,
        "model":"model-alpha",
        "agent":"claude",
        "gateway":"gateway",
        "workspace":"default",
        "driver":"vm",
        "model_provider":"model-provider",
        "github_provider":"github-provider",
        "prompt_sha256":"a".repeat(64),
        "format_sha256":"b".repeat(64),
        "archive_sha256":"c".repeat(64),
        "archive_config_sha256":"d".repeat(64),
        "execution_sha256":"e".repeat(64),
        "runner_sha256":"f".repeat(64),
        "created_at":1,
        "dispatched_at":2,
        "finished_at":3,
        "freshness":"fresh",
        "failure":null,
        "incomplete_log":false,
        "cleanup_pending":false
    }]);
    std::fs::write(&state_file, serde_json::to_vec(&state).unwrap()).unwrap();
    let round_dir = dir.path().join("rounds/r-draft");
    std::fs::create_dir_all(&round_dir).unwrap();
    let draft_file = round_dir.join("draft.json");
    let draft =
        br#"{"status":"REQUEST_CHANGES","comments":"Finding","lead_advice":"","review_log":""}"#;
    std::fs::write(&draft_file, draft).unwrap();
    let agent_file = round_dir.join("output/review-output.json");
    std::fs::create_dir_all(agent_file.parent().unwrap()).unwrap();
    let original = br#"{"status":"REQUEST_CHANGES","comments":"Finding","lead_advice":""}"#;
    std::fs::write(&agent_file, original).unwrap();
    let body_hash = SealedBody::new("Finding", "r-draft", PublicationHash::of(original))
        .hash()
        .to_string();
    let displayed = show_draft_offline(&state_file);
    assert!(displayed.contains("Finding\n\n_Agent review via Review Platform (round r-draft)._"));
    assert!(displayed.contains(&format!("--body-hash {body_hash}")));
    assert!(displayed.contains("Publish command:"));
    assert!(displayed.contains("freshness: unknown"));

    let published_file = dir.path().join("published-state.json");
    let mut published_state: State =
        serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    let round_id = RoundId::new("r-draft").unwrap();
    let hash = PublicationHash::new(&body_hash).unwrap();
    let attempt = published_state
        .reserve_publication(
            &PrKey::new("owner/repo", 42).unwrap(),
            &round_id,
            &hash,
            "uid:1".into(),
            "publisher".into(),
            NonZeroU64::new(7).unwrap(),
            vec![],
            4,
        )
        .unwrap();
    published_state
        .mark_publication_dispatched(&round_id, attempt, 5)
        .unwrap();
    let uncertain_file = dir.path().join("uncertain-state.json");
    std::fs::write(
        &uncertain_file,
        serde_json::to_vec(&published_state).unwrap(),
    )
    .unwrap();
    let publishing = show_draft_offline(&uncertain_file);
    assert!(!publishing.contains("Publish command:"));
    assert!(publishing.contains("Recovery command:"));
    let mut unknown_state = published_state.clone();
    unknown_state
        .finish_publication_attempt(
            &round_id,
            attempt,
            review_platform::publication::AttemptOutcome::Unknown,
            6,
        )
        .unwrap();
    let unknown_file = dir.path().join("unknown-state.json");
    std::fs::write(&unknown_file, serde_json::to_vec(&unknown_state).unwrap()).unwrap();
    let unknown = show_draft_offline(&unknown_file);
    assert!(!unknown.contains("Publish command:"));
    assert!(unknown.contains("Recovery command:"));
    let recovered = run(
        &uncertain_file,
        &[
            "recover-publication",
            "owner/repo",
            "42",
            "--round",
            "r-draft",
            "--confirm-not-posted",
            "1",
        ],
    );
    assert!(recovered.contains("confirmed attempt 1 was not posted"));
    let recovered: State =
        serde_json::from_slice(&std::fs::read(&uncertain_file).unwrap()).unwrap();
    recovered.validate().unwrap();
    assert_eq!(
        recovered.round(&round_id).unwrap().status(),
        review_platform::review::RoundStatus::AwaitingDecision
    );
    assert!(show_draft_offline(&uncertain_file).contains("Publish command:"));

    published_state
        .record_publication(
            &round_id,
            attempt,
            PublicationReceipt::new(
                NonZeroU64::new(71).unwrap(),
                "https://github.com/owner/repo/pull/42#pullrequestreview-71".into(),
                "publisher".into(),
                NonZeroU64::new(7).unwrap(),
                HeadSha::new(HEAD_A).unwrap(),
                "2026-10-30T10:00:00Z".into(),
            ),
            6,
        )
        .unwrap();
    std::fs::write(
        &published_file,
        serde_json::to_vec(&published_state).unwrap(),
    )
    .unwrap();
    assert!(!show_draft_offline(&published_file).contains("Publish command:"));
    let repeated = run(
        &published_file,
        &[
            "publish-draft",
            "owner/repo",
            "42",
            "--round",
            "r-draft",
            "--body-hash",
            &body_hash,
        ],
    );
    assert!(repeated.contains("GitHub review 71"));

    let wrong_hash = "0".repeat(64);
    let error = run_failing(
        &state_file,
        &[
            "reject-draft",
            "owner/repo",
            "42",
            "--round",
            "r-draft",
            "--body-hash",
            &wrong_hash,
        ],
    );
    assert!(error.contains("supplied body hash does not match"));

    let config_file = dir.path().join("config.json");
    let config = serde_json::json!({
        "schema_version": 5,
        "publisher_login": "publisher",
        "execution": {
            "openshell": std::env::current_exe().unwrap(),
            "gateway": "gateway",
            "workspace": "default",
            "driver": "vm",
            "github_provider": "github-readonly"
        },
        "models": [],
        "agents": [],
        "repositories": []
    });
    std::fs::write(&config_file, serde_json::to_vec(&config).unwrap()).unwrap();
    let missing_token = Command::new(env!("CARGO_BIN_EXE_review-platform"))
        .arg("--state-file")
        .arg(&state_file)
        .arg("--config-file")
        .arg(&config_file)
        .args([
            "publish-draft",
            "owner/repo",
            "42",
            "--round",
            "r-draft",
            "--body-hash",
            &body_hash,
        ])
        .env_remove("REVIEW_PLATFORM_GITHUB_TOKEN_RW")
        .output()
        .unwrap();
    assert!(!missing_token.status.success());
    assert!(
        String::from_utf8(missing_token.stderr)
            .unwrap()
            .contains("REVIEW_PLATFORM_GITHUB_TOKEN_RW")
    );
    assert_eq!(std::fs::read(&draft_file).unwrap(), draft);
    assert_eq!(std::fs::read(&agent_file).unwrap(), original);

    std::fs::write(
        &agent_file,
        br#"{"status":"REQUEST_CHANGES","comments":"Altered","lead_advice":""}"#,
    )
    .unwrap();
    let changed_artifact = run_failing(
        &state_file,
        &[
            "reject-draft",
            "owner/repo",
            "42",
            "--round",
            "r-draft",
            "--body-hash",
            &body_hash,
        ],
    );
    assert!(changed_artifact.contains("saved draft differs from the original agent artifact"));
    std::fs::write(&agent_file, original).unwrap();

    let output = run(
        &state_file,
        &[
            "reject-draft",
            "owner/repo",
            "42",
            "--round",
            "r-draft",
            "--body-hash",
            &body_hash,
        ],
    );
    assert!(output.contains("rejected locally"));
    assert!(output.contains("current authorization is preserved"));
    let status = run(&state_file, &["status", "owner/repo", "42"]);
    assert!(status.contains("latest round: r-draft (rejected)"));
    assert!(status.contains("authorized for the PR cycle"));
    assert!(!show_draft_offline(&state_file).contains("Publish command:"));
    assert_eq!(std::fs::read(&draft_file).unwrap(), draft);
    let stored: State = serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    stored.validate().unwrap();

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
    let before_repeat = std::fs::read(&state_file).unwrap();
    let repeated = run(
        &state_file,
        &[
            "reject-draft",
            "owner/repo",
            "42",
            "--round",
            "r-draft",
            "--body-hash",
            &body_hash,
        ],
    );
    assert!(repeated.contains("already rejected"));
    assert_eq!(std::fs::read(&state_file).unwrap(), before_repeat);
    assert!(run(&state_file, &["status", "owner/repo", "42"]).contains("authorized for one round"));
}

#[test]
fn oversized_draft_stays_sealed_and_rejectable_without_a_publish_command() {
    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");
    run(&state_file, &["request", "owner/repo", "42", HEAD_A]);
    assert_eq!(observe_empty_discussion(&state_file, HEAD_A), 2);
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    let discussion = state["requests"][0]["discussion"].clone();
    state["rounds"] = serde_json::json!([{
        "id":"r-draft",
        "key":{"repository":"owner/repo","number":42},
        "status":"draft_available",
        "sandbox":"rp-722d6472616674",
        "head_sha":HEAD_A,
        "revision":2,
        "discussion":discussion,
        "model":"model-alpha",
        "agent":"claude",
        "gateway":"gateway",
        "workspace":"default",
        "driver":"vm",
        "model_provider":"model-provider",
        "github_provider":"github-provider",
        "prompt_sha256":"a".repeat(64),
        "format_sha256":"b".repeat(64),
        "archive_sha256":"c".repeat(64),
        "archive_config_sha256":"d".repeat(64),
        "execution_sha256":"e".repeat(64),
        "runner_sha256":"f".repeat(64),
        "created_at":1,
        "dispatched_at":2,
        "finished_at":3,
        "freshness":"fresh",
        "failure":null,
        "incomplete_log":false,
        "cleanup_pending":false
    }]);
    std::fs::write(&state_file, serde_json::to_vec(&state).unwrap()).unwrap();

    let comments = "é".repeat(65_536);
    let agent = serde_json::to_vec(&serde_json::json!({
        "status":"REQUEST_CHANGES",
        "comments":comments,
        "lead_advice":""
    }))
    .unwrap();
    let draft = serde_json::to_vec(&serde_json::json!({
        "status":"REQUEST_CHANGES",
        "comments":comments,
        "lead_advice":"",
        "review_log":""
    }))
    .unwrap();
    let round_dir = dir.path().join("rounds/r-draft");
    std::fs::create_dir_all(round_dir.join("output")).unwrap();
    std::fs::write(round_dir.join("draft.json"), &draft).unwrap();
    std::fs::write(round_dir.join("output/review-output.json"), &agent).unwrap();
    let seal = SealedBody::new(&comments, "r-draft", PublicationHash::of(&agent));
    let output = show_draft_offline(&state_file);
    assert!(output.contains("Publication unavailable: GitHub review body has"));
    assert!(!output.contains("Publish command:"));
    assert!(output.contains(&format!("publication body SHA-256: {}", seal.hash())));

    let mut stored: State = serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    stored.validate().unwrap();
    let round_id = RoundId::new("r-draft").unwrap();
    assert!(stored.round(&round_id).unwrap().sealed_body().is_some());
    assert!(matches!(
        stored.reserve_publication(
            &PrKey::new("owner/repo", 42).unwrap(),
            &round_id,
            seal.hash(),
            "uid:1".into(),
            "publisher".into(),
            NonZeroU64::new(7).unwrap(),
            vec![],
            4
        ),
        Err(review_platform::review::RoundStateError::Publication(
            review_platform::publication::PublicationStateError::BodyTooLong { .. }
        ))
    ));
    let error = run_failing(
        &state_file,
        &[
            "publish-draft",
            "owner/repo",
            "42",
            "--round",
            "r-draft",
            "--body-hash",
            seal.hash().as_str(),
        ],
    );
    assert!(error.contains("GitHub review body has"));
    assert!(!error.contains("config"));
    let rejected = run(
        &state_file,
        &[
            "reject-draft",
            "owner/repo",
            "42",
            "--round",
            "r-draft",
            "--body-hash",
            seal.hash().as_str(),
        ],
    );
    assert!(rejected.contains("rejected locally"));
    assert!(!show_draft_offline(&state_file).contains("Publish command:"));
    assert_eq!(std::fs::read(round_dir.join("draft.json")).unwrap(), draft);
}

#[cfg(unix)]
#[test]
fn recovery_preserves_a_saved_draft_when_sandbox_stop_fails() {
    use review_platform::review::{RoundId, RoundStatus, read_saved_draft};
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let state_file = dir.path().join("state.json");
    let config_file = dir.path().join("config.json");
    let fake_openshell = dir.path().join("openshell");
    std::fs::write(
        &fake_openshell,
        b"#!/bin/sh\ncase \" $* \" in\n  *\" sandbox stop \"*) exit 1;;\n  *\" sandbox delete \"*) exit 0;;\nesac\nexit 1\n",
    )
    .unwrap();
    std::fs::set_permissions(&fake_openshell, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = serde_json::json!({
        "schema_version": 5,
        "execution": {
            "openshell": fake_openshell,
            "gateway": "test-gateway",
            "workspace": "default",
            "driver": "vm",
            "github_provider": "test-github-readonly"
        },
        "models": [],
        "agents": [],
        "repositories": []
    });
    std::fs::write(&config_file, serde_json::to_vec(&config).unwrap()).unwrap();
    let state = serde_json::json!({
        "schema_version": 5,
        "requests": [{
            "key": {"repository": "owner/repo", "number": 42},
            "head_sha": HEAD_A,
            "authorization": "awaiting_lead",
            "revision": 2,
            "discussion": {"author_id": 7, "comments": []},
            "model_validation": null
        }],
        "rounds": [{
            "id": "r-draft",
            "key": {"repository": "owner/repo", "number": 42},
            "status": "draft_available",
            "sandbox": "rp-722d6472616674",
            "head_sha": HEAD_A,
            "revision": 2,
            "discussion": {"author_id": 7, "comments": []},
            "model": "model-alpha",
            "agent": "claude",
            "gateway": "test-gateway",
            "workspace": "default",
            "driver": "vm",
            "model_provider": "model-provider",
            "github_provider": "test-github-readonly",
            "prompt_sha256": "a".repeat(64),
            "format_sha256": "b".repeat(64),
            "archive_sha256": "c".repeat(64),
            "archive_config_sha256": "d".repeat(64),
            "execution_sha256": "e".repeat(64),
            "runner_sha256": "f".repeat(64),
            "created_at": 1,
            "dispatched_at": 2,
            "finished_at": 3,
            "freshness": "fresh",
            "failure": null,
            "incomplete_log": false,
            "cleanup_pending": true
        }]
    });
    std::fs::write(&state_file, serde_json::to_vec(&state).unwrap()).unwrap();
    let round_dir = dir.path().join("rounds/r-draft");
    std::fs::create_dir_all(&round_dir).unwrap();
    let draft_file = round_dir.join("draft.json");
    std::fs::write(
        &draft_file,
        br#"{"status":"REQUEST_CHANGES","comments":"Saved finding","lead_advice":"","review_log":""}"#,
    )
    .unwrap();

    let result = Command::new(env!("CARGO_BIN_EXE_review-platform"))
        .args(["--state-file", state_file.to_str().unwrap()])
        .args(["--config-file", config_file.to_str().unwrap()])
        .args(["recover-review", "owner/repo", "42"])
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "recovery failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stored: State = serde_json::from_slice(&std::fs::read(&state_file).unwrap()).unwrap();
    stored.validate().unwrap();
    let round = stored.round(&RoundId::new("r-draft").unwrap()).unwrap();
    assert_eq!(round.status(), RoundStatus::AwaitingDecision);
    assert!(!round.cleanup_pending());
    assert_eq!(
        read_saved_draft(&draft_file).unwrap().comments(),
        "Saved finding"
    );
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
    assert_eq!(saved["schema_version"], 6);
    assert_eq!(saved["requests"][0]["revision"], 1);
    assert!(saved["requests"][0]["discussion"].is_null());
    assert!(saved["requests"][0]["model_validation"].is_null());
    let mut older = saved;
    older["schema_version"] = 2.into();
    std::fs::write(&state_file, serde_json::to_vec(&older).unwrap()).unwrap();
    let error = run_failing(&state_file, &["status", "owner/repo", "42"]);
    assert!(
        error.contains("unsupported state schema version: 2"),
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
        "rounds": [],
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
