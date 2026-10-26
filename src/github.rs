//! Read-only GitHub REST API adapter for PR metadata, diffs, and checks.

mod checks;

use anyhow::{Context, Result, ensure};
use review_platform::{HeadSha, PrKey};
use serde::Deserialize;
use std::io::Read;
use std::time::Duration;

const API_ROOT: &str = "https://api.github.com";
const MAX_DIFF_BYTES: u64 = 8 * 1024 * 1024;

pub struct GithubClient {
    agent: ureq::Agent,
    api_root: String,
    token: Option<String>,
}

pub struct PullRequest {
    pub title: String,
    pub author: String,
    pub state: PullRequestState,
    pub draft: bool,
    pub head_sha: HeadSha,
    pub base_sha: HeadSha,
}

#[derive(Debug, Deserialize)]
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
    pub fn new() -> Result<Self> {
        let token = match std::env::var("REVIEW_PLATFORM_GITHUB_TOKEN_RO") {
            Ok(token) if !token.is_empty() => Some(token),
            Ok(_) | Err(std::env::VarError::NotPresent) => None,
            Err(std::env::VarError::NotUnicode(_)) => {
                anyhow::bail!("REVIEW_PLATFORM_GITHUB_TOKEN_RO must be UTF-8")
            }
        };
        Ok(Self::for_api(API_ROOT.to_owned(), token))
    }

    fn for_api(api_root: String, token: Option<String>) -> Self {
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(Duration::from_secs(5))
            .timeout_read(Duration::from_secs(15))
            .redirects(0)
            .build();
        Self {
            agent,
            api_root,
            token,
        }
    }

    pub fn pull_request(&self, key: &PrKey) -> Result<PullRequest> {
        let route = pull_route(key);
        let response = self.get(
            &route,
            "application/vnd.github+json",
            &format!("pull request {key}"),
        )?;
        let raw: ApiPullRequest = response
            .into_json()
            .with_context(|| format!("reading GitHub pull request {key}"))?;
        ensure!(
            raw.number == key.number(),
            "GitHub returned PR #{} while inspecting {key}",
            raw.number
        );
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
    ) -> Result<String> {
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
            let comparison: ApiComparison = response
                .into_json()
                .with_context(|| format!("reading GitHub comparison for {key}"))?;
            ensure!(
                &comparison.merge_base_commit.sha == previous_head,
                "{previous_head} is not an ancestor of {expected_head}; inspect the full PR diff after a force push"
            );
            route
        } else {
            pull_route(key)
        };

        let response = self.get(
            &route,
            "application/vnd.github.diff",
            &format!("diff for {key}"),
        )?;
        let content_type = response.header("Content-Type").unwrap_or_default();
        ensure!(
            content_type.starts_with("application/vnd.github.diff"),
            "GitHub returned {content_type:?} instead of a diff for {key}"
        );
        let mut bytes = Vec::new();
        response
            .into_reader()
            .take(MAX_DIFF_BYTES + 1)
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading GitHub diff for {key}"))?;
        ensure!(
            bytes.len() as u64 <= MAX_DIFF_BYTES,
            "GitHub diff for {key} exceeds 8 MiB; inspect it on GitHub"
        );
        let diff = String::from_utf8(bytes)
            .with_context(|| format!("GitHub diff for {key} is not UTF-8"))?;
        self.ensure_unchanged(key, pull, since.is_none())?;
        Ok(diff)
    }

    fn ensure_unchanged(&self, key: &PrKey, pull: &PullRequest, check_base: bool) -> Result<()> {
        let current = self.pull_request(key)?;
        ensure!(
            current.head_sha == pull.head_sha,
            "GitHub PR {key} head changed from {} to {} while fetching the diff; inspect it again",
            pull.head_sha,
            current.head_sha
        );
        if check_base {
            ensure!(
                current.base_sha == pull.base_sha,
                "GitHub PR {key} base changed from {} to {} while fetching the diff; inspect it again",
                pull.base_sha,
                current.base_sha
            );
        }
        Ok(())
    }

    fn get(&self, route: &str, accept: &str, description: &str) -> Result<ureq::Response> {
        let url = format!("{}{}", self.api_root, route);
        let mut request = self
            .agent
            .get(&url)
            .set("Accept", accept)
            .set("X-GitHub-Api-Version", "2022-11-28")
            .set(
                "User-Agent",
                concat!("review-platform/", env!("CARGO_PKG_VERSION")),
            );
        if let Some(token) = &self.token {
            ensure!(
                token.bytes().all(|byte| byte.is_ascii_graphic()),
                "REVIEW_PLATFORM_GITHUB_TOKEN_RO contains invalid header characters"
            );
            request = request.set("Authorization", &format!("Bearer {token}"));
        }

        let response = request
            .call()
            .map_err(|error| github_error(error, description))?;
        ensure!(
            response.status() == 200,
            "GitHub returned unexpected HTTP {} for {description}",
            response.status()
        );
        Ok(response)
    }
}

