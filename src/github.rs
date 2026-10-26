//! Read-only GitHub REST API adapter for PR metadata, diffs, and checks.

pub mod checks;
mod error;

pub use error::GithubError;

use crate::{HeadSha, PrKey};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::io::Read;
use std::time::Duration;

const API_ROOT: &str = "https://api.github.com";
const MAX_DIFF_BYTES: u64 = 8 * 1024 * 1024;

type HttpResponse = ureq::http::Response<ureq::Body>;

pub type GithubResult<T> = std::result::Result<T, GithubError>;

pub struct GithubClient {
    agent: ureq::Agent,
    api_root: String,
    token: Option<String>,
}

/// GitHub PR metadata obtained through [`GithubClient::pull_request`].
pub struct PullRequest {
    title: String,
    author: String,
    state: PullRequestState,
    draft: bool,
    head_sha: HeadSha,
    base_sha: HeadSha,
}

impl PullRequest {
    pub fn title(&self) -> &str {
        &self.title
    }

    pub fn author(&self) -> &str {
        &self.author
    }

    pub fn state(&self) -> PullRequestState {
        self.state
    }

    pub fn draft(&self) -> bool {
        self.draft
    }

    pub fn head_sha(&self) -> &HeadSha {
        &self.head_sha
    }

    pub fn base_sha(&self) -> &HeadSha {
        &self.base_sha
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PullRequestState {
    Open,
    Closed,
}

impl std::fmt::Display for PullRequestState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Open => f.write_str("open"),
            Self::Closed => f.write_str("closed"),
        }
    }
}

#[derive(Deserialize)]
struct ApiPullRequest {
    number: u64,
    title: String,
    state: PullRequestState,
    draft: bool,
    user: ApiUser,
    head: ApiHead,
    base: ApiHead,
}

#[derive(Deserialize)]
struct ApiUser {
    login: String,
}

#[derive(Deserialize)]
struct ApiHead {
    sha: HeadSha,
}

#[derive(Deserialize)]
struct ApiComparison {
    merge_base_commit: ApiHead,
}

impl GithubClient {
    pub fn new() -> GithubResult<Self> {
        let token = match std::env::var("REVIEW_PLATFORM_GITHUB_TOKEN_RO") {
            Ok(token) if !token.is_empty() => Some(token),
            Ok(_) | Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => return Err(GithubError::TokenNotUtf8),
        };
        Ok(Self::for_api(API_ROOT.to_owned(), token))
    }

    fn for_api(api_root: String, token: Option<String>) -> Self {
        let config = ureq::Agent::config_builder()
            .timeout_connect(Some(Duration::from_secs(5)))
            .timeout_per_read(Some(Duration::from_secs(15)))
            .max_redirects(0)
            .http_status_as_error(false)
            .user_agent(concat!("review-platform/", env!("CARGO_PKG_VERSION")))
            .build();
        let agent = ureq::Agent::new_with_config(config);
        Self {
            agent,
            api_root,
            token,
        }
    }

    pub fn pull_request(&self, key: &PrKey) -> GithubResult<PullRequest> {
        let route = pull_route(key);
        let response = self.get(
            &route,
            "application/vnd.github+json",
            &format!("pull request {key}"),
        )?;
        let raw: ApiPullRequest = parse_json(response, format!("pull request {key}"))?;
        if raw.number != key.number() {
            return Err(GithubError::InvalidResponse {
                resource: format!("pull request {key}"),
                detail: format!("returned PR #{}", raw.number),
            });
        }
        Ok(PullRequest {
            title: raw.title,
            author: raw.user.login,
            state: raw.state,
            draft: raw.draft,
            head_sha: raw.head.sha,
            base_sha: raw.base.sha,
        })
    }

