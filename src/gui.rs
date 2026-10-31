//! Loopback HTTP server for the embedded operator UI and local request snapshots.

use crate::{review_commands, storage};
use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::{Path as UrlPath, State as HttpState},
    http::{HeaderValue, Method, Request, StatusCode, header},
    middleware::{Next, from_fn},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use futures_util::{Stream, stream};
use include_dir::{Dir, include_dir};
use review_platform::{
    Authorization, CommentSource,
    review::{RoundRecord, RoundStatus},
};
use serde::Serialize;
use std::{
    convert::Infallible,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::time::{MissedTickBehavior, interval};

const ADDRESS: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
const STATE_CHECK_INTERVAL: Duration = Duration::from_secs(1);
static UI: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/ui/dist");

#[derive(Serialize)]
struct ApiInfo {
    name: &'static str,
    version: u8,
}

#[derive(Serialize)]
struct Greeting {
    message: &'static str,
}

#[derive(Serialize)]
struct RequestList {
    requests: Vec<RequestSummary>,
}

#[derive(Serialize)]
struct RequestSummary {
    repository: String,
    number: u64,
    head_sha: String,
    revision: u64,
    authorization: Authorization,
    discussion: Option<DiscussionSummary>,
    validated_model: Option<ValidatedModelSummary>,
    latest_round: Option<RoundSummary>,
}

#[derive(Serialize)]
struct DiscussionSummary {
    issue_comments: usize,
    review_comments: usize,
    review_bodies: usize,
}

#[derive(Serialize)]
struct ValidatedModelSummary {
    model: String,
    agent: String,
}

#[derive(Serialize)]
struct RoundSummary {
    id: String,
    status: RoundStatus,
    cleanup_pending: bool,
    cleanup_status: CleanupStatus,
    head_sha: String,
    revision: u64,
    model: String,
    agent: String,
}

#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum CleanupStatus {
    Complete,
    InProgress,
    NeedsAttention,
    Unknown,
}

#[derive(Serialize)]
struct ApiError {
    error: &'static str,
}

#[derive(PartialEq, Eq)]
enum LastEvent {
    Snapshot(String),
    ReadError,
}

pub fn run(state_override: Option<PathBuf>) -> Result<()> {
    let state_file = storage::resolve_path(state_override)?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting GUI runtime")?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(ADDRESS)
            .await
            .context("binding GUI to 127.0.0.1:8080")?;
        println!("Review Platform GUI: http://localhost:8080/");
        axum::serve(listener, router(state_file))
            .await
            .context("serving GUI on 127.0.0.1:8080")
    })
}

fn router(state_file: PathBuf) -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api", get(api_info))
        .route("/api/hello", get(hello))
        .route("/api/requests", get(requests))
        .route("/api/requests/events", get(request_events))
        .route("/assets/{*path}", get(asset))
        .with_state(state_file)
        .layer(from_fn(localhost_read_only))
}

async fn localhost_read_only(request: Request<axum::body::Body>, next: Next) -> Response {
    let valid_host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost:8080") || host == "127.0.0.1:8080"
        });
    if !valid_host {
        return StatusCode::MISDIRECTED_REQUEST.into_response();
    }
    // Mutating API routes need lead authentication and cross-origin protection.
    if !matches!(*request.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let mut response = next.run(request).await;
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

async fn api_info() -> impl IntoResponse {
    Json(ApiInfo {
        name: "Review Platform API",
        version: 1,
    })
}

async fn hello() -> impl IntoResponse {
    Json(Greeting {
        message: "Hello from the Review Platform API!",
    })
}

async fn requests(
    HttpState(state_file): HttpState<PathBuf>,
) -> Result<Json<RequestList>, (StatusCode, Json<ApiError>)> {
    read_request_list(state_file)
        .await
        .map(Json)
        .map_err(|error| {
            eprintln!("error loading GUI request list: {error:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: "Could not read local review state.",
                }),
            )
        })
}