fn pull_route(key: &PrKey) -> String {
    format!("/repos/{}/pulls/{}", key.repository(), key.number())
}

fn github_error(error: ureq::Error, description: &str) -> anyhow::Error {
    match error {
        ureq::Error::Status(401, _) => {
            anyhow::anyhow!(
                "GitHub rejected REVIEW_PLATFORM_GITHUB_TOKEN_RO; check the token and try again"
            )
        }
        ureq::Error::Status(403 | 429, response)
            if response.header("x-ratelimit-remaining") == Some("0") =>
        {
            anyhow::anyhow!(
                "GitHub API rate limit reached; set REVIEW_PLATFORM_GITHUB_TOKEN_RO or retry later"
            )
        }
        ureq::Error::Status(404, _) => {
            anyhow::anyhow!("GitHub {description} was not found or is not accessible")
        }
        ureq::Error::Status(status, _) => {
            anyhow::anyhow!("GitHub returned HTTP {status} while inspecting {description}")
        }
        ureq::Error::Transport(error) => {
            anyhow::anyhow!(error).context(format!("connecting to GitHub for {description}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::checks::CheckSummary;
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;

    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const PREVIOUS_HEAD: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const BASE: &str = "cccccccccccccccccccccccccccccccccccccccc";
    const NEW_BASE: &str = "dddddddddddddddddddddddddddddddddddddddd";

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
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            for reply in replies {
                let (mut stream, _) = listener.accept().unwrap();
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
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: {}\r\nContent-Length: {}\r\n{}\r\n{}",
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
        assert_eq!(pull.head_sha.as_str(), HEAD);
        assert_eq!(pull.author, "author");
        assert!(matches!(pull.state, PullRequestState::Open));
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
        assert!(error.to_string().contains("not found"), "{error}");

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
        assert!(error.to_string().contains("reading GitHub"), "{error}");
    }

    #[test]
    fn rejects_invalid_token_without_exposing_it() {
        let secret = "test-secret\nmore-secret";
        let client = GithubClient::for_api(API_ROOT.to_owned(), Some(secret.to_owned()));
        let error = client
            .pull_request(&PrKey::new("owner/repo", 42).unwrap())
            .err()
            .unwrap();
        assert!(error.to_string().contains("invalid header characters"));
        assert!(!error.to_string().contains(secret));
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
        assert!(error.to_string().contains(HEAD), "{error}");
        assert!(error.to_string().contains(PREVIOUS_HEAD), "{error}");
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
        assert!(error.to_string().contains(BASE), "{error}");
        assert!(error.to_string().contains(NEW_BASE), "{error}");
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
        assert!(error.to_string().contains("not an ancestor"), "{error}");
    }

    #[test]
    fn checks_for_exact_head_ignore_an_empty_pending_legacy_rollup() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let check_runs = serde_json::json!({
            "total_count": 1,
            "check_runs": [{
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
        let first_page_runs = (0..100)
            .map(|number| {
                serde_json::json!({
                    "name": format!("job-{number}"),
                    "head_sha": HEAD,
                    "status": "completed",
                    "conclusion": "success"
                })
            })
            .collect::<Vec<_>>();
        let first_page =
            serde_json::json!({"total_count": 101, "check_runs": first_page_runs}).to_string();
        let second_page = serde_json::json!({
            "total_count": 101,
            "check_runs": [{
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
    fn checks_reject_a_run_for_a_different_head() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let check_runs = serde_json::json!({
            "total_count": 1,
            "check_runs": [{
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
        assert!(error.to_string().contains(HEAD), "{error}");
        assert!(error.to_string().contains(PREVIOUS_HEAD), "{error}");
    }
}
