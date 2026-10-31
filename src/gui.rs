//! Loopback HTTP server for the embedded operator UI and local request snapshots.

use crate::{request_commands, review_commands, storage};
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
    routing::{get, post},
};
use futures_util::{Stream, stream};
use include_dir::{Dir, include_dir};
use review_platform::{
    Authorization, CommentSource, PrKey, RefreshOutcome, StateError,
    github::GithubError,
    review::{RoundRecord, RoundStatus},
};
use serde::{Deserialize, Serialize};
use std::{
    convert::Infallible,
    fmt::Write as _,
    io::IsTerminal,
    net::{Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use subtle::ConstantTimeEq;
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
    error: String,
}

type Refresh = fn(&Path, &PrKey) -> Result<RefreshOutcome, request_commands::RefreshError>;

struct AppState {
    state_file: PathBuf,
    session: Mutex<Session>,
    refresh: Refresh,
}

struct Session {
    bootstrap: Option<String>,
    token: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionRequest {
    code: String,
}

#[derive(Serialize)]
struct SessionResponse {
    token: String,
}

#[derive(Serialize)]
struct RefreshResponse {
    changed: bool,
    head_changed: bool,
    discussion_changed: bool,
    authorization_cleared: bool,
}

#[derive(PartialEq, Eq)]
enum LastEvent {
    Snapshot(String),
    ReadError,
}

pub fn run(state_override: Option<PathBuf>) -> Result<()> {
    if !std::io::stdout().is_terminal() {
        anyhow::bail!("GUI requires a terminal to display its one-time sign-in code");
    }
    let state_file = storage::resolve_path(state_override)?;
    let bootstrap = random_secret()?;
    let token = random_secret()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting GUI runtime")?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(ADDRESS)
            .await
            .context("binding GUI to 127.0.0.1:8080")?;
        println!("Review Platform GUI: http://localhost:8080/");
        println!("One-time sign-in code: {bootstrap}");
        let session = Session {
            bootstrap: Some(bootstrap),
            token,
        };
        axum::serve(
            listener,
            router(state_file, session, request_commands::refresh),
        )
        .await
        .context("serving GUI on 127.0.0.1:8080")
    })
}

fn random_secret() -> Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::fill(&mut bytes)
        .map_err(|error| anyhow::anyhow!("generating GUI secret: {error}"))?;
    let mut result = String::with_capacity(64);
    for byte in bytes {
        write!(result, "{byte:02x}").context("formatting GUI secret")?;
    }
    Ok(result)
}

fn router(state_file: PathBuf, session: Session, refresh: Refresh) -> Router {
    let state = Arc::new(AppState {
        state_file,
        session: Mutex::new(session),
        refresh,
    });
    Router::new()
        .route("/", get(index))
        .route("/api", get(api_info))
        .route("/api/hello", get(hello))
        .route("/api/requests", get(requests))
        .route("/api/requests/events", get(request_events))
        .route("/api/session", get(check_session).post(start_session))
        .route("/api/requests/refresh", post(refresh_request))
        .route("/assets/{*path}", get(asset))
        .with_state(state)
        .layer(from_fn(localhost_guard))
}

