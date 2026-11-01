//! Authenticated private draft responses, without exposing logs or adapter errors.

use super::{ApiError, AppState, api_error, has_session};
use crate::draft_commands::{DraftError, InspectedDraft};
use axum::{
    Json,
    extract::{Path as UrlPath, Query, State as HttpState},
    http::{HeaderMap, StatusCode},
};
use review_platform::{
    HeadSha, InputRevision, PrKey,
    config::{AgentId, ModelId},
    publication::PublicationHash,
    review::{Freshness, ReviewStatus, RoundId, RoundStatus},
};
use serde::Serialize;
use std::{path::Path, sync::Arc};

pub(super) type Inspect = fn(&Path, &PrKey, &RoundId) -> Result<InspectedDraft, DraftError>;

#[derive(Serialize)]
pub(super) struct DraftResponse {
    key: PrKey,
    round_id: RoundId,
    head_sha: HeadSha,
    revision: InputRevision,
    decision: RoundStatus,
    model: ModelId,
    agent: AgentId,
    verdict: ReviewStatus,
    freshness: Freshness,
    publication_body: String,
    publication_body_sha256: PublicationHash,
    lead_advice: String,
}

pub(super) async fn inspect(
    HttpState(state): HttpState<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<RoundId>,
    Query(key): Query<PrKey>,
) -> Result<Json<DraftResponse>, (StatusCode, Json<ApiError>)> {
    if !has_session(&state, &headers) {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "Sign in again to view private drafts.",
        ));
    }
    let inspected =
        tokio::task::spawn_blocking(move || (state.inspect_draft)(&state.state_file, &key, &id))
            .await
            .map_err(|_| {
                api_error(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Could not inspect the private draft.",
                )
            })?
            .map_err(draft_error)?;
    let record = inspected.record();
    Ok(Json(DraftResponse {
        key: record.key().clone(),
        round_id: record.id().clone(),
        head_sha: record.head_sha().clone(),
        revision: record.revision(),
        decision: record.status(),
        model: record.model().clone(),
        agent: record.agent().clone(),
        verdict: inspected.draft().status(),
        freshness: inspected.freshness(),
        publication_body: inspected.seal().body().to_owned(),
        publication_body_sha256: inspected.seal().hash().clone(),
        lead_advice: inspected.draft().lead_advice().to_owned(),
    }))
}

