//! Loopback HTTP server for the embedded operator UI and local request snapshots.

use crate::storage;
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
use review_platform::{Authorization, review::RoundStatus};
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
    latest_round: Option<RoundSummary>,
}

#[derive(Serialize)]
struct RoundSummary {
    id: String,
    status: RoundStatus,
    cleanup_pending: bool,
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
        .map(|request| RequestSummary {
            repository: request.key().repository().to_owned(),
            number: request.key().number(),
            head_sha: request.head_sha().to_string(),
            revision: request.revision().get(),
            authorization: request.authorization(),
            latest_round: state.latest_round(request.key()).map(|round| RoundSummary {
                id: round.id().to_string(),
                status: round.status(),
                cleanup_pending: round.cleanup_pending(),
            }),
        })
        .collect();
    Ok(RequestList { requests })
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
