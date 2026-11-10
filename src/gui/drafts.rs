//! Authenticated private draft responses, without exposing logs or adapter errors.

use super::{
    ApiError, AppState, AuthorizationResponse, RunResponse, api_error, has_session, publications,
};
use crate::{
    draft_commands::{DraftError, InspectedDraft},
    publication_commands::{self, RejectError},
};
use axum::{
    Json,
    extract::{Path as UrlPath, Query, State as HttpState},
    http::{HeaderMap, StatusCode},
};
use review_platform::{
    HeadSha, InputRevision, PrKey, RejectOutcome,
    config::{AgentId, ModelId, ReviewConfig},
    publication::PublicationHash,
    review::{Freshness, ReviewStatus, RoundId, RoundStateError, RoundStatus},
};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::Arc};

pub(super) type Inspect = fn(&Path, &PrKey, &RoundId) -> Result<InspectedDraft, DraftError>;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct DraftDecisionRequest {
    key: PrKey,
    body_hash: PublicationHash,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct PublishDraftRequest {
    key: PrKey,
    body_hash: PublicationHash,
    publisher_login: String,
}

pub(super) async fn publish(
    HttpState(state): HttpState<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<RoundId>,
    Json(request): Json<PublishDraftRequest>,
) -> Result<(StatusCode, Json<RunResponse>), (StatusCode, Json<ApiError>)> {
    if !has_session(&state, &headers) {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "Sign in again to use GUI actions.",
        ));
    }
    if state.runs.is_shutting_down() {
        return Err(api_error(
            StatusCode::SERVICE_UNAVAILABLE,
            "The GUI server is shutting down.",
        ));
    }
    let _start = state.job_start_guard(&request.key)?;
    state
        .publications
        .start(
            state.state_file.clone(),
            state.config_file.clone(),
            request.key,
            id,
            request.body_hash,
            request.publisher_login,
        )
        .map_err(publication_start_error)?;
    Ok((StatusCode::ACCEPTED, Json(RunResponse { accepted: true })))
}

fn publication_start_error(error: publications::StartError) -> (StatusCode, Json<ApiError>) {
    match error {
        publications::StartError::Active => api_error(
            StatusCode::CONFLICT,
            "A publication is already active for this PR. Watch its status before deciding again.",
        ),
        publications::StartError::Unavailable => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not read GUI publication jobs.",
        ),
    }
}

pub(super) async fn reject(
    HttpState(state): HttpState<Arc<AppState>>,
    headers: HeaderMap,
    UrlPath(id): UrlPath<RoundId>,
    Json(request): Json<DraftDecisionRequest>,
) -> Result<Json<AuthorizationResponse>, (StatusCode, Json<ApiError>)> {
    if !has_session(&state, &headers) {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "Sign in again to use GUI actions.",
        ));
    }
    if state
        .publications
        .is_active(&request.key)
        .map_err(publication_start_error)?
    {
        return Err(publication_start_error(publications::StartError::Active));
    }
    let outcome = tokio::task::spawn_blocking(move || {
        publication_commands::reject(&state.state_file, &request.key, &id, &request.body_hash)
    })
    .await
    .map_err(|_| {
        api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not record draft rejection.",
        )
    })?
    .map_err(reject_error)?;
    Ok(Json(AuthorizationResponse {
        changed: outcome == RejectOutcome::Rejected,
    }))
}