async fn localhost_guard(request: Request<axum::body::Body>, next: Next) -> Response {
    let valid_host = request
        .headers()
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|host| {
            host.eq_ignore_ascii_case("localhost:8080") || host == "127.0.0.1:8080"
        });
    let mut response = if !valid_host {
        StatusCode::MISDIRECTED_REQUEST.into_response()
    } else if *request.method() == Method::POST {
        let host = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        let origin = request
            .headers()
            .get(header::ORIGIN)
            .and_then(|value| value.to_str().ok());
        if origin.is_some_and(|value| value == format!("http://{}", host.to_ascii_lowercase())) {
            next.run(request).await
        } else {
            StatusCode::FORBIDDEN.into_response()
        }
    } else if !matches!(*request.method(), Method::GET | Method::HEAD) {
        StatusCode::METHOD_NOT_ALLOWED.into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

fn secret_matches(expected: &str, actual: &str) -> bool {
    expected.as_bytes().ct_eq(actual.as_bytes()).into()
}

async fn start_session(
    HttpState(state): HttpState<Arc<AppState>>,
    Json(request): Json<SessionRequest>,
) -> Result<Json<SessionResponse>, StatusCode> {
    let mut session = state
        .session
        .lock()
        .map_err(|_| StatusCode::INTERNAL_SERVER_ERROR)?;
    if !session
        .bootstrap
        .as_ref()
        .is_some_and(|code| secret_matches(code, &request.code))
    {
        return Err(StatusCode::UNAUTHORIZED);
    }
    session.bootstrap = None;
    Ok(Json(SessionResponse {
        token: session.token.clone(),
    }))
}

fn has_session(state: &AppState, headers: &axum::http::HeaderMap) -> bool {
    headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .is_some_and(|token| {
            state.session.lock().is_ok_and(|session| {
                session.bootstrap.is_none() && secret_matches(&session.token, token)
            })
        })
}

async fn check_session(
    HttpState(state): HttpState<Arc<AppState>>,
    headers: axum::http::HeaderMap,
) -> StatusCode {
    if has_session(&state, &headers) {
        StatusCode::NO_CONTENT
    } else {
        StatusCode::UNAUTHORIZED
    }
}

async fn refresh_request(
    HttpState(state): HttpState<Arc<AppState>>,
    headers: axum::http::HeaderMap,
    Json(key): Json<PrKey>,
) -> Result<Json<RefreshResponse>, (StatusCode, Json<ApiError>)> {
    if !has_session(&state, &headers) {
        return Err(api_error(
            StatusCode::UNAUTHORIZED,
            "Sign in again to use GUI actions.",
        ));
    }
    let state_file = state.state_file.clone();
    let refresh = state.refresh;
    let outcome = tokio::task::spawn_blocking(move || refresh(&state_file, &key))
        .await
        .map_err(|_| {
            api_error(
                StatusCode::INTERNAL_SERVER_ERROR,
                "The refresh task failed.",
            )
        })?
        .map_err(refresh_error)?;
    let response = match outcome {
        RefreshOutcome::Unchanged => RefreshResponse {
            changed: false,
            head_changed: false,
            discussion_changed: false,
            authorization_cleared: false,
        },
        RefreshOutcome::Updated {
            head_changed,
            discussion_changed,
            authorization_cleared,
        } => RefreshResponse {
            changed: true,
            head_changed,
            discussion_changed,
            authorization_cleared,
        },
    };
    Ok(Json(response))
}

fn api_error(status: StatusCode, message: impl Into<String>) -> (StatusCode, Json<ApiError>) {
    (
        status,
        Json(ApiError {
            error: message.into(),
        }),
    )
}

fn refresh_error(error: request_commands::RefreshError) -> (StatusCode, Json<ApiError>) {
    use request_commands::RefreshError;
    match error {
        RefreshError::State(StateError::RequestNotFound(_)) => {
            api_error(StatusCode::NOT_FOUND, "Review request no longer exists.")
        }
        RefreshError::State(StateError::RevisionMismatch { .. }) => api_error(
            StatusCode::CONFLICT,
            "Review inputs changed during refresh. Try again.",
        ),
        RefreshError::State(_) | RefreshError::Storage(_) => api_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "Could not update local review state.",
        ),
        RefreshError::Github(
            GithubError::InvalidCredentials
            | GithubError::InvalidTokenHeader
            | GithubError::TokenNotUtf8,
        ) => api_error(
            StatusCode::BAD_GATEWAY,
            "Check REVIEW_PLATFORM_GITHUB_TOKEN_RO and restart the GUI.",
        ),
        RefreshError::Github(
            GithubError::AuthenticationRequired { .. } | GithubError::PermissionDenied { .. },
        ) => api_error(
            StatusCode::BAD_GATEWAY,
            "Check GitHub read access and REVIEW_PLATFORM_GITHUB_TOKEN_RO.",
        ),
        RefreshError::Github(GithubError::RateLimited) => api_error(
            StatusCode::BAD_GATEWAY,
            "GitHub rate limit reached. Try again later.",
        ),
        RefreshError::Github(GithubError::NotFound { .. }) => api_error(
            StatusCode::BAD_GATEWAY,
            "The pull request was not found or is not accessible on GitHub.",
        ),
        RefreshError::Github(
            GithubError::ObservationChanged
            | GithubError::AuthorChanged { .. }
            | GithubError::HeadChanged { .. }
            | GithubError::PaginationChanged { .. },
        ) => api_error(
            StatusCode::CONFLICT,
            "GitHub inputs changed during refresh. Try again.",
        ),
        RefreshError::Github(_) => api_error(
            StatusCode::BAD_GATEWAY,
            "Could not refresh from GitHub. Check access and try again.",
        ),
    }
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
    HttpState(state): HttpState<Arc<AppState>>,
) -> Result<Json<RequestList>, (StatusCode, Json<ApiError>)> {
    read_request_list(state.state_file.clone())
        .await
        .map(Json)
        .map_err(|error| {
            eprintln!("error loading GUI request list: {error:#}");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(ApiError {
                    error: "Could not read local review state.".into(),
                }),
            )
        })
}

