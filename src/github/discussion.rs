//! Observe author discussion and fetch bounded PR conversation for review input.

use super::{GithubClient, GithubError, GithubResult, HttpResponse};
use crate::{CommentFingerprint, CommentHash, CommentSource, HeadSha, ObservedDiscussion, PrKey};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Deserializer, Serialize};
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

/// Complete, stable conversation data for a specific inspected PR head.
pub struct DiscussionContext {
    key: PrKey,
    head_sha: HeadSha,
    author_id: NonZeroU64,
    collections: DiscussionCollections,
}

impl DiscussionContext {
    pub fn head_sha(&self) -> &HeadSha {
        &self.head_sha
    }

    pub fn observed_discussion(&self) -> GithubResult<ObservedDiscussion> {
        self.collections.observed(&self.key, self.author_id)
    }

    /// Serialize submitted reviews and comments as untrusted agent input.
    pub fn context_json(&self) -> GithubResult<Vec<u8>> {
        #[derive(Serialize)]
        struct Context<'a> {
            key: &'a PrKey,
            head_sha: &'a HeadSha,
            author_id: NonZeroU64,
            issue_comments: &'a [ApiComment],
            review_comments: &'a [ApiComment],
            reviews: Vec<&'a ApiReview>,
        }
        let context = Context {
            key: &self.key,
            head_sha: &self.head_sha,
            author_id: self.author_id,
            issue_comments: &self.collections.issue_comments,
            review_comments: &self.collections.review_comments,
            reviews: self
                .collections
                .reviews
                .iter()
                .filter(|review| review.state != ReviewState::Pending)
                .collect(),
        };
        let bytes =
            serde_json::to_vec_pretty(&context).map_err(|source| GithubError::InvalidResponse {
                resource: format!("discussion for {}", self.key),
                detail: source.to_string(),
            })?;
        if bytes.len() > 32 * 1024 * 1024 {
            return Err(GithubError::ResponseTooLarge {
                kind: "PR discussion context",
                limit_mib: 32,
            });
        }
        Ok(bytes)
    }
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

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct ApiDiscussionUser {
    id: NonZeroU64,
    #[serde(default)]
    login: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct ApiComment {
    id: NonZeroU64,
    #[serde(deserialize_with = "nullable")]
    user: Option<ApiDiscussionUser>,
    body: String,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    line: Option<u64>,
    #[serde(default)]
    original_line: Option<u64>,
    #[serde(default)]
    commit_id: Option<HeadSha>,
    #[serde(default)]
    in_reply_to_id: Option<NonZeroU64>,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
struct ApiReview {
    id: NonZeroU64,
    #[serde(deserialize_with = "nullable")]
    user: Option<ApiDiscussionUser>,
    #[serde(deserialize_with = "nullable")]
    body: Option<String>,
    state: ReviewState,
    #[serde(default)]
    commit_id: Option<HeadSha>,
    #[serde(default)]
    submitted_at: Option<String>,
}

#[derive(PartialEq, Eq)]
struct DiscussionCollections {
    issue_comments: Vec<ApiComment>,
    review_comments: Vec<ApiComment>,
    reviews: Vec<ApiReview>,
}

impl DiscussionCollections {
    fn observed(&self, key: &PrKey, author_id: NonZeroU64) -> GithubResult<ObservedDiscussion> {
        let mut fingerprints = Vec::new();
        for (items, source) in [
            (&self.issue_comments, CommentSource::IssueComment),
            (&self.review_comments, CommentSource::ReviewComment),
        ] {
            fingerprints.extend(
                items
                    .iter()
                    .filter_map(|item| item.fingerprint(source, author_id)),
            );
        }
        fingerprints.extend(
            self.reviews
                .iter()
                .filter_map(|item| item.fingerprint(CommentSource::Review, author_id)),
        );
        ObservedDiscussion::new(author_id, fingerprints).map_err(|error| {
            GithubError::InvalidResponse {
                resource: format!("author discussion for {key}"),
                detail: error.to_string(),
            }
        })
    }
}

// Require the API field while accepting null values GitHub may return.
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    deserializer: D,
) -> Result<Option<T>, D::Error> {
    Option::<T>::deserialize(deserializer)
}

#[derive(Debug, Deserialize, Serialize, PartialEq, Eq)]
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
        let first = self.discussion_pass(key)?;
        let second = self.discussion_pass(key)?;
        let first_observed = first.observed(key, pull.author_id)?;
        let second_observed = second.observed(key, pull.author_id)?;
        if first_observed != second_observed {
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
            discussion: second_observed,
        })
    }

    /// Fetch every conversation collection twice and bind it to the inspected PR.
    pub fn context_for_pull(
        &self,
        key: &PrKey,
        pull: &super::PullRequest,
    ) -> GithubResult<DiscussionContext> {
        let first = self.discussion_pass(key)?;
        let second = self.discussion_pass(key)?;
        if first != second {
            return Err(GithubError::ObservationChanged);
        }
        let current = self.pull_request(key)?;
        if current.head_sha != pull.head_sha {
            return Err(GithubError::HeadChanged {
                key: key.clone(),
                expected: pull.head_sha.clone(),
                actual: current.head_sha,
            });
        }
        if current.base_sha != pull.base_sha {
            return Err(GithubError::BaseChanged {
                key: key.clone(),
                expected: pull.base_sha.clone(),
                actual: current.base_sha,
            });
        }
        if current.author_id != pull.author_id {
            return Err(GithubError::AuthorChanged { key: key.clone() });
        }
        Ok(DiscussionContext {
            key: key.clone(),
            head_sha: pull.head_sha.clone(),
            author_id: pull.author_id,
            collections: second,
        })
    }

    fn discussion_pass(&self, key: &PrKey) -> GithubResult<DiscussionCollections> {
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
        let issue_comments = self.collect::<ApiComment>(key, &issue_route, "issue comments")?;
        let review_comments =
            self.collect::<ApiComment>(key, &review_comment_route, "review comments")?;
        let reviews = self.collect::<ApiReview>(key, &review_route, "reviews")?;
        Ok(DiscussionCollections {
            issue_comments,
            review_comments,
            reviews,
        })
    }

    fn collect<T: DiscussionItem>(
        &self,
        key: &PrKey,
        route: &str,
        kind: &'static str,
    ) -> GithubResult<Vec<T>> {
        let mut seen = HashSet::new();
        let mut all_items = Vec::new();
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
            let items_len = items.len();
            if items_len > PAGE_SIZE {
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
            }
            all_items.extend(items);
            if seen.len() > MAX_ITEMS {
                return Err(GithubError::TooManyResults {
                    kind,
                    limit: MAX_ITEMS,
                });
            }
            if items_len == 0 && has_next {
                return Err(GithubError::PaginationChanged {
                    kind,
                    detail: "empty page has a next link".to_owned(),
                });
            }
            if items_len < PAGE_SIZE && !has_next {
                return Ok(all_items);
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