async fn request_events(
    HttpState(state_file): HttpState<PathBuf>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut ticker = interval(STATE_CHECK_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let updates = stream::unfold(
        (state_file, ticker, None::<LastEvent>),
        |(state_file, mut ticker, mut previous)| async move {
            loop {
                ticker.tick().await;
                let current = match read_request_list(state_file.clone())
                    .await
                    .and_then(|list| {
                        serde_json::to_string(&list).context("serializing request list")
                    }) {
                    Ok(json) => LastEvent::Snapshot(json),
                    Err(error) => {
                        if previous.as_ref() != Some(&LastEvent::ReadError) {
                            eprintln!("error loading GUI request list: {error:#}");
                        }
                        LastEvent::ReadError
                    }
                };
                if previous.as_ref() == Some(&current) {
                    continue;
                }
                let event = match &current {
                    LastEvent::Snapshot(json) => {
                        Event::default().event("requests").data(json.clone())
                    }
                    LastEvent::ReadError => Event::default()
                        .event("state_error")
                        .data("Could not read local review state."),
                };
                previous = Some(current);
                return Some((
                    Ok::<Event, Infallible>(event),
                    (state_file, ticker, previous),
                ));
            }
        },
    );
    Sse::new(updates).keep_alive(KeepAlive::new().interval(Duration::from_secs(15)))
}

async fn read_request_list(state_file: PathBuf) -> Result<RequestList> {
    tokio::task::spawn_blocking(move || request_list(&state_file))
        .await
        .context("joining local state read")?
}

fn request_list(state_file: &Path) -> Result<RequestList> {
    let state = storage::load(state_file)?;
    let mut recorded = state.requests().iter().collect::<Vec<_>>();
    recorded.sort_by_key(|request| (request.key().repository(), request.key().number()));
    let requests = recorded
        .into_iter()
        .map(|request| -> Result<RequestSummary> {
            Ok(RequestSummary {
                repository: request.key().repository().to_owned(),
                number: request.key().number(),
                head_sha: request.head_sha().to_string(),
                revision: request.revision().get(),
                authorization: request.authorization(),
                discussion: request.discussion().map(|discussion| DiscussionSummary {
                    issue_comments: discussion.count(CommentSource::IssueComment),
                    review_comments: discussion.count(CommentSource::ReviewComment),
                    review_bodies: discussion.count(CommentSource::Review),
                }),
                validated_model: request.model_validation().map(|validation| {
                    ValidatedModelSummary {
                        model: validation.model().to_string(),
                        agent: validation.agent().to_string(),
                    }
                }),
                latest_round: state
                    .latest_round(request.key())
                    .map(|round| -> Result<RoundSummary> {
                        Ok(RoundSummary {
                            id: round.id().to_string(),
                            status: round.status(),
                            cleanup_pending: round.cleanup_pending(),
                            cleanup_status: cleanup_status(state_file, round)?,
                            head_sha: round.head_sha().to_string(),
                            revision: round.revision().get(),
                            model: round.model().to_string(),
                            agent: round.agent().to_string(),
                        })
                    })
                    .transpose()?,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(RequestList { requests })
}

fn needs_cleanup_check(round: &RoundRecord) -> bool {
    match round.status() {
        RoundStatus::Starting | RoundStatus::Running => true,
        RoundStatus::AwaitingDecision
        | RoundStatus::Publishing
        | RoundStatus::PublicationUnknown
        | RoundStatus::Published
        | RoundStatus::Rejected
        | RoundStatus::Failed
        | RoundStatus::Interrupted => round.cleanup_pending(),
    }
}

fn cleanup_status(state_file: &Path, round: &RoundRecord) -> Result<CleanupStatus> {
    if !needs_cleanup_check(round) {
        return Ok(CleanupStatus::Complete);
    }

    let runner_lock = review_commands::round_path(state_file, round.id()).join("runner");
    match storage::probe_lock(&runner_lock) {
        Ok(storage::LockProbe::Held) => Ok(CleanupStatus::InProgress),
        Ok(storage::LockProbe::Available(_guard)) => {
            // The runner might have saved its final state after our initial read.
            // Hold the lock while rereading so a new runner cannot race this check.
            let state = storage::load(state_file)?;
            Ok(match state.round(round.id()) {
                Some(current) if needs_cleanup_check(current) => CleanupStatus::NeedsAttention,
                Some(_) => CleanupStatus::Complete,
                None => CleanupStatus::Unknown,
            })
        }
        Err(_) => Ok(CleanupStatus::Unknown),
    }
}

async fn index() -> Response {
    serve_file("index.html")
}

async fn asset(UrlPath(path): UrlPath<String>) -> Response {
    if path.split('/').any(|part| part == ".." || part.is_empty()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    serve_file(&format!("assets/{path}"))
}

fn serve_file(path: &str) -> Response {
    let Some(file) = UI.get_file(path) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let content_type = match Path::new(path).extension().and_then(|ext| ext.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("woff2") => "font/woff2",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    let mut response = (
        [(header::CONTENT_TYPE, content_type)],
        file.contents().to_vec(),
    )
        .into_response();
    if path == "index.html" {
        response.headers_mut().insert(
            header::CONTENT_SECURITY_POLICY,
            HeaderValue::from_static(
                "default-src 'self'; connect-src 'self'; img-src 'self' data:; \
                 style-src 'self' 'unsafe-inline'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'",
            ),
        );
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use review_platform::{
        CommentFingerprint, CommentHash, HeadSha, ObservedDiscussion, PrKey, State,
    };
    use std::num::NonZeroU64;
    use std::time::{Duration, Instant};

    #[test]
    fn request_snapshot_exposes_discussion_counts_without_fingerprints() {
        let directory = tempfile::tempdir().unwrap();
        let state_file = directory.path().join("state.json");
        let mut state = State::default();
        let key = PrKey::new("owner/repo", 15).unwrap();
        let head = HeadSha::new("a".repeat(40)).unwrap();
        state.record_request(key.clone(), head.clone()).unwrap();
        let discussion = ObservedDiscussion::new(
            NonZeroU64::new(7).unwrap(),
            vec![
                CommentFingerprint::new(
                    CommentSource::IssueComment,
                    NonZeroU64::new(10).unwrap(),
                    CommentHash::from_digest([1; 32]),
                ),
                CommentFingerprint::new(
                    CommentSource::ReviewComment,
                    NonZeroU64::new(11).unwrap(),
                    CommentHash::from_digest([2; 32]),
                ),
                CommentFingerprint::new(
                    CommentSource::Review,
                    NonZeroU64::new(12).unwrap(),
                    CommentHash::from_digest([3; 32]),
                ),
            ],
        )
        .unwrap();
        state.refresh(&key, &head, &discussion).unwrap();
        storage::save(&state_file, &state).unwrap();

        let snapshot = serde_json::to_value(request_list(&state_file).unwrap()).unwrap();
        let request = &snapshot["requests"][0];
        assert_eq!(
            request["discussion"],
            serde_json::json!({
                "issue_comments": 1,
                "review_comments": 1,
                "review_bodies": 1,
            })
        );
        assert!(request["validated_model"].is_null());
        assert!(request["latest_round"].is_null());
    }

    #[test]
    fn cleanup_status_tracks_runner_lock_and_unfinished_rounds() {
        let directory = tempfile::tempdir().unwrap();
        let state_file = directory.path().join("state.json");
        let runner = directory.path().join("rounds/r-test/runner");
        let mut state = serde_json::json!({
            "schema_version": 6,
            "requests": [{
                "key": {"repository": "owner/repo", "number": 15},
                "head_sha": "a".repeat(40),
                "authorization": "awaiting_lead",
                "revision": 2,
                "discussion": {"author_id": 7, "comments": []},
                "model_validation": null
            }],
            "rounds": [{
                "id": "r-test",
                "key": {"repository": "owner/repo", "number": 15},
                "status": "running",
                "sandbox": "rp-722d74657374",
                "head_sha": "a".repeat(40),
                "revision": 2,
                "discussion": {"author_id": 7, "comments": []},
                "model": "model-alpha",
                "agent": "claude",
                "gateway": "test-gateway",
                "workspace": "default",
                "driver": "vm",
                "model_provider": "test-model-provider",
                "github_provider": "test-github-readonly",
                "prompt_sha256": "a".repeat(64),
                "format_sha256": "b".repeat(64),
                "archive_sha256": "c".repeat(64),
                "archive_config_sha256": "d".repeat(64),
                "execution_sha256": "e".repeat(64),
                "runner_sha256": "f".repeat(64),
                "created_at": 1,
                "dispatched_at": 2,
                "finished_at": null,
                "freshness": null,
                "failure": null,
                "incomplete_log": false,
                "cleanup_pending": true
            }]
        });
        let write_state = |value: &serde_json::Value| {
            std::fs::write(&state_file, serde_json::to_vec(value).unwrap()).unwrap();
        };
        let snapshot = || {
            serde_json::to_value(request_list(&state_file).unwrap()).unwrap()["requests"][0]
                ["latest_round"]["cleanup_status"]
                .as_str()
                .unwrap()
                .to_owned()
        };
        write_state(&state);
        assert_eq!(snapshot(), "unknown");
        assert!(!runner.parent().unwrap().exists());

        let guard = storage::try_lock(&runner).unwrap();
        assert_eq!(snapshot(), "in_progress");
        state["rounds"][0]["status"] = "starting".into();
        state["rounds"][0]["dispatched_at"] = serde_json::Value::Null;
        write_state(&state);
        assert_eq!(snapshot(), "in_progress");

        state["rounds"][0]["status"] = "awaiting_decision".into();
        state["rounds"][0]["dispatched_at"] = 2.into();
        state["rounds"][0]["finished_at"] = 3.into();
        state["rounds"][0]["freshness"] = "fresh".into();
        write_state(&state);
        assert_eq!(snapshot(), "in_progress");
        drop(guard);

        // A concurrently spawned child can briefly inherit the lock before
        // exec closes its descriptor, so release need not be instantaneous.
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let status = snapshot();
            if status != "in_progress" || Instant::now() >= deadline {
                assert_eq!(status, "needs_attention");
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        state["rounds"][0]["status"] = "starting".into();
        state["rounds"][0]["dispatched_at"] = serde_json::Value::Null;
        state["rounds"][0]["finished_at"] = serde_json::Value::Null;
        state["rounds"][0]["freshness"] = serde_json::Value::Null;
        write_state(&state);
        assert_eq!(snapshot(), "needs_attention");
        let probe = storage::probe_lock(&runner).unwrap();
        assert!(matches!(probe, storage::LockProbe::Available(_)));
        assert_eq!(snapshot(), "needs_attention");
        drop(probe);

        state["rounds"][0]["status"] = "awaiting_decision".into();
        state["rounds"][0]["dispatched_at"] = 2.into();
        state["rounds"][0]["finished_at"] = 3.into();
        state["rounds"][0]["freshness"] = "fresh".into();
        state["rounds"][0]["cleanup_pending"] = false.into();
        write_state(&state);
        assert_eq!(snapshot(), "complete");
    }
}
