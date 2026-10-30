//! Loopback HTTP server for the embedded operator UI and its API.

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::Path as UrlPath,
    http::{HeaderValue, Method, Request, StatusCode, header},
    middleware::{Next, from_fn},
    response::{IntoResponse, Response},
    routing::get,
};
use include_dir::{Dir, include_dir};
use serde::Serialize;
use std::{
    net::{Ipv4Addr, SocketAddr},
    path::Path,
};

const ADDRESS: SocketAddr = SocketAddr::new(std::net::IpAddr::V4(Ipv4Addr::LOCALHOST), 8080);
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

pub fn run() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting GUI runtime")?;
    runtime.block_on(async {
        let listener = tokio::net::TcpListener::bind(ADDRESS)
            .await
            .context("binding GUI to 127.0.0.1:8080")?;
        println!("Review Platform GUI: http://localhost:8080/");
        axum::serve(listener, router())
            .await
            .context("serving GUI on 127.0.0.1:8080")
    })
}

fn router() -> Router {
    Router::new()
        .route("/", get(index))
        .route("/api", get(api_info))
        .route("/api/hello", get(hello))
        .route("/assets/{*path}", get(asset))
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
