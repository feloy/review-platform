//! Stable read-only GitHub observation of a PR head and its author's discussion.

use super::{GithubClient, GithubError, GithubResult, HttpResponse};
use crate::{CommentFingerprint, CommentHash, CommentSource, HeadSha, ObservedDiscussion, PrKey};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::io::Read;
use std::num::NonZeroU64;

const PAGE_SIZE: usize = 100;
const MAX_ITEMS: usize = 1000;
const MAX_COLLECTION_BYTES: u64 = 8 * 1024 * 1024;

/// A complete observation from GitHub; fields cannot be changed by callers.
pub struct PullObservation {
    key: PrKey,
    head_sha: HeadSha,
    discussion: ObservedDiscussion,
}

impl PullObservation {
    pub fn key(&self) -> &PrKey {
        &self.key
    }

    pub fn head_sha(&self) -> &HeadSha {
        &self.head_sha
    }

    pub fn author_id(&self) -> u64 {
        self.discussion.author_id()
    }

    pub fn discussion(&self) -> &ObservedDiscussion {
        &self.discussion
    }
}

#[derive(Deserialize)]
struct ApiDiscussionUser {
    id: NonZeroU64,
}

#[derive(Deserialize)]
struct ApiComment {
    id: NonZeroU64,
    #[serde(deserialize_with = "nullable")]
    user: Option<ApiDiscussionUser>,
    body: String,
}

#[derive(Deserialize)]
struct ApiReview {
    id: NonZeroU64,
    #[serde(deserialize_with = "nullable")]
    user: Option<ApiDiscussionUser>,
    #[serde(deserialize_with = "nullable")]
    body: Option<String>,
    state: ReviewState,
}

fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ReviewState {
    Pending,
    Commented,
    Approved,
    ChangesRequested,
    Dismissed,
}

trait DiscussionItem: DeserializeOwned {
    fn id(&self) -> NonZeroU64;
    fn fingerprint(
        &self,
        source: CommentSource,
        author_id: NonZeroU64,
    ) -> Option<CommentFingerprint>;
}

impl DiscussionItem for ApiComment {
    fn id(&self) -> NonZeroU64 {
        self.id
    }

    fn fingerprint(
        &self,
        source: CommentSource,
        author_id: NonZeroU64,
    ) -> Option<CommentFingerprint> {
        (self.user.as_ref().is_some_and(|user| user.id == author_id))
            .then(|| fingerprint(source, self.id, &self.body))
    }
}

impl DiscussionItem for ApiReview {
    fn id(&self) -> NonZeroU64 {
        self.id
    }

    fn fingerprint(
        &self,
        source: CommentSource,
        author_id: NonZeroU64,
    ) -> Option<CommentFingerprint> {
        if !self.user.as_ref().is_some_and(|user| user.id == author_id)
            || self.state == ReviewState::Pending
        {
            return None;
        }
        self.body
            .as_deref()
            .filter(|body| !body.is_empty())
            .map(|body| fingerprint(source, self.id, body))
    }
}

fn fingerprint(source: CommentSource, id: NonZeroU64, body: &str) -> CommentFingerprint {
    let digest: [u8; 32] = Sha256::digest(body.as_bytes()).into();
    CommentFingerprint::new(source, id, CommentHash::from_digest(digest))
}

impl GithubClient {
    /// Read every discussion collection twice, then recheck head and author identity.
    pub fn observe_pull(&self, key: &PrKey) -> GithubResult<PullObservation> {
        let pull = self.pull_request(key)?;
        let first = self.discussion_pass(key, pull.author_id)?;
        let second = self.discussion_pass(key, pull.author_id)?;
        if first != second {
            return Err(GithubError::ObservationChanged);
        }
        let current = self.pull_request(key)?;
        if current.head_sha != pull.head_sha {
            return Err(GithubError::HeadChanged {
                key: key.clone(),
                expected: pull.head_sha,
                actual: current.head_sha,
            });
        }
        if current.author_id != pull.author_id {
            return Err(GithubError::AuthorChanged { key: key.clone() });
        }
        Ok(PullObservation {
            key: key.clone(),
            head_sha: current.head_sha,
            discussion: second,
        })
    }