fn reject_error(error: RejectError) -> (StatusCode, Json<ApiError>) {
    match error {
        RejectError::Draft(error) => draft_error(error),
        RejectError::BodyHashMismatch => api_error(
            StatusCode::CONFLICT,
            "The body hash does not match this draft. Reopen the draft before deciding.",
        ),
        RejectError::Round(RoundStateError::CleanupPending(_)) => api_error(
            StatusCode::CONFLICT,
            "Sandbox cleanup must finish before rejecting this draft.",
        ),
        RejectError::Round(_) => api_error(
            StatusCode::CONFLICT,
            "This round cannot be rejected in its current state. Reopen the draft before deciding.",
        ),
        RejectError::Local(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not record draft rejection in local review state.",
        ),
    }
}

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
    receipt: Option<review_platform::publication::PublicationReceipt>,
    publisher_login: Option<String>,
    inspected_at: Option<u64>,
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
    let (inspected, publisher_login, inspected_at) = tokio::task::spawn_blocking(move || {
        let inspected_at = super::observation_time();
        let inspected = (state.inspect_draft)(&state.state_file, &key, &id)?;
        // Configuration failures must not prevent private inspection or rejection.
        let publisher_login = std::fs::read(&state.config_file)
            .ok()
            .and_then(|bytes| ReviewConfig::from_json(&bytes, &state.config_file).ok())
            .and_then(|config| config.publisher_login().map(str::to_owned));
        Ok::<_, DraftError>((inspected, publisher_login, inspected_at))
    })
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
        receipt: record.receipt().cloned(),
        publisher_login,
        inspected_at,
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
        app_with_publications(
            path,
            bootstrap,
            inspect_draft,
            Arc::new(publications::Publications::default()),
        )
    }

    fn app_with_publications(
        path: PathBuf,
        bootstrap: Option<String>,
        inspect_draft: Inspect,
        publications: Arc<publications::Publications>,
    ) -> Router {
        router_with_state(Arc::new(AppState {
            state_file: path,
            config_file: PathBuf::from("unused-config.json"),
            session: Mutex::new(Session {
                bootstrap,
                token: "test-token".into(),
            }),
            refresh: super::super::refresh_for_gui,
            prepare: prepare_for_gui,
            runs: Arc::new(Runs::new(
                Arc::new(AtomicBool::new(false)),
                review_commands::run_inspected_review,
            )),
            inspect_draft,
            publications,
            removing: Mutex::new(Default::default()),
            contexts: Arc::new(super::super::Contexts::default()),
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
            "schema_version":8,
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

    fn rejection_body(path: &Path) -> serde_json::Value {
        let state = storage::load(path).unwrap();
        let id = RoundId::new("r-draft").unwrap();
        serde_json::json!({
            "key":{"repository":"owner/repo","number":15},
            "body_hash":state.round(&id).unwrap().sealed_body().unwrap().hash(),
        })
    }

    fn publication_body(path: &Path) -> serde_json::Value {
        let mut body = rejection_body(path);
        body["publisher_login"] = "publisher".into();
        body
    }

    fn post_rejection(body: &serde_json::Value) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/api/drafts/r-draft/reject")
            .header(header::HOST, "localhost:8080")
            .header(header::ORIGIN, "http://localhost:8080")
            .header(header::AUTHORIZATION, "Bearer test-token")
            .header(header::CONTENT_TYPE, "application/json")
            .body(Body::from(serde_json::to_vec(body).unwrap()))
            .unwrap()
    }

    fn post_publication(body: &serde_json::Value) -> Request<Body> {
        let mut request = post_rejection(body);
        *request.uri_mut() = "/api/drafts/r-draft/publish".parse().unwrap();
        request
    }

    fn post_unmanage() -> Request<Body> {
        let mut request =
            post_rejection(&serde_json::json!({"repository":"owner/repo","number":15}));
        *request.uri_mut() = "/api/requests/unmanage".parse().unwrap();
        request
    }

    fn blocked_publication(
        path: &Path,
        config: &Path,
        key: &PrKey,
        id: &RoundId,
        hash: &PublicationHash,
        publisher_login: &str,
    ) -> Result<publication_commands::PublicationReport, publication_commands::PublishError> {
        assert_eq!(publisher_login, "publisher");
        assert_eq!(config, Path::new("unused-config.json"));
        assert_eq!(key, &PrKey::new("owner/repo", 15).unwrap());
        assert_eq!(id, &RoundId::new("r-draft").unwrap());
        assert_eq!(
            hash.as_str(),
            rejection_body(path)["body_hash"].as_str().unwrap()
        );
        fs::write(path.with_extension("started"), b"started").unwrap();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while !path.with_extension("release").exists() && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        Err(publication_commands::PublishError::Publisher(
            review_platform::github::publisher::PublisherError::WrongPublisher {
                expected: "publisher".into(),
                actual: "PRIVATE_ADAPTER_DETAIL".into(),
            },
        ))
    }

    fn confirmed_publication(
        path: &Path,
        config: &Path,
        key: &PrKey,
        id: &RoundId,
        hash: &PublicationHash,
        publisher_login: &str,
    ) -> Result<publication_commands::PublicationReport, publication_commands::PublishError> {
        assert_eq!(publisher_login, "publisher");
        // Simulate the adapter's confirmed response through the actual domain transitions.
        {
            let _pr_lock = storage::pr_lock(path, key)?;
            let _lock = storage::lock(path)?;
            let mut state = storage::load(path)?;
            state.mark_round_cleaned(id)?;
            let attempt = state.reserve_publication(
                key,
                id,
                hash,
                "uid:1".into(),
                "publisher".into(),
                std::num::NonZeroU64::new(7).unwrap(),
                vec![],
                4,
            )?;
            state.mark_publication_dispatched(id, attempt, 5)?;
            state.record_publication(
                id,
                attempt,
                review_platform::publication::PublicationReceipt::new(
                    std::num::NonZeroU64::new(71).unwrap(),
                    "https://github.com/owner/repo/pull/15#pullrequestreview-71".into(),
                    "publisher".into(),
                    std::num::NonZeroU64::new(7).unwrap(),
                    HeadSha::new("a".repeat(40)).unwrap(),
                    "2026-11-01T18:00:00Z".into(),
                ),
                6,
            )?;
            storage::save(path, &state)?;
        }
        publication_commands::publish(path, config, key, id, hash)
    }

    #[test]
    fn confirmed_publication_exposes_the_saved_receipt_without_private_draft_leaks() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let body = publication_body(&path);
        let jobs = Arc::new(publications::Publications::new(confirmed_publication));
        runtime().block_on(async {
            let app = app_with_publications(path.clone(), None, unknown_freshness, jobs);
            assert_eq!(
                app.clone()
                    .oneshot(post_publication(&body))
                    .await
                    .unwrap()
                    .status(),
                StatusCode::ACCEPTED
            );
            let public = tokio::time::timeout(Duration::from_secs(2), async {
                loop {
                    let response = app
                        .clone()
                        .oneshot(get("/api/requests", None))
                        .await
                        .unwrap();
                    let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
                    let result: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    if result["requests"][0]["publication"]["status"] == "published" {
                        break bytes;
                    }
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            let result: serde_json::Value = serde_json::from_slice(&public).unwrap();
            assert_eq!(
                result["requests"][0]["latest_round"]["receipt"]["review_id"],
                71
            );
            assert_eq!(result["requests"][0]["latest_round"]["status"], "published");
            let text = String::from_utf8(public.to_vec()).unwrap();
            for secret in [ADVICE, "alert('draft')", "Private full review log"] {
                assert!(!text.contains(secret));
            }
            let response = app.oneshot(get(URI, Some("test-token"))).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let result: serde_json::Value =
                serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap())
                    .unwrap();
            assert_eq!(result["receipt"]["review_id"], 71);
            assert_eq!(result["decision"], "published");
        });
    }

    #[test]
    fn publication_requires_lead_session_same_origin_and_valid_inspected_identifiers() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let before = fs::read(&path).unwrap();
        let body = publication_body(&path);
        runtime().block_on(async {
            for (change, expected) in [
                ("no_token", StatusCode::UNAUTHORIZED),
                ("wrong_token", StatusCode::UNAUTHORIZED),
                ("bootstrap_unused", StatusCode::UNAUTHORIZED),
                ("no_origin", StatusCode::FORBIDDEN),
                ("cross_origin", StatusCode::FORBIDDEN),
                ("wrong_host", StatusCode::MISDIRECTED_REQUEST),
                ("invalid_hash", StatusCode::UNPROCESSABLE_ENTITY),
                ("invalid_pr", StatusCode::UNPROCESSABLE_ENTITY),
                ("missing_hash", StatusCode::UNPROCESSABLE_ENTITY),
                ("missing_publisher", StatusCode::UNPROCESSABLE_ENTITY),
                ("null_publisher", StatusCode::UNPROCESSABLE_ENTITY),
                ("invalid_publisher", StatusCode::UNPROCESSABLE_ENTITY),
                ("invalid_round", StatusCode::BAD_REQUEST),
                ("unknown_field", StatusCode::UNPROCESSABLE_ENTITY),
            ] {
                let mut body = body.clone();
                match change {
                    "invalid_hash" => body["body_hash"] = "invalid".into(),
                    "invalid_pr" => body["key"]["number"] = 0.into(),
                    "missing_hash" => {
                        body.as_object_mut().unwrap().remove("body_hash");
                    }
                    "missing_publisher" => {
                        body.as_object_mut().unwrap().remove("publisher_login");
                    }
                    "null_publisher" => body["publisher_login"] = serde_json::Value::Null,
                    "invalid_publisher" => body["publisher_login"] = 42.into(),
                    "unknown_field" => body["publication_body"] = "untrusted replacement".into(),
                    _ => {}
                }
                let mut request = post_publication(&body);
                match change {
                    "no_token" => {
                        request.headers_mut().remove(header::AUTHORIZATION);
                    }
                    "wrong_token" => {
                        request
                            .headers_mut()
                            .insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
                    }
                    "no_origin" => {
                        request.headers_mut().remove(header::ORIGIN);
                    }
                    "cross_origin" => {
                        request
                            .headers_mut()
                            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
                    }
                    "wrong_host" => {
                        request
                            .headers_mut()
                            .insert(header::HOST, "evil.example".parse().unwrap());
                    }
                    "invalid_round" => {
                        *request.uri_mut() = "/api/drafts/invalid/publish".parse().unwrap()
                    }
                    _ => {}
                }
                let response = app(
                    path.clone(),
                    (change == "bootstrap_unused").then(|| "code".into()),
                    offline_inspect,
                )
                .oneshot(request)
                .await
                .unwrap();
                assert_eq!(response.status(), expected, "{change}");
            }
        });
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn publication_jobs_survive_responses_reject_duplicates_and_stream_safe_failures() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let before = fs::read(&path).unwrap();
        let body = publication_body(&path);
        let jobs = Arc::new(publications::Publications::new(blocked_publication));
        runtime().block_on(async {
            let app = app_with_publications(path.clone(), None, offline_inspect, jobs.clone());
            let response = app.clone().oneshot(post_publication(&body)).await.unwrap();
            assert_eq!(response.status(), StatusCode::ACCEPTED);
            drop(response); // No browser connection owns the worker.
            tokio::time::timeout(Duration::from_secs(2), async {
                while !path.with_extension("started").exists() {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
            for request in [
                post_publication(&body),
                post_rejection(&rejection_body(&path)),
                post_unmanage(),
            ] {
                assert_eq!(
                    app.clone().oneshot(request).await.unwrap().status(),
                    StatusCode::CONFLICT
                );
            }
            let response = app
                .clone()
                .oneshot(get("/api/requests/events", None))
                .await
                .unwrap();
            let mut stream = response.into_body().into_data_stream();
            let event = tokio::time::timeout(Duration::from_secs(2), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(
                String::from_utf8(event.to_vec())
                    .unwrap()
                    .contains("\"status\":\"active\"")
            );
            fs::write(path.with_extension("release"), b"release").unwrap();
            let event = tokio::time::timeout(Duration::from_secs(3), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let text = String::from_utf8(event.to_vec()).unwrap();
            assert!(text.contains("\"status\":\"failed\""));
            assert!(text.contains("Check publisher_login"));
            for secret in ["PRIVATE_ADAPTER_DETAIL", ADVICE, "Private full review log"] {
                assert!(!text.contains(secret));
            }
            drop(stream);
            drop(app);
        });
        assert_eq!(fs::read(path).unwrap(), before);
    }

    #[test]
    fn unmanagement_clears_publication_jobs_before_readding_and_preserves_them_on_failure() {
        let cases: [(publications::Publish, &str); 2] = [
            (confirmed_publication, "published"),
            (blocked_publication, "failed"),
        ];
        for (publish, status) in cases {
            let directory = tempfile::tempdir().unwrap();
            let path = fixture(directory.path());
            let key = PrKey::new("owner/repo", 15).unwrap();
            fs::write(path.with_extension("release"), b"").unwrap();
            let jobs = Arc::new(publications::Publications::new(publish));
            let app = app_with_publications(path.clone(), None, offline_inspect, jobs.clone());
            runtime().block_on(async {
                let response = app
                    .clone()
                    .oneshot(post_publication(&publication_body(&path)))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::ACCEPTED);
                let summary = tokio::time::timeout(Duration::from_secs(3), async {
                    loop {
                        let summary = serde_json::to_value(jobs.summary(&key).unwrap()).unwrap();
                        if summary["status"] == status {
                            break summary;
                        }
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                })
                .await
                .unwrap();
                if status == "failed" {
                    // Pending cleanup refuses removal without discarding the job.
                    let response = app.clone().oneshot(post_unmanage()).await.unwrap();
                    assert_eq!(response.status(), StatusCode::CONFLICT);
                    assert_eq!(
                        serde_json::to_value(jobs.summary(&key).unwrap()).unwrap(),
                        summary
                    );
                    let mut state = storage::load(&path).unwrap();
                    state
                        .mark_round_cleaned(&RoundId::new("r-draft").unwrap())
                        .unwrap();
                    storage::save(&path, &state).unwrap();
                }
                let response = app.clone().oneshot(post_unmanage()).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                assert!(jobs.summary(&key).unwrap().is_none());
                {
                    let _pr_lock = storage::pr_lock(&path, &key).unwrap();
                    let _lock = storage::lock(&path).unwrap();
                    let mut state = storage::load(&path).unwrap();
                    state
                        .record_request(key.clone(), HeadSha::new("a".repeat(40)).unwrap())
                        .unwrap();
                    storage::save(&path, &state).unwrap();
                }
                let response = app
                    .clone()
                    .oneshot(get("/api/requests", None))
                    .await
                    .unwrap();
                let snapshot: serde_json::Value =
                    serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap())
                        .unwrap();
                assert_eq!(snapshot["requests"].as_array().unwrap().len(), 1);
                let request = &snapshot["requests"][0];
                assert!(request["run"].is_null());
                assert!(request["publication"].is_null());
                assert!(request["latest_round"].is_null());
            });
        }
    }

    #[test]
    fn unmanagement_requires_cleanup_and_deletes_private_draft_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let files = [
            directory.path().join("rounds/r-draft/draft.json"),
            directory
                .path()
                .join("rounds/r-draft/output/review-output.json"),
        ];
        let id = RoundId::new("r-draft").unwrap();
        let key = PrKey::new("owner/repo", 15).unwrap();
        runtime().block_on(async {
            let app = app(path.clone(), None, offline_inspect);
            let saved = fs::read(&path).unwrap();
            let response = app.clone().oneshot(post_unmanage()).await.unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(fs::read(&path).unwrap(), saved);
            let mut state = storage::load(&path).unwrap();
            state.mark_round_cleaned(&id).unwrap();
            storage::save(&path, &state).unwrap();
            let response = app.clone().oneshot(post_unmanage()).await.unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let removed = storage::load(&path).unwrap();
            assert!(removed.get(&key).is_none());
            assert!(removed.round(&id).is_none());
            let response = app.oneshot(get(URI, Some("test-token"))).await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND);
        });
        for file in files {
            assert!(!file.exists());
        }
        assert!(!directory.path().join("rounds/r-draft").exists());
    }

    #[test]
    fn unmanagement_checks_all_runner_locks_before_deleting_and_can_retry_missing_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let key = PrKey::new("owner/repo", 15).unwrap();
        let mut state = storage::load(&path).unwrap();
        state
            .mark_round_cleaned(&RoundId::new("r-draft").unwrap())
            .unwrap();
        let mut raw = serde_json::to_value(&state).unwrap();
        let mut prior = raw["rounds"][0].clone();
        prior["id"] = "r-prior".into();
        prior["sandbox"] = "rp-r-prior".into();
        prior["status"] = "failed".into();
        prior["failure"] = "preflight".into();
        prior["sealed_body"] = serde_json::Value::Null;
        raw["rounds"].as_array_mut().unwrap().push(prior);
        let mut state: State = serde_json::from_value(raw).unwrap();
        let other = PrKey::new("other/repo", 16).unwrap();
        state
            .record_request(other.clone(), HeadSha::new("b".repeat(40)).unwrap())
            .unwrap();
        storage::save(&path, &state).unwrap();
        let prior_dir = directory.path().join("rounds/r-prior");
        let guard = storage::try_lock(&prior_dir.join("runner")).unwrap();
        let before = fs::read(&path).unwrap();
        assert!(matches!(request_commands::unmanage(&path, &key),
            Err(request_commands::UnmanageError::RunnerLock { round, .. }) if round.as_str() == "r-prior"));
        assert_eq!(fs::read(&path).unwrap(), before);
        let draft_dir = directory.path().join("rounds/r-draft");
        assert!(draft_dir.join("draft.json").exists());
        drop(guard);
        // An earlier failed attempt may already have removed some round files.
        fs::remove_dir_all(&prior_dir).unwrap();
        let other_dir = directory.path().join("rounds/r-other");
        fs::create_dir(&other_dir).unwrap();
        fs::write(other_dir.join("keep"), b"other PR files").unwrap();
        request_commands::unmanage(&path, &key).unwrap();
        assert!(!draft_dir.exists());
        let saved = storage::load(&path).unwrap();
        assert!(saved.get(&key).is_none());
        assert!(saved.get(&other).is_some());
        assert!(saved.rounds().is_empty());
        assert_eq!(fs::read(other_dir.join("keep")).unwrap(), b"other PR files");
    }

    #[cfg(unix)]
    #[test]
    fn unmanagement_never_follows_round_or_output_symlinks_and_rejects_a_linked_root() {
        use std::os::unix::fs::symlink;
        for linked in ["rounds", "rounds/r-draft", "rounds/r-draft/output"] {
            let directory = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let path = fixture(directory.path());
            let key = PrKey::new("owner/repo", 15).unwrap();
            let mut state = storage::load(&path).unwrap();
            state
                .mark_round_cleaned(&RoundId::new("r-draft").unwrap())
                .unwrap();
            storage::save(&path, &state).unwrap();
            let original = fs::read(&path).unwrap();
            let source = directory.path().join(linked);
            let target = outside.path().join("saved");
            fs::rename(&source, &target).unwrap();
            fs::write(target.join("keep"), b"outside files").unwrap();
            symlink(&target, &source).unwrap();
            let result = request_commands::unmanage(&path, &key);
            if linked == "rounds" {
                assert!(matches!(
                    result,
                    Err(request_commands::UnmanageError::Artifacts { .. })
                ));
                assert_eq!(fs::read(&path).unwrap(), original);
            } else {
                result.unwrap();
                assert!(!directory.path().join("rounds/r-draft").exists());
                assert!(storage::load(&path).unwrap().get(&key).is_none());
            }
            assert_eq!(fs::read(target.join("keep")).unwrap(), b"outside files");
        }
    }

    #[test]
    fn rejection_requires_a_verified_lead_and_same_origin() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let original = fs::read(&path).unwrap();
        let body = rejection_body(&path);
        runtime().block_on(async {
            for (change, expected) in [
                ("no_token", StatusCode::UNAUTHORIZED),
                ("wrong_token", StatusCode::UNAUTHORIZED),
                ("no_origin", StatusCode::FORBIDDEN),
                ("cross_origin", StatusCode::FORBIDDEN),
                ("wrong_host", StatusCode::MISDIRECTED_REQUEST),
                ("bootstrap_unused", StatusCode::UNAUTHORIZED),
            ] {
                let mut request = post_rejection(&body);
                match change {
                    "no_token" => {
                        request.headers_mut().remove(header::AUTHORIZATION);
                    }
                    "wrong_token" => {
                        request
                            .headers_mut()
                            .insert(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
                    }
                    "no_origin" => {
                        request.headers_mut().remove(header::ORIGIN);
                    }
                    "cross_origin" => {
                        request
                            .headers_mut()
                            .insert(header::ORIGIN, "https://evil.example".parse().unwrap());
                    }
                    "wrong_host" => {
                        request
                            .headers_mut()
                            .insert(header::HOST, "evil.example".parse().unwrap());
                    }
                    "bootstrap_unused" => {}
                    _ => unreachable!(),
                }
                let bootstrap = (change == "bootstrap_unused").then(|| "code".into());
                let response = app(path.clone(), bootstrap, offline_inspect)
                    .oneshot(request)
                    .await
                    .unwrap();
                assert_eq!(response.status(), expected, "{change}");
                assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            }
        });
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn rejection_checks_the_explicit_round_key_hash_and_cleanup_without_mutation() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let original = fs::read(&path).unwrap();
        let body = rejection_body(&path);
        let id = RoundId::new("r-draft").unwrap();
        let key = PrKey::new("owner/repo", 15).unwrap();
        let hash = PublicationHash::new(body["body_hash"].as_str().unwrap()).unwrap();
        assert!(matches!(
            publication_commands::reject(&path, &key, &id, &hash),
            Err(RejectError::Round(RoundStateError::CleanupPending(_)))
        ));
        assert!(matches!(
            publication_commands::reject(&path, &key, &id, &PublicationHash::of(b"wrong")),
            Err(RejectError::BodyHashMismatch)
        ));
        runtime().block_on(async {
            let app = app(path.clone(), None, offline_inspect);
            for (change, expected) in [
                ("cleanup", StatusCode::CONFLICT),
                ("wrong_hash", StatusCode::CONFLICT),
                ("invalid_hash", StatusCode::UNPROCESSABLE_ENTITY),
                ("wrong_pr", StatusCode::NOT_FOUND),
                ("invalid_pr", StatusCode::UNPROCESSABLE_ENTITY),
                ("missing_round", StatusCode::NOT_FOUND),
                ("invalid_round", StatusCode::BAD_REQUEST),
                ("unknown_field", StatusCode::UNPROCESSABLE_ENTITY),
            ] {
                let mut body = body.clone();
                match change {
                    "wrong_hash" => body["body_hash"] = "0".repeat(64).into(),
                    "invalid_hash" => body["body_hash"] = "invalid".into(),
                    "wrong_pr" => body["key"]["number"] = 16.into(),
                    "invalid_pr" => body["key"]["number"] = 0.into(),
                    "unknown_field" => body["head_sha"] = "a".repeat(40).into(),
                    _ => {}
                }
                let mut request = post_rejection(&body);
                if change == "missing_round" {
                    *request.uri_mut() = "/api/drafts/r-missing/reject".parse().unwrap();
                }
                if change == "invalid_round" {
                    *request.uri_mut() = "/api/drafts/invalid/reject".parse().unwrap();
                }
                let response = app.clone().oneshot(request).await.unwrap();
                assert_eq!(response.status(), expected, "{change}");
            }
        });
        assert_eq!(fs::read(path).unwrap(), original);
    }

    #[test]
    fn rejection_preserves_stale_private_drafts_and_authorization_and_is_idempotent() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let body = rejection_body(&path);
        let id = RoundId::new("r-draft").unwrap();
        let key = PrKey::new("owner/repo", 15).unwrap();
        let mut state = storage::load(&path).unwrap();
        state.mark_round_cleaned(&id).unwrap();
        state
            .authorize(
                &key,
                &HeadSha::new("a".repeat(40)).unwrap(),
                InputRevision::new(2).unwrap(),
                review_platform::AuthorizationScope::PrCycle,
            )
            .unwrap();
        let discussion = state.get(&key).unwrap().discussion().unwrap().clone();
        state
            .refresh(&key, &HeadSha::new("b".repeat(40)).unwrap(), &discussion)
            .unwrap();
        storage::save(&path, &state).unwrap();
        let files = ["draft.json", "output/review-output.json"]
            .map(|file| directory.path().join("rounds/r-draft").join(file));
        let before = files.each_ref().map(|file| fs::read(file).unwrap());
        runtime().block_on(async {
            let app = app(path.clone(), None, offline_inspect);
            let mut recorded = None;
            for changed in [true, false] {
                let response = app.clone().oneshot(post_rejection(&body)).await.unwrap();
                assert_eq!(response.status(), StatusCode::OK);
                let result: serde_json::Value =
                    serde_json::from_slice(&to_bytes(response.into_body(), 8192).await.unwrap())
                        .unwrap();
                assert_eq!(result, serde_json::json!({"changed":changed}));
                if let Some(before) = &recorded {
                    assert_eq!(&fs::read(&path).unwrap(), before);
                }
                recorded = Some(fs::read(&path).unwrap());
            }
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
            let text = String::from_utf8(event.to_vec()).unwrap();
            assert!(text.contains("\"status\":\"rejected\""));
            assert!(!text.contains(ADVICE));
        });
        let saved = storage::load(&path).unwrap();
        saved.validate().unwrap();
        let round = saved.round(&id).unwrap();
        assert_eq!(round.status(), RoundStatus::Rejected);
        assert!(round.rejection().unwrap().at() > 0);
        assert!(!round.rejection().unwrap().operator().is_empty());
        assert_eq!(
            saved.get(&key).unwrap().authorization(),
            state.get(&key).unwrap().authorization()
        );
        for (file, original) in files.iter().zip(before) {
            assert_eq!(fs::read(file).unwrap(), original);
        }
    }

    #[test]
    fn rejection_refuses_altered_artifacts_and_publication_in_progress() {
        let directory = tempfile::tempdir().unwrap();
        let path = fixture(directory.path());
        let body = rejection_body(&path);
        let id = RoundId::new("r-draft").unwrap();
        let mut state = storage::load(&path).unwrap();
        state.mark_round_cleaned(&id).unwrap();
        storage::save(&path, &state).unwrap();
        let artifact = directory
            .path()
            .join("rounds/r-draft/output/review-output.json");
        let original = fs::read(&artifact).unwrap();
        let mut altered = original.clone();
        altered.push(b'\n');
        fs::write(&artifact, altered).unwrap();
        runtime().block_on(async {
            let app = app(path.clone(), None, offline_inspect);
            let response = app.clone().oneshot(post_rejection(&body)).await.unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let text =
                String::from_utf8(to_bytes(response.into_body(), 8192).await.unwrap().to_vec())
                    .unwrap();
            for secret in [ADVICE, "review-output.json", "Private full review log"] {
                assert!(!text.contains(secret));
            }
            fs::write(&artifact, original).unwrap();
            state
                .reserve_publication(
                    &PrKey::new("owner/repo", 15).unwrap(),
                    &id,
                    &PublicationHash::new(body["body_hash"].as_str().unwrap()).unwrap(),
                    "uid:1".into(),
                    "publisher".into(),
                    std::num::NonZeroU64::new(7).unwrap(),
                    vec![],
                    4,
                )
                .unwrap();
            storage::save(&path, &state).unwrap();
            let before = fs::read(&path).unwrap();
            let response = app.oneshot(post_rejection(&body)).await.unwrap();
            assert_eq!(response.status(), StatusCode::CONFLICT);
            assert_eq!(fs::read(&path).unwrap(), before);
        });
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