    /// Return the PR diff, or changes after a supplied ancestor commit.
    /// Recheck the live PR before returning mutable diff data to the caller.
    pub fn diff_for_pull(
        &self,
        key: &PrKey,
        pull: &PullRequest,
        since: Option<&HeadSha>,
    ) -> GithubResult<String> {
        let expected_head = &pull.head_sha;
        let route = if let Some(previous_head) = since {
            if previous_head == expected_head {
                self.ensure_unchanged(key, pull, false)?;
                return Ok(String::new());
            }
            let route = format!(
                "/repos/{}/compare/{}...{}",
                key.repository(),
                previous_head,
                expected_head
            );
            let response = self.get(
                &route,
                "application/vnd.github+json",
                &format!("comparison for {key}"),
            )?;
            let comparison: ApiComparison = parse_json(response, format!("comparison for {key}"))?;
            if &comparison.merge_base_commit.sha != previous_head {
                return Err(GithubError::DivergentHistory {
                    previous: previous_head.clone(),
                    current: expected_head.clone(),
                });
            }
            route
        } else {
            pull_route(key)
        };

        let response = self
            .get(
                &route,
                "application/vnd.github.diff",
                &format!("diff for {key}"),
            )
            .map_err(|error| match error {
                GithubError::HttpStatus { status: 406, .. } => {
                    GithubError::DiffUnavailable { key: key.clone() }
                }
                other => other,
            })?;
        let content_type = response
            .headers()
            .get("Content-Type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default();
        if !content_type.starts_with("application/vnd.github.diff") {
            return Err(GithubError::UnexpectedDiffMediaType {
                key: key.clone(),
                actual: content_type.to_owned(),
            });
        }
        let mut bytes = Vec::new();
        response
            .into_body()
            .into_reader()
            .take(MAX_DIFF_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|source| GithubError::ReadResponse {
                resource: format!("GitHub diff for {key}"),
                source,
            })?;
        if bytes.len() as u64 > MAX_DIFF_BYTES {
            return Err(GithubError::DiffTooLarge {
                key: key.clone(),
                max_mib: MAX_DIFF_BYTES / (1024 * 1024),
            });
        }
        let diff = String::from_utf8(bytes).map_err(|source| GithubError::DiffNotUtf8 {
            key: key.clone(),
            source,
        })?;
        self.ensure_unchanged(key, pull, since.is_none())?;
        Ok(diff)
    }

    fn ensure_unchanged(
        &self,
        key: &PrKey,
        pull: &PullRequest,
        check_base: bool,
    ) -> GithubResult<()> {
        let current = self.pull_request(key)?;
        if current.head_sha != pull.head_sha {
            return Err(GithubError::HeadChanged {
                key: key.clone(),
                expected: pull.head_sha.clone(),
                actual: current.head_sha,
            });
        }
        if check_base && current.base_sha != pull.base_sha {
            return Err(GithubError::BaseChanged {
                key: key.clone(),
                expected: pull.base_sha.clone(),
                actual: current.base_sha,
            });
        }
        Ok(())
    }

    fn get(&self, route: &str, accept: &str, description: &str) -> GithubResult<HttpResponse> {
        let url = format!("{}{}", self.api_root, route);
        let mut request = self
            .agent
            .get(url.as_str())
            .header("Accept", accept)
            .header("X-GitHub-Api-Version", "2022-11-28");
        if let Some(token) = &self.token {
            if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
                return Err(GithubError::InvalidTokenHeader);
            }
            request = request.header("Authorization", format!("Bearer {token}"));
        }

        let response = request.call().map_err(|source| GithubError::Transport {
            resource: description.to_owned(),
            source: Box::new(source),
        })?;
        let status = response.status().as_u16();
        if status != 200 {
            return Err(github_status_error(
                status,
                response.headers(),
                description,
                self.token.is_some(),
            ));
        }
        Ok(response)
    }
}

fn pull_route(key: &PrKey) -> String {
    format!("/repos/{}/pulls/{}", key.repository(), key.number())
}

fn parse_json<T: DeserializeOwned>(response: HttpResponse, resource: String) -> GithubResult<T> {
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .read_to_end(&mut bytes)
        .map_err(|source| GithubError::ReadResponse {
            resource: resource.clone(),
            source,
        })?;
    serde_json::from_slice(&bytes)
        .map_err(|source| GithubError::DecodeResponse { resource, source })
}