async fn request_events(
    HttpState(state): HttpState<Arc<AppState>>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let mut ticker = interval(STATE_CHECK_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let updates = stream::unfold(
        (state.state_file.clone(), ticker, None::<LastEvent>),
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
        Ok(storage::LockProbe::Available) => {
            // The runner might have saved its final state after our initial read.
            // The probe has released its lock before this reread.
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
    use axum::body::{Body, to_bytes};
    use futures_util::StreamExt;
    use review_platform::{
        AuthorizationScope, CommentFingerprint, CommentHash, HeadSha, InputRevision,
        ObservedDiscussion, PrKey, State,
    };
    use std::num::NonZeroU64;
    use std::time::{Duration, Instant};
    use tower::ServiceExt;

    fn test_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
    }

    fn test_router(path: PathBuf) -> Router {
        router(
            path,
            Session {
                bootstrap: Some("test-code".into()),
                token: "test-token".into(),
            },
            request_commands::refresh,
        )
    }

    fn fake_refresh(
        path: &Path,
        key: &PrKey,
    ) -> Result<RefreshOutcome, request_commands::RefreshError> {
        if key.number() == 16 {
            return Err(GithubError::RateLimited.into());
        }
        let mut state = storage::load(path).unwrap();
        let head = HeadSha::new("b".repeat(40)).unwrap();
        let discussion = ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap();
        let outcome = state.refresh(key, &head, &discussion)?;
        storage::save(path, &state).unwrap();
        Ok(outcome)
    }

    fn json_post(
        uri: &str,
        origin: Option<&str>,
        bearer: Option<&str>,
        json: &str,
    ) -> Request<Body> {
        let mut builder = Request::builder()
            .method(Method::POST)
            .uri(uri)
            .header(header::HOST, "localhost:8080")
            .header(header::CONTENT_TYPE, "application/json");
        if let Some(origin) = origin {
            builder = builder.header(header::ORIGIN, origin);
        }
        if let Some(token) = bearer {
            builder = builder.header(header::AUTHORIZATION, format!("Bearer {token}"));
        }
        builder.body(Body::from(json.to_owned())).unwrap()
    }

    #[test]
    fn actions_require_one_time_session_and_same_origin() {
        let directory = tempfile::tempdir().unwrap();
        let state_file = directory.path().join("state.json");
        let key = PrKey::new("owner/repo", 15).unwrap();
        let head = HeadSha::new("a".repeat(40)).unwrap();
        let discussion = ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap();
        let mut state = State::default();
        state.record_request(key.clone(), head.clone()).unwrap();
        state.refresh(&key, &head, &discussion).unwrap();
        state
            .authorize(
                &key,
                &head,
                InputRevision::new(2).unwrap(),
                AuthorizationScope::OneRound,
            )
            .unwrap();
        storage::save(&state_file, &state).unwrap();
        let original = std::fs::read(&state_file).unwrap();
        let app = router(
            state_file.clone(),
            Session {
                bootstrap: Some("test-code".into()),
                token: "test-token".into(),
            },
            fake_refresh,
        );
        test_runtime().block_on(async {
            let session_path = "/api/session";
            let refresh_path = "/api/requests/refresh";
            let refresh_body = r#"{"repository":"owner/repo","number":15}"#;
            for origin in [
                None,
                Some("null"),
                Some("http://evil.example"),
                Some("http://localhost:8081"),
                Some("http://127.0.0.1:8080"),
            ] {
                let response = app
                    .clone()
                    .oneshot(json_post(
                        session_path,
                        origin,
                        None,
                        r#"{"code":"test-code"}"#,
                    ))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::FORBIDDEN,
                    "origin: {origin:?}"
                );
                let response = app
                    .clone()
                    .oneshot(json_post(
                        refresh_path,
                        origin,
                        Some("test-token"),
                        refresh_body,
                    ))
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    StatusCode::FORBIDDEN,
                    "origin: {origin:?}"
                );
            }
            for token in [None, Some("test-token"), Some("wrong")] {
                let response = app
                    .clone()
                    .oneshot(json_post(
                        refresh_path,
                        Some("http://localhost:8080"),
                        token,
                        refresh_body,
                    ))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            }
            let response = app
                .clone()
                .oneshot(json_post(
                    session_path,
                    Some("http://localhost:8080"),
                    None,
                    r#"{"code":"wrong"}"#,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(std::fs::read(&state_file).unwrap(), original);

            let response = app
                .clone()
                .oneshot(json_post(
                    session_path,
                    Some("http://localhost:8080"),
                    None,
                    r#"{"code":"test-code"}"#,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&body).unwrap()["token"],
                "test-token"
            );
            let response = app
                .clone()
                .oneshot(json_post(
                    session_path,
                    Some("http://localhost:8080"),
                    None,
                    r#"{"code":"test-code"}"#,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            for token in [None, Some("wrong")] {
                let response = app
                    .clone()
                    .oneshot(json_post(
                        refresh_path,
                        Some("http://localhost:8080"),
                        token,
                        refresh_body,
                    ))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            }
            for origin in [
                None,
                Some("null"),
                Some("http://evil.example"),
                Some("http://localhost:8081"),
                Some("http://127.0.0.1:8080"),
            ] {
                let response = app
                    .clone()
                    .oneshot(json_post(
                        refresh_path,
                        origin,
                        Some("test-token"),
                        refresh_body,
                    ))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN);
            }
            let response = app
                .clone()
                .oneshot(json_post(
                    refresh_path,
                    Some("http://localhost:8080"),
                    Some("test-token"),
                    r#"{"repository":"../bad","number":0}"#,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
            let response = app
                .clone()
                .oneshot(json_post(
                    refresh_path,
                    Some("http://localhost:8080"),
                    Some("test-token"),
                    r#"{"repository":"owner/repo","number":16}"#,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
            assert_eq!(std::fs::read(&state_file).unwrap(), original);

            let response = app
                .clone()
                .oneshot(json_post(
                    refresh_path,
                    Some("http://localhost:8080"),
                    Some("test-token"),
                    refresh_body,
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let body = to_bytes(response.into_body(), 1024).await.unwrap();
            let result: serde_json::Value = serde_json::from_slice(&body).unwrap();
            assert_eq!(result["head_changed"], true);
            assert_eq!(result["authorization_cleared"], true);
            let saved = storage::load(&state_file).unwrap();
            let request = saved.get(&key).unwrap();
            assert_eq!(request.authorization(), Authorization::AwaitingLead);
            assert_eq!(request.head_sha().to_string(), "b".repeat(40));
        });
    }

    fn http_request(method: Method, uri: &str, host: Option<&str>) -> Request<Body> {
        let mut builder = Request::builder().method(method).uri(uri);
        if let Some(host) = host {
            builder = builder.header(header::HOST, host);
        }
        builder.body(Body::empty()).unwrap()
    }

    fn assert_response_headers(response: &Response) {
        assert_eq!(
            response.headers()[header::X_CONTENT_TYPE_OPTIONS],
            "nosniff"
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert!(
            !response
                .headers()
                .contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN)
        );
    }

    #[test]
    fn router_enforces_loopback_host_and_read_only_methods() {
        let directory = tempfile::tempdir().unwrap();
        let app = test_router(directory.path().join("state.json"));
        test_runtime().block_on(async {
            for host in ["localhost:8080", "LOCALHOST:8080", "127.0.0.1:8080"] {
                for method in [Method::GET, Method::HEAD] {
                    let response = app
                        .clone()
                        .oneshot(http_request(method, "/api", Some(host)))
                        .await
                        .unwrap();
                    assert_eq!(response.status(), StatusCode::OK, "host: {host}");
                    assert_response_headers(&response);
                }
            }
            for host in [None, Some("evil.example:8080"), Some("localhost:8081")] {
                let response = app
                    .clone()
                    .oneshot(http_request(Method::GET, "/api", host))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST);
                assert_response_headers(&response);
            }
            for method in [Method::PUT, Method::DELETE, Method::OPTIONS] {
                let response = app
                    .clone()
                    .oneshot(http_request(method, "/api", Some("localhost:8080")))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
                assert_response_headers(&response);
            }
        });
    }

    #[test]
    fn router_serves_index_with_csp_and_rejects_unsafe_asset_paths() {
        let directory = tempfile::tempdir().unwrap();
        let app = test_router(directory.path().join("state.json"));
        test_runtime().block_on(async {
            let response = app
                .clone()
                .oneshot(http_request(Method::GET, "/", Some("localhost:8080")))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_response_headers(&response);
            let csp = response.headers()[header::CONTENT_SECURITY_POLICY]
                .to_str()
                .unwrap();
            for directive in [
                "default-src 'self'",
                "connect-src 'self'",
                "object-src 'none'",
                "frame-ancestors 'none'",
            ] {
                assert!(
                    csp.contains(directive),
                    "missing CSP directive: {directive}"
                );
            }

            for path in [
                "/assets/../index.html",
                "/assets/%2e%2e/index.html",
                "/assets//index.js",
                "/assets/dir//index.js",
                "/assets/unknown.xyz",
            ] {
                let response = app
                    .clone()
                    .oneshot(http_request(Method::GET, path, Some("localhost:8080")))
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::NOT_FOUND, "path: {path}");
                assert_response_headers(&response);
            }
        });
    }

    #[test]
    fn request_endpoint_returns_empty_and_sorted_local_snapshots() {
        let directory = tempfile::tempdir().unwrap();
        let state_file = directory.path().join("state.json");
        let app = test_router(state_file.clone());
        test_runtime().block_on(async {
            let response = app
                .clone()
                .oneshot(http_request(
                    Method::GET,
                    "/api/requests",
                    Some("localhost:8080"),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_response_headers(&response);
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            assert_eq!(
                serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
                serde_json::json!({"requests": []})
            );

            let mut state = State::default();
            let head = HeadSha::new("a".repeat(40)).unwrap();
            for (repository, number) in [("z/repo", 1), ("a/repo", 9), ("a/repo", 2)] {
                state
                    .record_request(PrKey::new(repository, number).unwrap(), head.clone())
                    .unwrap();
            }
            let discussion = ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap();
            for (number, scope) in [
                (2, AuthorizationScope::PrCycle),
                (9, AuthorizationScope::OneRound),
            ] {
                let key = PrKey::new("a/repo", number).unwrap();
                state.refresh(&key, &head, &discussion).unwrap();
                state
                    .authorize(&key, &head, InputRevision::new(2).unwrap(), scope)
                    .unwrap();
            }
            storage::save(&state_file, &state).unwrap();

            let response = app
                .oneshot(http_request(
                    Method::GET,
                    "/api/requests",
                    Some("localhost:8080"),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
            let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let requests = body["requests"].as_array().unwrap();
            assert_eq!(
                (
                    requests[0]["repository"].as_str(),
                    requests[0]["number"].as_u64()
                ),
                (Some("a/repo"), Some(2))
            );
            assert_eq!(
                (
                    requests[1]["repository"].as_str(),
                    requests[1]["number"].as_u64()
                ),
                (Some("a/repo"), Some(9))
            );
            assert_eq!(
                (
                    requests[2]["repository"].as_str(),
                    requests[2]["number"].as_u64()
                ),
                (Some("z/repo"), Some(1))
            );
            assert_eq!(requests[0]["authorization"], "pr_cycle");
            assert_eq!(requests[1]["authorization"], "one_round");
            assert_eq!(requests[2]["authorization"], "awaiting_lead");
            assert!(requests[0]["latest_round"].is_null());
        });
    }

    #[test]
    fn request_events_send_changes_errors_and_recovery() {
        let directory = tempfile::tempdir().unwrap();
        let state_file = directory.path().join("state.json");
        let app = test_router(state_file.clone());
        test_runtime().block_on(async {
            let response = app
                .oneshot(http_request(
                    Method::GET,
                    "/api/requests/events",
                    Some("localhost:8080"),
                ))
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_response_headers(&response);
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/event-stream"
            );
            let mut stream = response.into_body().into_data_stream();

            let first = tokio::time::timeout(Duration::from_secs(3), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(
                std::str::from_utf8(&first).unwrap(),
                "event: requests\ndata: {\"requests\":[]}\n\n"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(1200), stream.next())
                    .await
                    .is_err()
            );

            let mut state = State::default();
            state
                .record_request(
                    PrKey::new("owner/repo", 15).unwrap(),
                    HeadSha::new("a".repeat(40)).unwrap(),
                )
                .unwrap();
            storage::save(&state_file, &state).unwrap();
            let changed = tokio::time::timeout(Duration::from_secs(3), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let changed_text = std::str::from_utf8(&changed).unwrap();
            let json = changed_text
                .strip_prefix("event: requests\ndata: ")
                .unwrap()
                .strip_suffix("\n\n")
                .unwrap();
            let snapshot: serde_json::Value = serde_json::from_str(json).unwrap();
            assert_eq!(snapshot["requests"][0]["repository"], "owner/repo");
            assert_eq!(snapshot["requests"][0]["number"], 15);

            std::fs::write(&state_file, b"invalid state").unwrap();
            let error = tokio::time::timeout(Duration::from_secs(3), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(
                std::str::from_utf8(&error).unwrap(),
                "event: state_error\ndata: Could not read local review state.\n\n"
            );
            assert!(
                tokio::time::timeout(Duration::from_millis(1200), stream.next())
                    .await
                    .is_err()
            );

            storage::save(&state_file, &state).unwrap();
            let recovered = tokio::time::timeout(Duration::from_secs(3), stream.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert_eq!(recovered, changed);
        });
    }

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
        let summary = serde_json::to_value(request_list(&state_file).unwrap()).unwrap()["requests"]
            [0]["latest_round"]
            .clone();
        assert_eq!(
            summary,
            serde_json::json!({
                "id": "r-test",
                "status": "running",
                "cleanup_pending": true,
                "cleanup_status": "unknown",
                "head_sha": "a".repeat(40),
                "revision": 2,
                "model": "model-alpha",
                "agent": "claude"
            })
        );
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
        assert!(matches!(probe, storage::LockProbe::Available));
        assert_eq!(snapshot(), "needs_attention");

        state["rounds"][0]["status"] = "awaiting_decision".into();
        state["rounds"][0]["dispatched_at"] = 2.into();
        state["rounds"][0]["finished_at"] = 3.into();
        state["rounds"][0]["freshness"] = "fresh".into();
        state["rounds"][0]["cleanup_pending"] = false.into();
        write_state(&state);
        assert_eq!(snapshot(), "complete");
    }
}