    fn discussion_pass(
        &self,
        key: &PrKey,
        author_id: NonZeroU64,
    ) -> GithubResult<ObservedDiscussion> {
        let issue_route = format!(
            "/repos/{}/issues/{}/comments",
            key.repository(),
            key.number()
        );
        let review_comment_route = format!(
            "/repos/{}/pulls/{}/comments",
            key.repository(),
            key.number()
        );
        let review_route = format!("/repos/{}/pulls/{}/reviews", key.repository(), key.number());
        let mut comments = self.collect::<ApiComment>(
            key,
            &issue_route,
            "issue comments",
            CommentSource::IssueComment,
            author_id,
        )?;
        comments.extend(self.collect::<ApiComment>(
            key,
            &review_comment_route,
            "review comments",
            CommentSource::ReviewComment,
            author_id,
        )?);
        comments.extend(self.collect::<ApiReview>(
            key,
            &review_route,
            "reviews",
            CommentSource::Review,
            author_id,
        )?);
        ObservedDiscussion::new(author_id, comments).map_err(|error| GithubError::InvalidResponse {
            resource: format!("author discussion for {key}"),
            detail: error.to_string(),
        })
    }

    fn collect<T: DiscussionItem>(
        &self,
        key: &PrKey,
        route: &str,
        kind: &'static str,
        source: CommentSource,
        author_id: NonZeroU64,
    ) -> GithubResult<Vec<CommentFingerprint>> {
        let mut seen = HashSet::new();
        let mut fingerprints = Vec::new();
        let mut bytes_read = 0;
        let resource = format!("{kind} for {key}");
        for page in 1..=(MAX_ITEMS / PAGE_SIZE + 1) {
            let response = self.get(
                &format!("{route}?per_page={PAGE_SIZE}&page={page}"),
                "application/vnd.github+json",
                &resource,
            )?;
            let has_next = match response.headers().get("Link") {
                None => false,
                Some(header) => header
                    .to_str()
                    .map_err(|_| GithubError::InvalidResponse {
                        resource: resource.clone(),
                        detail: "invalid Link header".to_owned(),
                    })?
                    .contains("rel=\"next\""),
            };
            let items: Vec<T> = read_collection_page(response, kind, &resource, &mut bytes_read)?;
            if items.len() > PAGE_SIZE {
                return Err(GithubError::InvalidResponse {
                    resource: resource.clone(),
                    detail: format!("page {page} contains more than {PAGE_SIZE} items"),
                });
            }
            for item in &items {
                if !seen.insert(item.id()) {
                    return Err(GithubError::PaginationChanged {
                        kind,
                        detail: format!("ID {} appeared twice", item.id()),
                    });
                }
                if let Some(fingerprint) = item.fingerprint(source, author_id) {
                    fingerprints.push(fingerprint);
                }
            }
            if seen.len() > MAX_ITEMS {
                return Err(GithubError::TooManyResults {
                    kind,
                    limit: MAX_ITEMS,
                });
            }
            if items.is_empty() && has_next {
                return Err(GithubError::PaginationChanged {
                    kind,
                    detail: "empty page has a next link".to_owned(),
                });
            }
            if items.len() < PAGE_SIZE && !has_next {
                return Ok(fingerprints);
            }
        }
        Err(GithubError::TooManyResults {
            kind,
            limit: MAX_ITEMS,
        })
    }
}

fn read_collection_page<T: DeserializeOwned>(
    response: HttpResponse,
    kind: &'static str,
    resource: &str,
    bytes_read: &mut u64,
) -> GithubResult<T> {
    let remaining = MAX_COLLECTION_BYTES - *bytes_read;
    let mut bytes = Vec::new();
    response
        .into_body()
        .into_reader()
        .take(remaining + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| GithubError::ReadResponse {
            resource: resource.to_owned(),
            source,
        })?;
    *bytes_read += bytes.len() as u64;
    if *bytes_read > MAX_COLLECTION_BYTES {
        return Err(GithubError::ResponseTooLarge {
            kind,
            limit_mib: MAX_COLLECTION_BYTES / (1024 * 1024),
        });
    }
    serde_json::from_slice(&bytes).map_err(|source| GithubError::DecodeResponse {
        resource: resource.to_owned(),
        source,
    })
}