fn draft_error(error: DraftError) -> (StatusCode, Json<ApiError>) {
    match error {
        DraftError::NotFound(_) | DraftError::WrongRequest(_) => api_error(
            StatusCode::NOT_FOUND,
            "Private draft not found for this PR and round.",
        ),
        DraftError::Unavailable(_) => api_error(
            StatusCode::CONFLICT,
            "This round has no completed private draft.",
        ),
        DraftError::Artifact(_) | DraftError::ArtifactMismatch | DraftError::SealChanged => {
            api_error(
                StatusCode::UNPROCESSABLE_ENTITY,
                "Could not verify the private draft against its original output and publication seal. Check the saved round files.",
            )
        }
        DraftError::Storage(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not read local review state.",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Session, prepare_for_gui, router_with_state};
    use super::*;
    use crate::{draft_commands, gui_runs::Runs, request_commands, review_commands, storage};
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::{Request, header},
    };
    use futures_util::StreamExt;
    use review_platform::{State, publication::SealedBody};
    use std::{
        fs,
        path::PathBuf,
        sync::{Mutex, atomic::AtomicBool},
        time::Duration,
    };
    use tower::ServiceExt;

    fn runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn offline_inspect(
        path: &Path,
        key: &PrKey,
        id: &RoundId,
    ) -> Result<InspectedDraft, DraftError> {
        draft_commands::inspect_with_freshness(path, key, id, |record, excluded| {
            assert_eq!(record.key(), key);
            assert!(excluded.is_empty());
            Freshness::Fresh
        })
    }

    fn unknown_freshness(
        path: &Path,
        key: &PrKey,
        id: &RoundId,
    ) -> Result<InspectedDraft, DraftError> {
        draft_commands::inspect_with_freshness(path, key, id, |_, _| Freshness::Unknown)
    }

    fn app(path: PathBuf, bootstrap: Option<String>, inspect_draft: Inspect) -> Router {
        router_with_state(Arc::new(AppState {
            state_file: path,
            config_file: PathBuf::from("unused-config.json"),
            session: Mutex::new(Session {
                bootstrap,
                token: "test-token".into(),
            }),
            refresh: request_commands::refresh,
            prepare: prepare_for_gui,
            runs: Arc::new(Runs::new(
                Arc::new(AtomicBool::new(false)),
                review_commands::run_inspected_review,
            )),
            inspect_draft,
        }))
    }

    fn get(uri: &str, bearer: Option<&str>) -> Request<Body> {
        let mut request = Request::builder()
            .uri(uri)
            .header(header::HOST, "localhost:8080");
        if let Some(token) = bearer {
            request = request.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        request.body(Body::empty()).unwrap()
    }

    const URI: &str = "/api/drafts/r-draft?repository=owner%2Frepo&number=15";
    const COMMENTS: &str = "Finding\n<script>alert('draft')</script>";
    const ADVICE: &str = "Private lead advice";

    fn fixture(directory: &Path) -> PathBuf {
        let state_file = directory.join("state.json");
        let round_dir = directory.join("rounds/r-draft");
        fs::create_dir_all(round_dir.join("output")).unwrap();
        let artifact = serde_json::to_vec(&serde_json::json!({
            "status":"ACCEPT_WITH_NITS", "comments":COMMENTS, "lead_advice":ADVICE,
        }))
        .unwrap();
        fs::write(round_dir.join("output/review-output.json"), &artifact).unwrap();
        fs::write(
            round_dir.join("draft.json"),
            serde_json::to_vec(&serde_json::json!({
                "status":"ACCEPT_WITH_NITS", "comments":COMMENTS, "lead_advice":ADVICE,
                "review_log":"Private full review log",
            }))
            .unwrap(),
        )
        .unwrap();
        let state = serde_json::json!({
            "schema_version":6,
            "requests":[{"key":{"repository":"owner/repo","number":15}, "head_sha":"a".repeat(40),
                "authorization":"awaiting_lead", "revision":2, "discussion":{"author_id":7,"comments":[]}, "model_validation":null}],
            "rounds":[{"id":"r-draft", "key":{"repository":"owner/repo","number":15}, "status":"awaiting_decision",
                "sandbox":"rp-722d6472616674", "head_sha":"a".repeat(40), "revision":2,
                "discussion":{"author_id":7,"comments":[]}, "model":"model-alpha", "agent":"claude",
                "gateway":"gateway", "workspace":"default", "driver":"vm", "model_provider":"model-provider",
                "github_provider":"github-ro", "prompt_sha256":"a".repeat(64), "format_sha256":"b".repeat(64),
                "archive_sha256":"c".repeat(64), "archive_config_sha256":"d".repeat(64),
                "execution_sha256":"e".repeat(64), "runner_sha256":"f".repeat(64),
                "created_at":1, "dispatched_at":2, "finished_at":3, "freshness":"fresh", "failure":null,
                "incomplete_log":false, "cleanup_pending":true}],
        });
        let mut state: State = serde_json::from_value(state).unwrap();
        state
            .seal_round(
                &RoundId::new("r-draft").unwrap(),
                SealedBody::new(COMMENTS, "r-draft", PublicationHash::of(&artifact)),
            )
            .unwrap();
        storage::save(&state_file, &state).unwrap();
        state_file
    }

    #[test]
    fn private_draft_requires_lead_session_and_valid_host() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        runtime().block_on(async {
            for bootstrap in [None, Some("unused-code".to_owned())] {
                let app = app(path.clone(), bootstrap.clone(), offline_inspect);
                for token in [None, Some("wrong"), Some("test-token")] {
                    if bootstrap.is_none() && token == Some("test-token") {
                        continue;
                    }
                    let response = app.clone().oneshot(get(URI, token)).await.unwrap();
                    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
                    let body = to_bytes(response.into_body(), 8192).await.unwrap();
                    assert!(!String::from_utf8(body.to_vec()).unwrap().contains(ADVICE));
                }
            }
            let mut request = get(URI, Some("test-token"));
            request
                .headers_mut()
                .insert(header::HOST, "evil.example".parse().unwrap());
            let response = app(path, None, offline_inspect)
                .oneshot(request)
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
        });
    }

    #[test]
    fn inspection_returns_exact_body_and_advice_without_logs_or_public_snapshot_leaks() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let original = fs::read(&path).unwrap();
        let app = app(path.clone(), None, offline_inspect);
        runtime().block_on(async {
            let response = app
                .clone()
                .oneshot(get(URI, Some("test-token")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
            assert_eq!(
                response.headers()[header::X_CONTENT_TYPE_OPTIONS],
                "nosniff"
            );
            let body = to_bytes(response.into_body(), 8192).await.unwrap();
            let inspected: serde_json::Value = serde_json::from_slice(&body).unwrap();
            let expected_body =
                format!("{COMMENTS}\n\n_Agent review via Review Platform (round r-draft)._");
            assert_eq!(inspected["publication_body"], expected_body);
            assert_eq!(
                inspected["publication_body_sha256"],
                PublicationHash::of(expected_body.as_bytes()).to_string()
            );
            assert_eq!(inspected["lead_advice"], ADVICE);
            assert_eq!(inspected["verdict"], "ACCEPT_WITH_NITS");
            assert_eq!(inspected["round_id"], "r-draft");
            assert_eq!(inspected["head_sha"], "a".repeat(40));
            assert_eq!(inspected["revision"], 2);
            assert_eq!(inspected["freshness"], "fresh");
            assert_eq!(inspected["decision"], "awaiting_decision");
            for field in ["review_log", "prompt", "model_provider", "gateway"] {
                assert!(inspected.get(field).is_none());
            }
            assert!(
                !String::from_utf8(body.to_vec())
                    .unwrap()
                    .contains("Private full review log")
            );
            let response = app
                .clone()
                .oneshot(get("/api/requests", None))
                .await
                .unwrap();
            let list = to_bytes(response.into_body(), 8192).await.unwrap();
            let response = app
                .oneshot(get("/api/requests/events", None))
                .await
                .unwrap();
            let mut stream = response.into_body().into_data_stream();
            let event = tokio::time::timeout(Duration::from_secs(3), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            for public in [list, event] {
                let text = String::from_utf8(public.to_vec()).unwrap();
                assert!(!text.contains(ADVICE));
                assert!(!text.contains("alert('draft')"));
                assert!(!text.contains("Private full review log"));
            }
        });
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn inspection_does_not_save_a_missing_seal_or_reuse_recorded_freshness() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let mut state: serde_json::Value =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        state["rounds"][0]["sealed_body"] = serde_json::Value::Null;
        fs::write(&path, serde_json::to_vec(&state).unwrap()).unwrap();
        let original = fs::read(&path).unwrap();
        runtime().block_on(async {
            let response = app(path.clone(), None, unknown_freshness)
                .oneshot(get(URI, Some("test-token")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 8192).await.unwrap();
            let inspected: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(inspected["freshness"], "unknown");
            assert!(
                inspected["publication_body"]
                    .as_str()
                    .unwrap()
                    .contains(COMMENTS)
            );
        });
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn inspection_forwards_confirmed_publication_ids_to_the_freshness_check() {
        use review_platform::publication::PublicationReceipt;
        use std::num::NonZeroU64;
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let key = PrKey::new("owner/repo", 15).unwrap();
        let id = RoundId::new("r-draft").unwrap();
        let mut state = storage::load(&path).unwrap();
        state.mark_round_cleaned(&id).unwrap();
        let hash = state
            .round(&id)
            .unwrap()
            .sealed_body()
            .unwrap()
            .hash()
            .clone();
        let attempt = state
            .reserve_publication(
                &key,
                &id,
                &hash,
                "uid:1".into(),
                "publisher".into(),
                NonZeroU64::new(7).unwrap(),
                vec![],
                4,
            )
            .unwrap();
        state.mark_publication_dispatched(&id, attempt, 5).unwrap();
        state
            .record_publication(
                &id,
                attempt,
                PublicationReceipt::new(
                    NonZeroU64::new(71).unwrap(),
                    "https://github.com/owner/repo/pull/15#pullrequestreview-71".into(),
                    "publisher".into(),
                    NonZeroU64::new(7).unwrap(),
                    HeadSha::new("a".repeat(40)).unwrap(),
                    "2026-11-01T10:00:00Z".into(),
                ),
                6,
            )
            .unwrap();
        storage::save(&path, &state).unwrap();
        let inspected =
            draft_commands::inspect_with_freshness(&path, &key, &id, |record, excluded| {
                assert_eq!(record.status(), RoundStatus::Published);
                assert_eq!(excluded, &[71]);
                Freshness::Stale
            })
            .unwrap();
        assert_eq!(inspected.freshness(), Freshness::Stale);
        assert_eq!(inspected.draft().lead_advice(), ADVICE);
    }

    #[test]
    fn inspection_rejects_invalid_identifiers_wrong_prs_and_unfinished_rounds() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let app = app(path.clone(), None, offline_inspect);
        runtime().block_on(async {
            for (uri, expected) in [
                (
                    "/api/drafts/invalid?repository=owner%2Frepo&number=15",
                    StatusCode::BAD_REQUEST,
                ),
                (
                    "/api/drafts/r-draft?repository=..%2Frepo&number=15",
                    StatusCode::BAD_REQUEST,
                ),
                (
                    "/api/drafts/r-draft?repository=owner%2Frepo&number=0",
                    StatusCode::BAD_REQUEST,
                ),
                (
                    "/api/drafts/r-draft?repository=owner%2Frepo",
                    StatusCode::BAD_REQUEST,
                ),
                (
                    "/api/drafts/r-missing?repository=owner%2Frepo&number=15",
                    StatusCode::NOT_FOUND,
                ),
                (
                    "/api/drafts/r-draft?repository=owner%2Frepo&number=16",
                    StatusCode::NOT_FOUND,
                ),
            ] {
                assert_eq!(
                    app.clone()
                        .oneshot(get(uri, Some("test-token")))
                        .await
                        .unwrap()
                        .status(),
                    expected,
                    "{uri}"
                );
            }
            let state: serde_json::Value =
                serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            for status in ["starting", "running", "failed", "interrupted"] {
                let mut unfinished = state.clone();
                unfinished["rounds"][0]["status"] = status.into();
                unfinished["rounds"][0]["sealed_body"] = serde_json::Value::Null;
                if status == "starting" {
                    unfinished["rounds"][0]["dispatched_at"] = serde_json::Value::Null;
                }
                if ["starting", "running"].contains(&status) {
                    unfinished["rounds"][0]["finished_at"] = serde_json::Value::Null;
                    unfinished["rounds"][0]["freshness"] = serde_json::Value::Null;
                } else {
                    unfinished["rounds"][0]["failure"] = "preflight".into();
                }
                fs::write(&path, serde_json::to_vec(&unfinished).unwrap()).unwrap();
                let response = app
                    .clone()
                    .oneshot(get(URI, Some("test-token")))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::CONFLICT, "{status}");
            }
        });
    }

    #[test]
    fn changed_or_missing_draft_files_fail_integrity_without_exposing_contents() {
        for change in ["saved_draft", "agent_bytes", "both_files", "missing_file"] {
            let directory = tempfile::tempdir().unwrap();
            let path = fixture(directory.path());
            let round_dir = directory.path().join("rounds/r-draft");
            let saved_file = round_dir.join("draft.json");
            let agent_file = round_dir.join("output/review-output.json");
            let original = fs::read(&path).unwrap();
            if change == "missing_file" {
                fs::remove_file(&saved_file).unwrap();
            } else if change == "agent_bytes" {
                let mut bytes = fs::read(&agent_file).unwrap();
                bytes.push(b'\n');
                fs::write(&agent_file, bytes).unwrap();
            } else {
                let mut saved: serde_json::Value =
                    serde_json::from_slice(&fs::read(&saved_file).unwrap()).unwrap();
                saved["comments"] = "Changed private content".into();
                fs::write(&saved_file, serde_json::to_vec(&saved).unwrap()).unwrap();
                if change == "both_files" {
                    saved.as_object_mut().unwrap().remove("review_log");
                    fs::write(&agent_file, serde_json::to_vec(&saved).unwrap()).unwrap();
                }
            }
            let error = draft_commands::read_verified(
                &path,
                &storage::load(&path).unwrap(),
                &PrKey::new("owner/repo", 15).unwrap(),
                &RoundId::new("r-draft").unwrap(),
            )
            .err()
            .unwrap();
            match change {
                "saved_draft" => assert!(matches!(error, DraftError::ArtifactMismatch)),
                "agent_bytes" | "both_files" => assert!(matches!(error, DraftError::SealChanged)),
                "missing_file" => assert!(matches!(
                    error,
                    DraftError::Artifact(review_platform::review::ArtifactError::Read { .. })
                )),
                _ => unreachable!(),
            }
            runtime().block_on(async {
                let response = app(path.clone(), None, offline_inspect)
                    .oneshot(get(URI, Some("test-token")))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
                let text =
                    String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec())
                        .unwrap();
                for secret in [
                    ADVICE,
                    "Changed private content",
                    "draft.json",
                    "review-output.json",
                    "Private full review log",
                ] {
                    assert!(!text.contains(secret));
                }
            });
            assert_eq!(fs::read(path).unwrap(), original);
        }
    }

    #[cfg(unix)]
    #[test]
    fn linked_draft_and_agent_files_are_rejected() {
        for file in ["draft.json", "output/review-output.json"] {
            let directory = tempfile::tempdir().unwrap();
            let path = fixture(directory.path());
            let linked = directory.path().join("rounds/r-draft").join(file);
            let original = directory.path().join("original.json");
            fs::rename(&linked, &original).unwrap();
            std::os::unix::fs::symlink(&original, &linked).unwrap();
            runtime().block_on(async {
                let response = app(path, None, offline_inspect)
                    .oneshot(get(URI, Some("test-token")))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            });
        }
    }
}
