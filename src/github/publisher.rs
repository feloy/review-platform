//! Write credential boundary for exact GitHub review publication and recovery.

use crate::publication::{PublicationAttempt, PublicationReceipt};
use crate::{HeadSha, PrKey};
use serde::Deserialize;
use std::collections::HashSet;
use std::io::Read;
use std::num::NonZeroU64;
use std::time::Duration;
use thiserror::Error;
use ureq::tls::{RootCerts, TlsConfig};

const API_ROOT: &str = "https://api.github.com";
const PAGE_SIZE: usize = 100;
const MAX_REVIEWS: usize = 1000;
const MAX_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum PublisherError {
    #[error("set REVIEW_PLATFORM_GITHUB_TOKEN_RW to a token with pull-request write access")]
    MissingToken,
    #[error("REVIEW_PLATFORM_GITHUB_TOKEN_RW must be UTF-8 without header control characters")]
    InvalidToken,
    #[error("publishing requires a configured publisher_login")]
    MissingExpectedLogin,
    #[error("configured publisher_login is not a valid GitHub login")]
    InvalidExpectedLogin,
    #[error("write token belongs to {actual}, expected {expected}")]
    WrongPublisher { expected: String, actual: String },
    #[error("write token belongs to GitHub account ID {actual}, expected ID {expected}")]
    WrongPublisherId { expected: u64, actual: u64 },
    #[error("GitHub returned HTTP {status} for {operation}")]
    HttpStatus {
        operation: &'static str,
        status: u16,
    },
    #[error("GitHub connection failed during {operation}: {source}")]
    Transport {
        operation: &'static str,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("reading GitHub {operation} response: {source}")]
    Read {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("GitHub {operation} response exceeds 8 MiB")]
    TooLarge { operation: &'static str },
    #[error("invalid GitHub {operation} JSON: {source}")]
    Json {
        operation: &'static str,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid GitHub {operation} response: {detail}")]
    InvalidResponse {
        operation: &'static str,
        detail: String,
    },
    #[error("GitHub review enumeration is incomplete or unstable")]
    IncompleteReviews,
}

#[derive(Debug, Error)]
pub enum PostError {
    #[error("publication request was not dispatched: {0}")]
    BeforeDispatch(#[source] PublisherError),
    #[error("GitHub definitively rejected the review: {0}")]
    Definitive(#[source] PublisherError),
    #[error("publication outcome is unknown: {0}")]
    Unknown(#[source] PublisherError),
}

pub struct PublisherIdentity {
    login: String,
    id: NonZeroU64,
}

impl PublisherIdentity {
    pub fn login(&self) -> &str {
        &self.login
    }
    pub fn id(&self) -> NonZeroU64 {
        self.id
    }
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ApiUser {
    id: NonZeroU64,
    login: String,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
struct ApiReview {
    id: NonZeroU64,
    html_url: String,
    user: Option<ApiUser>,
    body: Option<String>,
    commit_id: Option<HeadSha>,
    state: ApiReviewState,
    submitted_at: Option<String>,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ApiReviewState {
    Pending,
    Commented,
    Approved,
    ChangesRequested,
    Dismissed,
}

/// An inspected GitHub review; only the adapter can construct it.
pub struct PublishedReview {
    raw: ApiReview,
}

impl PublishedReview {
    pub fn id(&self) -> u64 {
        self.raw.id.get()
    }

    pub fn nonzero_id(&self) -> NonZeroU64 {
        self.raw.id
    }

    pub fn matches(&self, publisher_id: u64, head: &HeadSha, body: &str) -> bool {
        self.raw
            .user
            .as_ref()
            .is_some_and(|user| user.id.get() == publisher_id)
            && self.raw.commit_id.as_ref() == Some(head)
            && self.raw.body.as_deref() == Some(body)
            && self.raw.state == ApiReviewState::Commented
            && self.raw.submitted_at.is_some()
    }

    pub fn into_receipt(self) -> Result<PublicationReceipt, PublisherError> {
        let user = self
            .raw
            .user
            .ok_or_else(|| PublisherError::InvalidResponse {
                operation: "review",
                detail: "review publisher is missing".to_owned(),
            })?;
        let commit_id = self
            .raw
            .commit_id
            .ok_or_else(|| PublisherError::InvalidResponse {
                operation: "review",
                detail: "review commit is missing".to_owned(),
            })?;
        let published_at =
            self.raw
                .submitted_at
                .ok_or_else(|| PublisherError::InvalidResponse {
                    operation: "review",
                    detail: "review publication time is missing".to_owned(),
                })?;
        Ok(PublicationReceipt::new(
            self.raw.id,
            self.raw.html_url,
            user.login,
            user.id,
            commit_id,
            published_at,
        ))
    }
}

/// Find only new submitted reviews matching an attempt's publisher and sealed inputs.
pub fn recovery_matches<'a>(
    reviews: &'a [PublishedReview],
    attempt: &PublicationAttempt,
    head: &HeadSha,
    body: &str,
) -> Vec<&'a PublishedReview> {
    reviews
        .iter()
        .filter(|review| {
            review.matches(attempt.publisher_id(), head, body)
                && !attempt.is_preexisting(review.id())
        })
        .collect()
}

pub struct GithubPublisher {
    agent: ureq::Agent,
    api_root: String,
    token: String,
    expected_login: String,
}

impl GithubPublisher {
    pub fn new(expected_login: Option<&str>) -> Result<Self, PublisherError> {
        let expected_login = expected_login.ok_or(PublisherError::MissingExpectedLogin)?;
        let token = match std::env::var("REVIEW_PLATFORM_GITHUB_TOKEN_RW") {
            Ok(token) if !token.is_empty() => token,
            Ok(_) | Err(std::env::VarError::NotPresent) => {
                return Err(PublisherError::MissingToken);
            }
            Err(std::env::VarError::NotUnicode(_)) => return Err(PublisherError::InvalidToken),
        };
        Self::for_api(API_ROOT.to_owned(), token, expected_login.to_owned())
    }

    fn for_api(
        api_root: String,
        token: String,
        expected_login: String,
    ) -> Result<Self, PublisherError> {
        if !token.bytes().all(|byte| byte.is_ascii_graphic()) {
            return Err(PublisherError::InvalidToken);
        }
        if expected_login.is_empty()
            || expected_login.len() > 39
            || expected_login.starts_with('-')
            || expected_login.ends_with('-')
            || !expected_login
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err(PublisherError::InvalidExpectedLogin);
        }
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
        Ok(Self {
            agent: ureq::Agent::new_with_config(config),
            api_root,
            token,
            expected_login,
        })
    }

    pub fn verify_identity(&self) -> Result<PublisherIdentity, PublisherError> {
        let identity = self.read_identity()?;
        if !identity.login.eq_ignore_ascii_case(&self.expected_login) {
            return Err(PublisherError::WrongPublisher {
                expected: self.expected_login.clone(),
                actual: identity.login,
            });
        }
        Ok(identity)
    }

    pub fn verify_identity_id(
        &self,
        expected_id: u64,
    ) -> Result<PublisherIdentity, PublisherError> {
        let identity = self.read_identity()?;
        if identity.id.get() != expected_id {
            return Err(PublisherError::WrongPublisherId {
                expected: expected_id,
                actual: identity.id.get(),
            });
        }
        Ok(identity)
    }

    fn read_identity(&self) -> Result<PublisherIdentity, PublisherError> {
        let response = self.get("/user", "publisher identity")?;
        let user: ApiUser = decode(response, "publisher identity")?;
        Ok(PublisherIdentity {
            login: user.login,
            id: user.id,
        })
    }

    pub fn reviews(&self, key: &PrKey) -> Result<Vec<PublishedReview>, PublisherError> {
        let first = self.collect_reviews(key)?;
        let second = self.collect_reviews(key)?;
        if first != second {
            return Err(PublisherError::IncompleteReviews);
        }
        Ok(second
            .into_iter()
            .map(|raw| PublishedReview { raw })
            .collect())
    }

    fn collect_reviews(&self, key: &PrKey) -> Result<Vec<ApiReview>, PublisherError> {
        let mut result = Vec::new();
        let mut seen = HashSet::new();
        for page in 1..=(MAX_REVIEWS / PAGE_SIZE + 1) {
            let route = format!(
                "/repos/{}/pulls/{}/reviews?per_page={PAGE_SIZE}&page={page}",
                key.repository(),
                key.number()
            );
            let response = self.get(&route, "review enumeration")?;
            let has_next = match response.headers().get("Link") {
                None => false,
                Some(value) => value
                    .to_str()
                    .map_err(|_| PublisherError::IncompleteReviews)?
                    .contains("rel=\"next\""),
            };
            let reviews: Vec<ApiReview> = decode(response, "review enumeration")?;
            if reviews.len() > PAGE_SIZE || (reviews.len() < PAGE_SIZE && has_next) {
                return Err(PublisherError::IncompleteReviews);
            }
            for review in reviews {
                validate_review(&review, key)?;
                if !seen.insert(review.id) {
                    return Err(PublisherError::IncompleteReviews);
                }
                result.push(review);
            }
            if result.len() > MAX_REVIEWS {
                return Err(PublisherError::IncompleteReviews);
            }
            if !has_next {
                return Ok(result);
            }
            if has_next && result.len() >= MAX_REVIEWS {
                return Err(PublisherError::IncompleteReviews);
            }
        }
        Err(PublisherError::IncompleteReviews)
    }

    /// The caller must persist a dispatched attempt before invoking this once.
    pub fn create_review(
        &self,
        key: &PrKey,
        head: &HeadSha,
        body: &str,
        identity: &PublisherIdentity,
    ) -> Result<PublicationReceipt, PostError> {
        let payload = serde_json::to_vec(&serde_json::json!({
            "body": body,
            "commit_id": head,
            "event": "COMMENT",
        }))
        .map_err(|source| {
            PostError::Definitive(PublisherError::Json {
                operation: "review request",
                source,
            })
        })?;
        let url = format!(
            "{}/repos/{}/pulls/{}/reviews",
            self.api_root,
            key.repository(),
            key.number()
        );
        let request = self
            .agent
            .post(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Authorization", format!("Bearer {}", self.token))
            .header("Content-Type", "application/json");
        let response = request.send(payload.as_slice()).map_err(|source| {
            let before_dispatch = matches!(
                source,
                ureq::Error::HostNotFound
                    | ureq::Error::BadUri(_)
                    | ureq::Error::InvalidProxyUrl
                    | ureq::Error::ConnectionFailed
            );
            let error = PublisherError::Transport {
                operation: "review publication",
                source: Box::new(source),
            };
            if before_dispatch {
                PostError::BeforeDispatch(error)
            } else {
                PostError::Unknown(error)
            }
        })?;
        let status = response.status().as_u16();
        if status != 200 && status != 201 {
            let error = PublisherError::HttpStatus {
                operation: "review publication",
                status,
            };
            return if (400..500).contains(&status) && status != 408 && status != 429 {
                Err(PostError::Definitive(error))
            } else {
                Err(PostError::Unknown(error))
            };
        }
        let review: ApiReview =
            decode(response, "review publication").map_err(PostError::Unknown)?;
        validate_review(&review, key).map_err(PostError::Unknown)?;
        let posted = PublishedReview { raw: review };
        if !posted.matches(identity.id.get(), head, body)
            || !posted
                .raw
                .user
                .as_ref()
                .is_some_and(|user| user.login.eq_ignore_ascii_case(identity.login()))
        {
            return Err(PostError::Unknown(PublisherError::InvalidResponse {
                operation: "review publication",
                detail: "review identity, commit, state, or body differs from the request"
                    .to_owned(),
            }));
        }
        posted.into_receipt().map_err(PostError::Unknown)
    }

    fn get(
        &self,
        route: &str,
        operation: &'static str,
    ) -> Result<ureq::http::Response<ureq::Body>, PublisherError> {
        let url = format!("{}{}", self.api_root, route);
        let response = self
            .agent
            .get(&url)
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .header("Authorization", format!("Bearer {}", self.token))
            .call()
            .map_err(|source| PublisherError::Transport {
                operation,
                source: Box::new(source),
            })?;
        if response.status().as_u16() != 200 {
            return Err(PublisherError::HttpStatus {
                operation,
                status: response.status().as_u16(),
            });
        }
        Ok(response)
    }
}

fn decode<T: serde::de::DeserializeOwned>(
    response: ureq::http::Response<ureq::Body>,
    operation: &'static str,
) -> Result<T, PublisherError> {
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(MAX_RESPONSE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| PublisherError::Read { operation, source })?;
    if bytes.len() as u64 > MAX_RESPONSE_BYTES {
        return Err(PublisherError::TooLarge { operation });
    }
    serde_json::from_slice(&bytes).map_err(|source| PublisherError::Json { operation, source })
}

fn validate_review(review: &ApiReview, key: &PrKey) -> Result<(), PublisherError> {
    let prefix = format!(
        "https://github.com/{}/pull/{}#pullrequestreview-",
        key.repository(),
        key.number()
    );
    if !review
        .html_url
        .eq_ignore_ascii_case(&format!("{prefix}{}", review.id))
        || review
            .user
            .as_ref()
            .is_some_and(|user| user.login.is_empty())
        || review.submitted_at.as_ref().is_some_and(|time| {
            time.is_empty()
                || time.len() > 64
                || !time.is_ascii()
                || time.bytes().any(|byte| byte.is_ascii_control())
        })
    {
        return Err(PublisherError::InvalidResponse {
            operation: "review",
            detail: "missing or invalid review URL, user, or publication time".to_owned(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader, Write};
    use std::net::TcpListener;
    use std::thread;
    use std::time::Instant;

    const HEAD: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct Reply {
        method: &'static str,
        route: &'static str,
        status: &'static str,
        body: String,
        expected_request: Option<serde_json::Value>,
    }

    fn serve(replies: Vec<Reply>) -> (String, thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let handle = thread::spawn(move || {
            for reply in replies {
                let deadline = Instant::now() + Duration::from_secs(10);
                let mut stream = loop {
                    match listener.accept() {
                        Ok((stream, _)) => break stream,
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            assert!(Instant::now() < deadline, "mock request timed out");
                            thread::sleep(Duration::from_millis(5));
                        }
                        Err(error) => panic!("mock accept: {error}"),
                    }
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(10)))
                    .unwrap();
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                reader.read_line(&mut line).unwrap_or_else(|error| {
                    panic!(
                        "reading mock {} {} request: {error}",
                        reply.status, reply.route
                    )
                });
                assert_eq!(
                    line,
                    format!("{} {} HTTP/1.1\r\n", reply.method, reply.route)
                );
                let mut length = 0;
                let mut authorization = false;
                loop {
                    line.clear();
                    reader.read_line(&mut line).unwrap();
                    if line == "\r\n" {
                        break;
                    }
                    let lower = line.to_ascii_lowercase();
                    if let Some(value) = lower.strip_prefix("content-length: ") {
                        length = value.trim().parse::<usize>().unwrap();
                    }
                    authorization |= lower == "authorization: bearer write-token\r\n";
                }
                assert!(authorization);
                let mut request_body = vec![0; length];
                reader.read_exact(&mut request_body).unwrap();
                if let Some(expected) = reply.expected_request {
                    assert_eq!(
                        serde_json::from_slice::<serde_json::Value>(&request_body).unwrap(),
                        expected
                    );
                } else {
                    assert!(request_body.is_empty());
                }
                let response = format!(
                    "HTTP/1.1 {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    reply.status,
                    reply.body.len(),
                    reply.body
                );
                stream.write_all(response.as_bytes()).unwrap();
            }
        });
        (url, handle)
    }

    fn response_review(body: &str) -> String {
        serde_json::json!({
            "id": 71,
            "html_url": "https://github.com/owner/repo/pull/42#pullrequestreview-71",
            "user": {"id": 7, "login": "publisher"},
            "body": body,
            "commit_id": HEAD,
            "state": "COMMENTED",
            "submitted_at": "2026-10-30T10:00:00Z"
        })
        .to_string()
    }

    fn review_with_id(id: u64, body: &str) -> PublishedReview {
        let mut value: serde_json::Value = serde_json::from_str(&response_review(body)).unwrap();
        value["id"] = id.into();
        value["html_url"] =
            format!("https://github.com/owner/repo/pull/42#pullrequestreview-{id}").into();
        PublishedReview {
            raw: serde_json::from_value(value).unwrap(),
        }
    }

    #[test]
    fn recovery_candidates_cover_zero_one_and_multiple_new_matches() {
        use crate::publication::{PublicationHash, SealedBody};
        use crate::{InputRevision, ObservedDiscussion};

        let head = HeadSha::new(HEAD).unwrap();
        let body = "Finding\n\n_Agent review via Review Platform (round r-test)._";
        let seal = SealedBody::new("Finding", "r-test", PublicationHash::of(b"agent"));
        let attempt = PublicationAttempt::new(
            NonZeroU64::new(1).unwrap(),
            &seal,
            head.clone(),
            InputRevision::new(2).unwrap(),
            ObservedDiscussion::new(NonZeroU64::new(7).unwrap(), vec![]).unwrap(),
            "uid:1".into(),
            "publisher".into(),
            NonZeroU64::new(7).unwrap(),
            1,
            vec![NonZeroU64::new(71).unwrap()],
        );
        let existing = review_with_id(71, body);
        let copied = review_with_id(72, "different body");
        assert!(recovery_matches(&[existing, copied], &attempt, &head, body).is_empty());

        let reviews = [review_with_id(71, body), review_with_id(72, body)];
        let one = recovery_matches(&reviews, &attempt, &head, body);
        assert_eq!(
            one.iter().map(|review| review.id()).collect::<Vec<_>>(),
            vec![72]
        );

        let reviews = [
            review_with_id(71, body),
            review_with_id(72, body),
            review_with_id(73, body),
        ];
        let multiple = recovery_matches(&reviews, &attempt, &head, body);
        assert_eq!(
            multiple
                .iter()
                .map(|review| review.id())
                .collect::<Vec<_>>(),
            vec![72, 73]
        );
    }

    #[test]
    fn posts_the_exact_sealed_body_as_a_comment_on_the_reviewed_commit() {
        let body = "Finding\n\n_Agent review via Review Platform (round r-test)._";
        let route = "/repos/owner/repo/pulls/42/reviews?per_page=100&page=1";
        let (url, worker) = serve(vec![
            Reply {
                method: "GET",
                route: "/user",
                status: "200 OK",
                body: r#"{"id":7,"login":"publisher"}"#.into(),
                expected_request: None,
            },
            Reply {
                method: "GET",
                route,
                status: "200 OK",
                body: "[]".into(),
                expected_request: None,
            },
            Reply {
                method: "GET",
                route,
                status: "200 OK",
                body: "[]".into(),
                expected_request: None,
            },
            Reply {
                method: "POST",
                route: "/repos/owner/repo/pulls/42/reviews",
                status: "200 OK",
                body: response_review(body),
                expected_request: Some(
                    serde_json::json!({"body": body, "commit_id": HEAD, "event": "COMMENT"}),
                ),
            },
        ]);
        let publisher =
            GithubPublisher::for_api(url, "write-token".into(), "publisher".into()).unwrap();
        let identity = publisher.verify_identity().unwrap();
        let key = PrKey::new("owner/repo", 42).unwrap();
        assert!(publisher.reviews(&key).unwrap().is_empty());
        let receipt = publisher
            .create_review(&key, &HeadSha::new(HEAD).unwrap(), body, &identity)
            .unwrap();
        assert_eq!(receipt.review_id(), 71);
        assert_eq!(receipt.commit_id().as_str(), HEAD);
        worker.join().unwrap();
    }

    #[test]
    fn wrong_publisher_identity_stops_before_any_post() {
        let (url, worker) = serve(vec![Reply {
            method: "GET",
            route: "/user",
            status: "200 OK",
            body: r#"{"id":8,"login":"other"}"#.into(),
            expected_request: None,
        }]);
        let publisher =
            GithubPublisher::for_api(url, "write-token".into(), "publisher".into()).unwrap();
        assert!(matches!(
            publisher.verify_identity(),
            Err(PublisherError::WrongPublisher { .. })
        ));
        worker.join().unwrap();
    }

    #[test]
    fn recovery_uses_the_recorded_numeric_account_id_after_a_login_change() {
        let (url, worker) = serve(vec![Reply {
            method: "GET",
            route: "/user",
            status: "200 OK",
            body: r#"{"id":7,"login":"renamed-publisher"}"#.into(),
            expected_request: None,
        }]);
        let publisher =
            GithubPublisher::for_api(url, "write-token".into(), "publisher".into()).unwrap();
        assert_eq!(publisher.verify_identity_id(7).unwrap().id().get(), 7);
        worker.join().unwrap();
    }

    #[test]
    fn definitive_rejection_and_invalid_success_have_different_outcomes() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let identity = PublisherIdentity {
            login: "publisher".into(),
            id: NonZeroU64::new(7).unwrap(),
        };
        for (status, response, unknown) in [
            ("422 Unprocessable Entity", "{}".to_owned(), false),
            ("201 Created", response_review("different body"), true),
            ("500 Server Error", "{}".to_owned(), true),
        ] {
            let (url, worker) = serve(vec![Reply {
                method: "POST",
                route: "/repos/owner/repo/pulls/42/reviews",
                status,
                body: response,
                expected_request: Some(
                    serde_json::json!({"body":"exact body","commit_id":HEAD,"event":"COMMENT"}),
                ),
            }]);
            let publisher =
                GithubPublisher::for_api(url, "write-token".into(), "publisher".into()).unwrap();
            let result = publisher.create_review(
                &key,
                &HeadSha::new(HEAD).unwrap(),
                "exact body",
                &identity,
            );
            assert_eq!(matches!(result, Err(PostError::Unknown(_))), unknown);
            worker.join().unwrap();
        }
    }

    #[test]
    fn recovery_enumeration_rejects_a_changed_second_pass() {
        let route = "/repos/owner/repo/pulls/42/reviews?per_page=100&page=1";
        let (url, worker) = serve(vec![
            Reply {
                method: "GET",
                route,
                status: "200 OK",
                body: format!("[{}]", response_review("body")),
                expected_request: None,
            },
            Reply {
                method: "GET",
                route,
                status: "200 OK",
                body: "[]".into(),
                expected_request: None,
            },
        ]);
        let publisher =
            GithubPublisher::for_api(url, "write-token".into(), "publisher".into()).unwrap();
        assert!(matches!(
            publisher.reviews(&PrKey::new("owner/repo", 42).unwrap()),
            Err(PublisherError::IncompleteReviews)
        ));
        worker.join().unwrap();
    }
}