fn github_status_error(
    status: u16,
    headers: &ureq::http::HeaderMap,
    description: &str,
    token_configured: bool,
) -> GithubError {
    match status {
        401 if token_configured => GithubError::InvalidCredentials,
        401 => GithubError::AuthenticationRequired {
            resource: description.to_owned(),
        },
        429 => GithubError::RateLimited,
        403 if headers
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok())
            == Some("0")
            || headers.contains_key("retry-after") =>
        {
            GithubError::RateLimited
        }
        404 => GithubError::NotFound {
            resource: description.to_owned(),
        },
        status => GithubError::HttpStatus {
            status,
            resource: description.to_owned(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::checks::{CheckRunStatus, CheckSummary};
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Instant;

    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PREVIOUS_HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const BASE: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const NEW_BASE: &str = "dddddddddddddddddddddddddddddddddddddddd";
    const MOCK_TIMEOUT: Duration = Duration::from_secs(10);

    struct MockReply {
        route: String,
        accept: &'static str,
        status: String,
        content_type: &'static str,
        body: String,
        extra_headers: String,
    }

    fn json_reply(route: String, body: String) -> MockReply {
        MockReply {
            route,
            accept: "application/vnd.github+json",
            status: "200 OK".to_owned(),
            content_type: "application/json",
            body,
            extra_headers: String::new(),
        }
    }

    fn diff_reply(route: String, body: String) -> MockReply {
        MockReply {
            route,
            accept: "application/vnd.github.diff",
            status: "200 OK".to_owned(),
            content_type: "application/vnd.github.diff; charset=utf-8",
            body,
            extra_headers: String::new(),
        }
    }

    fn pr_body(head: &str, base: &str) -> String {
        serde_json::json!({
            "number": 42,
            "title": "Review this change",
            "state": "open",
            "draft": false,
            "user": {"login": "author"},
            "head": {"sha": head},
            "base": {"sha": base}
        })
        .to_string()
    }

    fn successful_check_runs(first: u64, last: u64) -> Vec<serde_json::Value> {
        (first..=last)
            .map(|id| {
                serde_json::json!({
                    "id": id,
                    "name": format!("job-{id}"),
                    "head_sha": HEAD,
                    "status": "completed",
                    "conclusion": "success"
                })
            })
            .collect()
    }

    fn check_runs_reply(page: usize, total_count: u64, runs: Vec<serde_json::Value>) -> MockReply {
        json_reply(
            format!(
                "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page={page}&filter=latest"
            ),
            serde_json::json!({"total_count": total_count, "check_runs": runs}).to_string(),
        )
    }

    fn serve_once(
        status: &str,
        body: &str,
        extra_headers: &str,
    ) -> (String, thread::JoinHandle<()>) {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let mut reply = json_reply(pull_route(&key), body.to_owned());
        reply.status = status.to_owned();
        reply.extra_headers = extra_headers.to_owned();
        serve_many(vec![reply])
    }

    fn serve_many(replies: Vec<MockReply>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            for reply in replies {
                let deadline = Instant::now() + MOCK_TIMEOUT;
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(
                                Instant::now() < deadline,
                                "timed out waiting for mock request {}",
                                reply.route
                            );
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("accepting mock request {}: {error}", reply.route),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream.set_read_timeout(Some(MOCK_TIMEOUT)).unwrap();
                stream.set_write_timeout(Some(MOCK_TIMEOUT)).unwrap();
                let mut lines = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                lines.read_line(&mut line).unwrap();
                assert_eq!(line, format!("GET {} HTTP/1.1\r\n", reply.route));
                let mut headers = String::new();
                loop {
                    line.clear();
                    lines.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    headers.push_str(&line.to_ascii_lowercase());
                }
                assert!(
                    headers.contains(&format!("accept: {}", reply.accept)),
                    "{headers}"
                );
                assert!(headers.contains("authorization: bearer test-token"));
                assert!(headers.contains("x-github-api-version: 2022-11-28"));
                // Each reply owns one connection; tell the client not to reuse it.
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n{}",
                    reply.status,
                    reply.content_type,
                    reply.body.len(),
                    reply.extra_headers,
                    reply.body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (url, handle)
    }

    #[test]
    fn reads_the_live_head_with_get_and_read_only_token() {
        let body = serde_json::json!({
            "number": 42,
            "title": "Review this change",
            "state": "open",
            "draft": false,
            "user": {"login": "author"},
            "head": {"sha": HEAD.to_ascii_uppercase()},
            "base": {"sha": BASE}
        })
        .to_string();
        let (url, handle) = serve_once("200 OK", &body, "");
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client
            .pull_request(&PrKey::new("OWNER/REPO", 42).unwrap())
            .unwrap();
        handle.join().unwrap();
        assert_eq!(pull.head_sha().as_str(), HEAD);
        assert_eq!(pull.author(), "author");
        assert_eq!(pull.state(), PullRequestState::Open);
    }

    #[test]
    fn reports_missing_and_malformed_pull_requests() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let (url, handle) = serve_once("404 Not Found", "{}", "");
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .pull_request(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(matches!(&error, GithubError::NotFound { .. }), "{error}");

        let body = serde_json::json!({
            "number": 42,
            "title": "Review this change",
            "state": "open",
            "draft": false,
            "user": {"login": "author"},
            "head": {"sha": "invalid"},
            "base": {"sha": BASE}
        })
        .to_string();
        let (url, handle) = serve_once("200 OK", &body, "");
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .pull_request(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(
            matches!(&error, GithubError::DecodeResponse { .. }),
            "{error}"
        );
    }

    #[test]
    fn rejects_invalid_token_without_exposing_it() {
        let secret = "test-secret\nmore-secret";
        let client = GithubClient::for_api(API_ROOT.to_owned(), Some(secret.to_owned()));
        let error = client
            .pull_request(&PrKey::new("owner/repo", 42).unwrap())
            .err()
            .unwrap();
        assert!(matches!(&error, GithubError::InvalidTokenHeader));
        assert!(!error.to_string().contains(secret));
    }

    #[test]
    fn classifies_authentication_and_rate_limit_responses() {
        let no_headers = ureq::http::HeaderMap::new();
        assert!(matches!(
            github_status_error(401, &no_headers, "pull request", true),
            GithubError::InvalidCredentials
        ));

        assert!(matches!(
            github_status_error(401, &no_headers, "pull request", false),
            GithubError::AuthenticationRequired { .. }
        ));

        assert!(matches!(
            github_status_error(429, &no_headers, "check runs", true),
            GithubError::RateLimited
        ));

        let mut rate_limit_headers = ureq::http::HeaderMap::new();
        rate_limit_headers.insert("x-ratelimit-remaining", "0".parse().unwrap());
        assert!(matches!(
            github_status_error(403, &rate_limit_headers, "check runs", true),
            GithubError::RateLimited
        ));

        let mut retry_headers = ureq::http::HeaderMap::new();
        retry_headers.insert("retry-after", "60".parse().unwrap());
        assert!(matches!(
            github_status_error(403, &retry_headers, "check runs", true),
            GithubError::RateLimited
        ));

        assert!(matches!(
            github_status_error(403, &no_headers, "check runs", true),
            GithubError::HttpStatus { status: 403, .. }
        ));
    }

    #[test]
    fn incremental_diff_uses_the_reviewed_commit_and_rechecks_the_pr_head() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let compare_route = format!("/repos/owner/repo/compare/{PREVIOUS_HEAD}...{HEAD}");
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            json_reply(
                compare_route.clone(),
                serde_json::json!({"merge_base_commit": {"sha": PREVIOUS_HEAD}}).to_string(),
            ),
            diff_reply(compare_route, "diff --git a/a b/a\n+new line\n".to_owned()),
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let current = client.pull_request(&key).unwrap();
        let previous = HeadSha::new(PREVIOUS_HEAD).unwrap();
        let diff = client
            .diff_for_pull(&key, &current, Some(&previous))
            .unwrap();
        handle.join().unwrap();
        assert!(diff.contains("+new line"));
    }

    #[test]
    fn diff_406_points_the_lead_to_github() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let mut unavailable_diff = diff_reply(pull_route(&key), String::new());
        unavailable_diff.status = "406 Not Acceptable".to_owned();
        let (url, handle) = serve_many(vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            unavailable_diff,
        ]);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let error = client.diff_for_pull(&key, &pull, None).unwrap_err();
        handle.join().unwrap();
        assert!(
            matches!(&error, GithubError::DiffUnavailable { key: failed_key } if failed_key == &key),
            "{error}"
        );
        assert!(error.to_string().contains("inspect the PR on GitHub"));
    }

    #[test]
    fn diff_rejects_a_head_change_during_fetch() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            diff_reply(pull_route(&key), "diff --git a/a b/a\n".to_owned()),
            json_reply(pull_route(&key), pr_body(PREVIOUS_HEAD, BASE)),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let error = client.diff_for_pull(&key, &pull, None).err().unwrap();
        handle.join().unwrap();
        assert!(
            matches!(
                &error,
                GithubError::HeadChanged { expected, actual, .. }
                    if expected.as_str() == HEAD && actual.as_str() == PREVIOUS_HEAD
            ),
            "{error}"
        );
    }

    #[test]
    fn full_pr_diff_rejects_a_base_change_during_fetch() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            diff_reply(pull_route(&key), "diff --git a/a b/a\n".to_owned()),
            json_reply(pull_route(&key), pr_body(HEAD, NEW_BASE)),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let error = client.diff_for_pull(&key, &pull, None).err().unwrap();
        handle.join().unwrap();
        assert!(
            matches!(
                &error,
                GithubError::BaseChanged { expected, actual, .. }
                    if expected.as_str() == BASE && actual.as_str() == NEW_BASE
            ),
            "{error}"
        );
    }

    #[test]
    fn incremental_diff_requires_the_reviewed_head_to_be_an_ancestor() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let compare_route = format!("/repos/owner/repo/compare/{PREVIOUS_HEAD}...{HEAD}");
        let body = serde_json::json!({"merge_base_commit": {"sha": HEAD}}).to_string();
        let (url, handle) = serve_many(vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            json_reply(compare_route, body),
        ]);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let error = client
            .diff_for_pull(&key, &pull, Some(&HeadSha::new(PREVIOUS_HEAD).unwrap()))
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(
            matches!(&error, GithubError::DivergentHistory { .. }),
            "{error}"
        );
    }

    #[test]
    fn checks_for_exact_head_ignore_an_empty_pending_legacy_rollup() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let check_runs = serde_json::json!({
            "total_count": 1,
            "check_runs": [{
                "id": 1,
                "name": "build",
                "head_sha": HEAD,
                "status": "completed",
                "conclusion": "success"
            }]
        })
        .to_string();
        let statuses = serde_json::json!({
            "sha": HEAD,
            "state": "pending",
            "total_count": 0,
            "statuses": []
        })
        .to_string();
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=1&filter=latest"
                ),
                check_runs,
            ),
            json_reply(
                format!("/repos/owner/repo/commits/{HEAD}/status?per_page=100&page=1"),
                statuses,
            ),
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let report = client.checks_for_pull(&key, &pull).unwrap();
        handle.join().unwrap();
        assert_eq!(report.summary(), CheckSummary::Green);
        assert_eq!(report.check_runs().len(), 1);
        assert!(report.commit_statuses().is_empty());
    }

    #[test]
    fn check_run_pagination_includes_failures_after_the_first_page() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let first_page =
            serde_json::json!({"total_count": 101, "check_runs": successful_check_runs(1, 100)})
                .to_string();
        let second_page = serde_json::json!({
            "total_count": 101,
            "check_runs": [{
                "id": 101,
                "name": "last-job",
                "head_sha": HEAD,
                "status": "completed",
                "conclusion": "failure"
            }]
        })
        .to_string();
        let statuses = serde_json::json!({
            "sha": HEAD,
            "state": "success",
            "total_count": 1,
            "statuses": [{"context": "legacy-ci", "state": "success"}]
        })
        .to_string();
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=1&filter=latest"
                ),
                first_page.clone(),
            ),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=2&filter=latest"
                ),
                second_page.clone(),
            ),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=1&filter=latest"
                ),
                first_page,
            ),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=2&filter=latest"
                ),
                second_page,
            ),
            json_reply(
                format!("/repos/owner/repo/commits/{HEAD}/status?per_page=100&page=1"),
                statuses,
            ),
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let report = client.checks_for_pull(&key, &pull).unwrap();
        handle.join().unwrap();
        assert_eq!(report.summary(), CheckSummary::Failing);
        assert_eq!(report.check_runs().len(), 101);
        assert_eq!(report.commit_statuses().len(), 1);
    }

    #[test]
    fn check_run_pagination_rejects_a_duplicate_with_unchanged_total() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let first_page =
            serde_json::json!({"total_count": 101, "check_runs": successful_check_runs(1, 100)})
                .to_string();
        let second_page = serde_json::json!({
            "total_count": 101,
            "check_runs": [{
                "id": 100,
                "name": "job-100",
                "head_sha": HEAD,
                "status": "completed",
                "conclusion": "success"
            }]
        })
        .to_string();
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=1&filter=latest"
                ),
                first_page,
            ),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=2&filter=latest"
                ),
                second_page,
            ),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let error = client.checks_for_pull(&key, &pull).err().unwrap();
        handle.join().unwrap();
        assert!(
            matches!(
                &error,
                GithubError::PaginationChanged {
                    kind: "check runs",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn check_run_pagination_rejects_a_replaced_first_page_run() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let first_page =
            serde_json::json!({"total_count": 101, "check_runs": successful_check_runs(1, 100)})
                .to_string();
        let second_page = serde_json::json!({
            "total_count": 101,
            "check_runs": [{
                "id": 101,
                "name": "last-job",
                "head_sha": HEAD,
                "status": "completed",
                "conclusion": "success"
            }]
        })
        .to_string();
        let mut changed_runs = successful_check_runs(1, 100);
        changed_runs[0] = serde_json::json!({
            "id": 102,
            "name": "job-1",
            "head_sha": HEAD,
            "status": "completed",
            "conclusion": "failure"
        });
        let refreshed_page =
            serde_json::json!({"total_count": 101, "check_runs": changed_runs}).to_string();
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=1&filter=latest"
                ),
                first_page,
            ),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=2&filter=latest"
                ),
                second_page.clone(),
            ),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=1&filter=latest"
                ),
                refreshed_page,
            ),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=2&filter=latest"
                ),
                second_page,
            ),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let error = client.checks_for_pull(&key, &pull).err().unwrap();
        handle.join().unwrap();
        assert!(
            matches!(
                &error,
                GithubError::PaginationChanged {
                    kind: "check runs",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn check_run_pagination_rejects_a_skip_after_the_middle_page_changes() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let first_page = successful_check_runs(1, 100);
        let middle_page = successful_check_runs(101, 200);
        // Run 150 disappears after page 2 was fetched. Run 201 moves onto page 2,
        // and a new run 202 fills page 3, leaving the first read with no duplicate.
        let mut refreshed_middle_page = successful_check_runs(101, 149);
        refreshed_middle_page.extend(successful_check_runs(151, 201));
        refreshed_middle_page.last_mut().unwrap()["conclusion"] = "failure".into();
        let last_page = successful_check_runs(202, 202);
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            check_runs_reply(1, 201, first_page.clone()),
            check_runs_reply(2, 201, middle_page),
            check_runs_reply(3, 201, last_page.clone()),
            check_runs_reply(1, 201, first_page),
            check_runs_reply(2, 201, refreshed_middle_page),
            check_runs_reply(3, 201, last_page),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let error = client.checks_for_pull(&key, &pull).err().unwrap();
        handle.join().unwrap();
        assert!(
            matches!(
                &error,
                GithubError::PaginationChanged {
                    kind: "check runs",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn check_run_pagination_allows_status_progress_on_the_same_ids() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let mut first_page = successful_check_runs(1, 100);
        first_page[0]["status"] = "in_progress".into();
        first_page[0]["conclusion"] = serde_json::Value::Null;
        let mut refreshed_first_page = first_page.clone();
        refreshed_first_page[0]["status"] = "completed".into();
        refreshed_first_page[0]["conclusion"] = "success".into();
        let last_page = successful_check_runs(101, 101);
        let statuses = serde_json::json!({
            "sha": HEAD,
            "state": "pending",
            "total_count": 0,
            "statuses": []
        })
        .to_string();
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            check_runs_reply(1, 101, first_page),
            check_runs_reply(2, 101, last_page.clone()),
            check_runs_reply(1, 101, refreshed_first_page),
            check_runs_reply(2, 101, last_page),
            json_reply(
                format!("/repos/owner/repo/commits/{HEAD}/status?per_page=100&page=1"),
                statuses,
            ),
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let report = client.checks_for_pull(&key, &pull).unwrap();
        handle.join().unwrap();
        assert_eq!(report.summary(), CheckSummary::Pending);
        assert_eq!(report.check_runs()[0].status, CheckRunStatus::InProgress);
    }

    #[test]
    fn checks_reject_a_run_for_a_different_head() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let check_runs = serde_json::json!({
            "total_count": 1,
            "check_runs": [{
                "id": 1,
                "name": "build",
                "head_sha": PREVIOUS_HEAD,
                "status": "completed",
                "conclusion": "success"
            }]
        })
        .to_string();
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            json_reply(
                format!(
                    "/repos/owner/repo/commits/{HEAD}/check-runs?per_page=100&page=1&filter=latest"
                ),
                check_runs,
            ),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let error = client.checks_for_pull(&key, &pull).err().unwrap();
        handle.join().unwrap();
        assert!(
            matches!(&error, GithubError::InvalidResponse { .. }),
            "{error}"
        );
    }
}
