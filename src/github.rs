//! Read-only GitHub REST API adapter for PR metadata, diffs, and checks.

pub mod checks;
pub mod commits;
pub mod discussion;
mod error;
mod preparation;

pub use error::GithubError;
pub use preparation::PreparationInputs;

use crate::preparation::{PullAvailability, PullObservation};
use crate::{HeadSha, PrKey};
use serde::Deserialize;
use serde::de::DeserializeOwned;
use std::io::Read;
use std::num::NonZeroU64;
use std::time::Duration;
use ureq::tls::{RootCerts, TlsConfig};

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
    author_id: NonZeroU64,
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

    pub fn author_id(&self) -> u64 {
        self.author_id.get()
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

    pub fn preparation_observation(&self) -> PullObservation<'_> {
        let availability = match self.state {
            PullRequestState::Open => PullAvailability::Open,
            PullRequestState::Closed => PullAvailability::Closed,
        };
        PullObservation::new(&self.head_sha, availability)
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
    id: NonZeroU64,
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
            .tls_config(
                TlsConfig::builder()
                    .root_certs(RootCerts::PlatformVerifier)
                    .build(),
            )
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
            author_id: raw.user.id,
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
                resource: format!("diff for {key}"),
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
        403 => GithubError::PermissionDenied {
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
            "user": {"id": 7, "login": "author"},
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

    fn discussion_page(route: &str, page: usize, items: &[serde_json::Value]) -> MockReply {
        json_reply(
            format!("{route}?per_page=100&page={page}"),
            serde_json::to_string(items).unwrap(),
        )
    }

    fn discussion_collection_replies(route: &str, items: &[serde_json::Value]) -> Vec<MockReply> {
        let mut replies = items
            .chunks(100)
            .enumerate()
            .map(|(index, chunk)| discussion_page(route, index + 1, chunk))
            .collect::<Vec<_>>();
        if items.len() % 100 == 0 {
            replies.push(discussion_page(route, items.len() / 100 + 1, &[]));
        }
        replies
    }

    fn discussion_pass_replies(
        issues: &[serde_json::Value],
        inline: &[serde_json::Value],
        reviews: &[serde_json::Value],
    ) -> Vec<MockReply> {
        let mut replies =
            discussion_collection_replies("/repos/owner/repo/issues/42/comments", issues);
        replies.extend(discussion_collection_replies(
            "/repos/owner/repo/pulls/42/comments",
            inline,
        ));
        replies.extend(discussion_collection_replies(
            "/repos/owner/repo/pulls/42/reviews",
            reviews,
        ));
        replies
    }

    fn observation_replies(
        issues: &[serde_json::Value],
        inline: &[serde_json::Value],
        reviews: &[serde_json::Value],
    ) -> Vec<MockReply> {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let mut replies = vec![json_reply(pull_route(&key), pr_body(HEAD, BASE))];
        replies.extend(discussion_pass_replies(issues, inline, reviews));
        replies.extend(discussion_pass_replies(issues, inline, reviews));
        replies.push(json_reply(pull_route(&key), pr_body(HEAD, BASE)));
        replies
    }

    fn preparation_replies(final_issues: &[serde_json::Value]) -> Vec<MockReply> {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let commits_route = "/repos/owner/repo/pulls/42/commits?per_page=100&page=1";
        let commit = serde_json::json!([{
            "sha": HEAD,
            "commit": {"message": "Change\n\nCo-authored-by: Claude <claude@example.com>"}
        }]);
        let mut replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            check_runs_reply(1, 1, successful_check_runs(1, 1)),
            json_reply(
                format!("/repos/owner/repo/commits/{HEAD}/status?per_page=100&page=1"),
                serde_json::json!({
                    "sha": HEAD,
                    "state": "pending",
                    "total_count": 0,
                    "statuses": []
                })
                .to_string(),
            ),
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            json_reply(commits_route.to_owned(), commit.to_string()),
            json_reply(commits_route.to_owned(), commit.to_string()),
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
        ];
        replies.extend(observation_replies(final_issues, &[], &[]));
        replies
    }

    fn commit_page_replies(commits: &[serde_json::Value]) -> Vec<MockReply> {
        commits
            .chunks(100)
            .enumerate()
            .map(|(index, page)| {
                let mut reply = json_reply(
                    format!(
                        "/repos/owner/repo/pulls/42/commits?per_page=100&page={}",
                        index + 1
                    ),
                    serde_json::to_string(page).unwrap(),
                );
                if (index + 1) * 100 < commits.len() {
                    reply.extra_headers =
                        "Link: <https://api.github.com/next>; rel=\"next\"\r\n".to_owned();
                }
                reply
            })
            .collect()
    }

    fn author_comment(id: u64, body: &str) -> serde_json::Value {
        serde_json::json!({"id": id, "user": {"id": 7}, "body": body})
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
            "user": {"id": 7, "login": "author"},
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
            "user": {"id": 7, "login": "author"},
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
            GithubError::PermissionDenied { .. }
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
    fn check_run_pagination_returns_fresher_status_on_the_same_ids() {
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
        assert_eq!(report.summary(), CheckSummary::Green);
        assert_eq!(report.check_runs()[0].status, CheckRunStatus::Completed);
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

    #[test]
    fn observation_covers_author_discussion_and_ignores_other_users_and_pending_reviews() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let issues = vec![
            author_comment(1, "hello"),
            serde_json::json!({"id": 2, "user": {"id": 8}, "body": "outsider"}),
            serde_json::json!({"id": 8, "user": null, "body": "deleted user"}),
        ];
        let inline = vec![serde_json::json!({
            "id": 3, "user": {"id": 7}, "body": "reply", "in_reply_to_id": 99
        })];
        let reviews = vec![
            serde_json::json!({"id": 4, "user": {"id": 7}, "body": "review body", "state": "COMMENTED"}),
            serde_json::json!({"id": 5, "user": {"id": 7}, "body": "hidden", "state": "PENDING"}),
            serde_json::json!({"id": 6, "user": {"id": 7}, "body": "", "state": "APPROVED"}),
            serde_json::json!({"id": 7, "user": {"id": 8}, "body": "outsider", "state": "COMMENTED"}),
            serde_json::json!({"id": 9, "user": {"id": 7}, "body": null, "state": "COMMENTED"}),
        ];
        let (url, handle) = serve_many(observation_replies(&issues, &inline, &reviews));
        let observation = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .unwrap();
        handle.join().unwrap();
        assert_eq!(observation.key(), &key);
        assert_eq!(observation.head_sha().as_str(), HEAD);
        assert_eq!(observation.author_id(), 7);
        assert_eq!(
            observation
                .discussion()
                .count(crate::CommentSource::IssueComment),
            1
        );
        assert_eq!(
            observation
                .discussion()
                .count(crate::CommentSource::ReviewComment),
            1
        );
        assert_eq!(
            observation.discussion().count(crate::CommentSource::Review),
            1
        );
        let stored = serde_json::to_string(observation.discussion()).unwrap();
        assert!(!stored.contains("hello"));
        assert!(!stored.contains("review body"));
        assert!(
            stored.contains("2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824")
        );
    }

    #[test]
    fn observation_fetches_all_pages_and_detects_a_changed_second_pass() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let issues = (1..=101)
            .map(|id| author_comment(id, &format!("comment {id}")))
            .collect::<Vec<_>>();
        let (url, handle) = serve_many(observation_replies(&issues, &[], &[]));
        let observation = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .unwrap();
        handle.join().unwrap();
        assert_eq!(
            observation
                .discussion()
                .count(crate::CommentSource::IssueComment),
            101
        );

        let mut replies = vec![json_reply(pull_route(&key), pr_body(HEAD, BASE))];
        replies.extend(discussion_pass_replies(
            &[author_comment(1, "before")],
            &[],
            &[],
        ));
        replies.extend(discussion_pass_replies(
            &[author_comment(1, "after")],
            &[],
            &[],
        ));
        let (url, handle) = serve_many(replies);
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(matches!(error, GithubError::ObservationChanged));
        assert!(!error.to_string().contains("before"));
        assert!(!error.to_string().contains("after"));
    }

    #[test]
    fn observation_rechecks_head_and_author_after_collection_reads() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let mut replies = observation_replies(&[], &[], &[]);
        replies.last_mut().unwrap().body = pr_body(PREVIOUS_HEAD, BASE);
        let (url, handle) = serve_many(replies);
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(matches!(error, GithubError::HeadChanged { .. }));

        let mut replies = observation_replies(&[], &[], &[]);
        let mut changed: serde_json::Value = serde_json::from_str(&pr_body(HEAD, BASE)).unwrap();
        changed["user"]["id"] = 8.into();
        replies.last_mut().unwrap().body = changed.to_string();
        let (url, handle) = serve_many(replies);
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(matches!(error, GithubError::AuthorChanged { .. }));
    }

    #[test]
    fn observation_rejects_duplicate_ids_and_malformed_comments() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            discussion_page(
                "/repos/owner/repo/issues/42/comments",
                1,
                &[author_comment(1, "first"), author_comment(1, "second")],
            ),
        ];
        let (url, handle) = serve_many(replies);
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(matches!(
            error,
            GithubError::PaginationChanged {
                kind: "issue comments",
                ..
            }
        ));

        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            discussion_page(
                "/repos/owner/repo/issues/42/comments",
                1,
                &[serde_json::json!({"id": 1, "user": {"id": 7}, "body": null})],
            ),
        ];
        let (url, handle) = serve_many(replies);
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(matches!(error, GithubError::DecodeResponse { .. }));

        let mut first = discussion_page(
            "/repos/owner/repo/issues/42/comments",
            1,
            &[author_comment(1, "first")],
        );
        first.extra_headers = "Link: <https://api.github.com/next>; rel=\"next\"\r\n".to_owned();
        let mut missing_next = discussion_page("/repos/owner/repo/issues/42/comments", 2, &[]);
        missing_next.status = "404 Not Found".to_owned();
        let (url, handle) = serve_many(vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            first,
            missing_next,
        ]);
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(matches!(error, GithubError::NotFound { .. }));
    }

    #[test]
    fn observation_reports_permissions_rate_limits_and_retrieval_failures() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        for (status, extra_headers, expected_rate_limit) in [
            ("403 Forbidden", "", false),
            ("403 Forbidden", "x-ratelimit-remaining: 0\r\n", true),
        ] {
            let mut denied = discussion_page("/repos/owner/repo/issues/42/comments", 1, &[]);
            denied.status = status.to_owned();
            denied.extra_headers = extra_headers.to_owned();
            let (url, handle) = serve_many(vec![
                json_reply(pull_route(&key), pr_body(HEAD, BASE)),
                denied,
            ]);
            let error = GithubClient::for_api(url, Some("test-token".to_owned()))
                .observe_pull(&key)
                .err()
                .unwrap();
            handle.join().unwrap();
            assert_eq!(
                matches!(error, GithubError::RateLimited),
                expected_rate_limit
            );
            if !expected_rate_limit {
                assert!(matches!(error, GithubError::PermissionDenied { .. }));
            }
        }

        let mut broken = discussion_page("/repos/owner/repo/issues/42/comments", 1, &[]);
        broken.body = "[".to_owned();
        let (url, handle) = serve_many(vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            broken,
        ]);
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(matches!(error, GithubError::DecodeResponse { .. }));
    }

    #[test]
    fn observation_rejects_collections_beyond_item_and_byte_limits() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let many = (1..=1001)
            .map(|id| author_comment(id, "x"))
            .collect::<Vec<_>>();
        let mut replies = vec![json_reply(pull_route(&key), pr_body(HEAD, BASE))];
        replies.extend(discussion_collection_replies(
            "/repos/owner/repo/issues/42/comments",
            &many,
        ));
        let (url, handle) = serve_many(replies);
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(matches!(
            error,
            GithubError::TooManyResults {
                kind: "issue comments",
                limit: 1000
            }
        ));

        let oversized = author_comment(1, &"x".repeat(8 * 1024 * 1024));
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            discussion_page("/repos/owner/repo/issues/42/comments", 1, &[oversized]),
        ];
        let (url, handle) = serve_many(replies);
        let error = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .err()
            .unwrap();
        handle.join().unwrap();
        assert!(matches!(
            error,
            GithubError::ResponseTooLarge {
                kind: "issue comments",
                ..
            }
        ));
        assert!(!error.to_string().contains("xxxx"));
    }

    #[test]
    fn changed_other_user_comments_do_not_invalidate_an_observation() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let mut replies = vec![json_reply(pull_route(&key), pr_body(HEAD, BASE))];
        replies.extend(discussion_pass_replies(
            &[serde_json::json!({"id": 1, "user": {"id": 8}, "body": "before"})],
            &[],
            &[],
        ));
        replies.extend(discussion_pass_replies(
            &[serde_json::json!({"id": 1, "user": {"id": 8}, "body": "after"})],
            &[],
            &[],
        ));
        replies.push(json_reply(pull_route(&key), pr_body(HEAD, BASE)));
        let (url, handle) = serve_many(replies);
        let observation = GithubClient::for_api(url, Some("test-token".to_owned()))
            .observe_pull(&key)
            .unwrap();
        handle.join().unwrap();
        assert!(observation.discussion().comments().is_empty());
    }

    #[test]
    fn coauthor_hints_are_bound_to_the_pr_head_and_rechecked() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let commits_route = "/repos/owner/repo/pulls/42/commits?per_page=100&page=1";
        let commit = serde_json::json!({
            "sha": HEAD,
            "commit": {"message": "Change\n\nCo-authored-by: Claude <Claude@Example.com>"}
        });
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            json_reply(
                commits_route.to_owned(),
                serde_json::json!([commit]).to_string(),
            ),
            json_reply(
                commits_route.to_owned(),
                serde_json::json!([commit]).to_string(),
            ),
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let evidence = client.coauthors_for_pull(&key, &pull).unwrap();
        handle.join().unwrap();
        assert_eq!(evidence.head_sha(), pull.head_sha());
        assert_eq!(evidence.commit_count(), 1);
        assert!(evidence.identities().contains(
            &crate::config::CoauthorIdentity::new("Claude", "claude@example.com").unwrap()
        ));
    }

    #[test]
    fn changed_commit_list_cannot_propose_a_model() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let commits_route = "/repos/owner/repo/pulls/42/commits?per_page=100&page=1";
        let first = serde_json::json!([{
            "sha": HEAD,
            "commit": {"message": "Change\n\nCo-authored-by: Claude <claude@example.com>"}
        }]);
        let second = serde_json::json!([{
            "sha": HEAD,
            "commit": {"message": "Different message"}
        }]);
        let replies = vec![
            json_reply(pull_route(&key), pr_body(HEAD, BASE)),
            json_reply(commits_route.to_owned(), first.to_string()),
            json_reply(commits_route.to_owned(), second.to_string()),
        ];
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let pull = client.pull_request(&key).unwrap();
        let error = client.coauthors_for_pull(&key, &pull).err().unwrap();
        handle.join().unwrap();
        assert!(matches!(
            error,
            GithubError::PaginationChanged {
                kind: "PR commits",
                ..
            }
        ));
    }

    #[test]
    fn commit_hints_report_the_api_cap_only_when_the_head_is_missing() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let truncated = (1..=250)
            .map(|number| {
                serde_json::json!({
                    "sha": format!("{number:040x}"),
                    "commit": {"message": "Change"}
                })
            })
            .collect::<Vec<_>>();
        let mut complete = truncated.clone();
        complete[249]["sha"] = HEAD.into();
        complete[249]["commit"]["message"] =
            "Change\n\nCo-authored-by: Claude <claude@example.com>".into();

        let read_hints = |commits: &[serde_json::Value], final_head: &str| {
            let mut replies = vec![json_reply(pull_route(&key), pr_body(HEAD, BASE))];
            replies.extend(commit_page_replies(commits));
            replies.extend(commit_page_replies(commits));
            replies.push(json_reply(pull_route(&key), pr_body(final_head, BASE)));
            let (url, handle) = serve_many(replies);
            let client = GithubClient::for_api(url, Some("test-token".to_owned()));
            let pull = client.pull_request(&key).unwrap();
            let result = client.coauthors_for_pull(&key, &pull);
            handle.join().unwrap();
            result
        };

        let hints = read_hints(&complete, HEAD).unwrap();
        assert_eq!(hints.commit_count(), 250);
        assert!(hints.identities().contains(
            &crate::config::CoauthorIdentity::new("Claude", "claude@example.com").unwrap()
        ));

        let capped = read_hints(&truncated, HEAD).err().unwrap();
        assert!(matches!(
            &capped,
            GithubError::PrCommitsCapped { key: failed, limit: 250 } if failed == &key
        ));
        assert!(capped.to_string().contains("250-commit limit"));

        let changed = read_hints(&truncated, PREVIOUS_HEAD).err().unwrap();
        assert!(matches!(changed, GithubError::HeadChanged { .. }));

        let incomplete = read_hints(&truncated[..1], HEAD).err().unwrap();
        assert!(matches!(incomplete, GithubError::InvalidResponse { .. }));
    }

    #[test]
    fn preparation_inputs_bind_checks_and_coauthors_to_stable_observation() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let mut replies = observation_replies(&[], &[], &[]);
        replies.extend(preparation_replies(&[]));
        replies.push(json_reply(pull_route(&key), pr_body(HEAD, BASE)));
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let observed = client.observe_pull(&key).unwrap();
        let inputs = client.preparation_inputs(observed).unwrap();
        handle.join().unwrap();
        assert_eq!(inputs.head_sha().as_str(), HEAD);
        assert_eq!(inputs.checks_summary(), CheckSummary::Green);
        assert!(inputs.coauthors().contains(
            &crate::config::CoauthorIdentity::new("Claude", "claude@example.com").unwrap()
        ));
    }

    #[test]
    fn preparation_inputs_reject_head_and_author_changes_after_refresh() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let mut changed_author: serde_json::Value =
            serde_json::from_str(&pr_body(HEAD, BASE)).unwrap();
        changed_author["user"]["id"] = 8.into();
        for (changed_pull, expected_head_change) in [
            (pr_body(PREVIOUS_HEAD, BASE), true),
            (changed_author.to_string(), false),
        ] {
            let mut replies = observation_replies(&[], &[], &[]);
            replies.push(json_reply(pull_route(&key), changed_pull));
            let (url, handle) = serve_many(replies);
            let client = GithubClient::for_api(url, Some("test-token".to_owned()));
            let observed = client.observe_pull(&key).unwrap();
            let error = client.preparation_inputs(observed).err().unwrap();
            handle.join().unwrap();
            assert_eq!(
                matches!(&error, GithubError::HeadChanged { .. }),
                expected_head_change
            );
            assert_eq!(
                matches!(&error, GithubError::AuthorChanged { .. }),
                !expected_head_change
            );
        }
    }

    #[test]
    fn preparation_inputs_reject_author_discussion_change_on_final_pass() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let mut replies = observation_replies(&[], &[], &[]);
        replies.extend(preparation_replies(&[author_comment(9, "new reply")]));
        let (url, handle) = serve_many(replies);
        let client = GithubClient::for_api(url, Some("test-token".to_owned()));
        let observed = client.observe_pull(&key).unwrap();
        let error = client.preparation_inputs(observed).err().unwrap();
        handle.join().unwrap();
        assert!(matches!(error, GithubError::ObservationChanged));
    }

    #[test]
    fn preparation_inputs_recheck_the_final_pull_head_and_author() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let mut changed_author: serde_json::Value =
            serde_json::from_str(&pr_body(HEAD, BASE)).unwrap();
        changed_author["user"]["id"] = 8.into();
        for (changed_pull, expected_head_change) in [
            (pr_body(PREVIOUS_HEAD, BASE), true),
            (changed_author.to_string(), false),
        ] {
            let mut replies = observation_replies(&[], &[], &[]);
            replies.extend(preparation_replies(&[]));
            replies.push(json_reply(pull_route(&key), changed_pull));
            let (url, handle) = serve_many(replies);
            let client = GithubClient::for_api(url, Some("test-token".to_owned()));
            let observed = client.observe_pull(&key).unwrap();
            let error = client.preparation_inputs(observed).err().unwrap();
            handle.join().unwrap();
            assert_eq!(
                matches!(&error, GithubError::HeadChanged { .. }),
                expected_head_change
            );
            assert_eq!(
                matches!(&error, GithubError::AuthorChanged { .. }),
                !expected_head_change
            );
        }
    }

    #[test]
    fn round_preparation_verifies_archives_and_rejects_blocked_selection() {
        use crate::archive::ArchiveError;
        use crate::config::ReviewConfig;
        use crate::preparation::Readiness;
        use crate::round::{self, ModelSelection, RoundError};
        use crate::{
            AuthorizationScope, InputRevision, ModelValidationError, ObservedDiscussion, State,
        };
        use sha2::{Digest, Sha256};
        use std::num::NonZeroU64;

        #[derive(Clone, Copy)]
        enum Case {
            SelectedValid,
            SelectedInvalidArchive,
            ProposedValid,
            ProposedInvalidArchive,
            SelectedBlocked,
        }

        let directory = tempfile::tempdir().unwrap();
        let archive = directory.path().join("reviewer.tar");
        std::fs::write(&archive, b"archive bytes").unwrap();
        let checksum = format!("{:x}", Sha256::digest(b"archive bytes"));
        let key = PrKey::new("owner/repo", 42).unwrap();
        let head = HeadSha::new(HEAD).unwrap();
        let revision = InputRevision::new(2).unwrap();

        for case in [
            Case::SelectedValid,
            Case::SelectedInvalidArchive,
            Case::ProposedValid,
            Case::ProposedInvalidArchive,
            Case::SelectedBlocked,
        ] {
            let valid_checksum = matches!(case, Case::SelectedValid | Case::ProposedValid);
            let select_model = matches!(
                case,
                Case::SelectedValid | Case::SelectedInvalidArchive | Case::SelectedBlocked
            );
            let configured_checksum = if valid_checksum {
                checksum.as_str()
            } else {
                "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"
            };
            let config = ReviewConfig::from_json(
                &serde_json::to_vec(&serde_json::json!({
                    "schema_version": 2,
                    "models": [{"id": "model-alpha", "coauthors": []}],
                    "agents": [{"id": "claude", "models": ["model-alpha"], "prompt": "Review"}],
                    "repositories": [{
                        "name": "owner/repo",
                        "review_models": ["model-alpha"],
                        "runners": [{
                            "agent": "claude",
                            "vm_archive": {"path": archive, "sha256": configured_checksum}
                        }]
                    }]
                }))
                .unwrap(),
            )
            .unwrap();
            let mut state = State::default();
            state.record_request(key.clone(), head.clone()).unwrap();
            state
                .refresh(
                    &key,
                    &head,
                    &ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap(),
                )
                .unwrap();
            if !matches!(case, Case::SelectedBlocked) {
                state
                    .authorize(&key, &head, revision, AuthorizationScope::OneRound)
                    .unwrap();
            }

            let mut replies = observation_replies(&[], &[], &[]);
            replies.extend(preparation_replies(&[]));
            replies.push(json_reply(pull_route(&key), pr_body(HEAD, BASE)));
            let (url, handle) = serve_many(replies);
            let client = GithubClient::for_api(url, Some("test-token".to_owned()));
            let refreshed = round::refresh(&client, &mut state, &config, &key).unwrap();
            assert_eq!(refreshed.outcome(), crate::RefreshOutcome::Unchanged);
            let choice = ModelSelection::new(
                crate::config::ModelId::new("model-alpha").unwrap(),
                head.clone(),
                revision,
            );
            let result = round::prepare(
                &client,
                &mut state,
                refreshed,
                select_model.then_some(&choice),
            );
            handle.join().unwrap();
            match case {
                Case::SelectedValid | Case::ProposedValid => {
                    let prepared = result.unwrap();
                    assert_eq!(prepared.validation_changed(), select_model);
                    assert_eq!(
                        prepared.preparation().readiness(),
                        if select_model {
                            Readiness::Ready
                        } else {
                            Readiness::ModelValidationRequired
                        }
                    );
                    assert_eq!(prepared.resolved().agent().id().as_str(), "claude");
                    assert_eq!(
                        state.get(&key).unwrap().model_validation().is_some(),
                        select_model
                    );
                }
                Case::SelectedInvalidArchive | Case::ProposedInvalidArchive => {
                    assert!(matches!(
                        result.err().unwrap(),
                        RoundError::Archive(ArchiveError::ChecksumMismatch { .. })
                    ));
                    assert!(state.get(&key).unwrap().model_validation().is_none());
                }
                Case::SelectedBlocked => {
                    assert!(matches!(
                        result.err().unwrap(),
                        RoundError::Validation(ModelValidationError::Blocked(
                            Readiness::AwaitingAuthorization
                        ))
                    ));
                    assert!(state.get(&key).unwrap().model_validation().is_none());
                }
            }
        }
    }
}
