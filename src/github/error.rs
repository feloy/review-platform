//! Typed GitHub adapter failures for CLI reporting and future daemon decisions.

use crate::{HeadSha, PrKey};
use std::string::FromUtf8Error;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum GithubError {
    #[error("REVIEW_PLATFORM_GITHUB_TOKEN_RO must be UTF-8")]
    TokenNotUtf8,
    #[error("REVIEW_PLATFORM_GITHUB_TOKEN_RO contains invalid header characters")]
    InvalidTokenHeader,
    #[error(
        "GitHub requires authentication to read {resource}; set REVIEW_PLATFORM_GITHUB_TOKEN_RO"
    )]
    AuthenticationRequired { resource: String },
    #[error("GitHub rejected REVIEW_PLATFORM_GITHUB_TOKEN_RO; check the token and try again")]
    InvalidCredentials,
    #[error("GitHub API rate limit reached; set REVIEW_PLATFORM_GITHUB_TOKEN_RO or retry later")]
    RateLimited,
    #[error("GitHub {resource} was not found or is not accessible")]
    NotFound { resource: String },
    #[error(
        "GitHub denied access to {resource}; check read permissions on REVIEW_PLATFORM_GITHUB_TOKEN_RO"
    )]
    PermissionDenied { resource: String },
    #[error("GitHub returned HTTP {status} while inspecting {resource}")]
    HttpStatus { status: u16, resource: String },
    #[error("connecting to GitHub for {resource}: {source}")]
    Transport {
        resource: String,
        #[source]
        source: Box<dyn std::error::Error + Send + Sync>,
    },
    #[error("reading {resource}: {source}")]
    ReadResponse {
        resource: String,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid GitHub JSON for {resource}: {source}")]
    DecodeResponse {
        resource: String,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid GitHub response for {resource}: {detail}")]
    InvalidResponse { resource: String, detail: String },
    #[error("GitHub {kind} changed during pagination ({detail}); retry later")]
    PaginationChanged { kind: &'static str, detail: String },
    #[error("GitHub reports more than {limit} {kind}; inspect them on GitHub")]
    TooManyResults { kind: &'static str, limit: usize },
    #[error(
        "GitHub {kind} response exceeds {limit_mib} MiB per retrieval pass; inspect it on GitHub"
    )]
    ResponseTooLarge { kind: &'static str, limit_mib: u64 },
    #[error("GitHub author discussion changed during retrieval; retry later")]
    ObservationChanged,
    #[error("GitHub PR {key} author changed during retrieval; inspect it again")]
    AuthorChanged { key: PrKey },
    #[error(
        "GitHub PR {key} head changed from {expected} to {actual} while fetching PR data; inspect it again"
    )]
    HeadChanged {
        key: PrKey,
        expected: HeadSha,
        actual: HeadSha,
    },
    #[error(
        "GitHub PR {key} base changed from {expected} to {actual} while fetching PR data; inspect it again"
    )]
    BaseChanged {
        key: PrKey,
        expected: HeadSha,
        actual: HeadSha,
    },
    #[error(
        "{previous} is not an ancestor of {current}; inspect the full PR diff after a force push"
    )]
    DivergentHistory { previous: HeadSha, current: HeadSha },
    #[error("GitHub returned {actual:?} instead of a diff for {key}")]
    UnexpectedDiffMediaType { key: PrKey, actual: String },
    #[error(
        "GitHub refused the diff for {key} (HTTP 406, possibly due to file or line limits); inspect the PR on GitHub"
    )]
    DiffUnavailable { key: PrKey },
    #[error("GitHub diff for {key} exceeds {max_mib} MiB; inspect it on GitHub")]
    DiffTooLarge { key: PrKey, max_mib: u64 },
    #[error("GitHub diff for {key} is not UTF-8: {source}")]
    DiffNotUtf8 {
        key: PrKey,
        #[source]
        source: FromUtf8Error,
    },
}
