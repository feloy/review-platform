//! Sealed review bodies, publication attempts, and lead decision receipts.

use crate::{HeadSha, InputRevision, ObservedDiscussion, PrKey, model::valid_github_login};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::num::NonZeroU64;
use thiserror::Error;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum PublicationStateError {
    #[error("publication SHA-256 must contain 64 hexadecimal characters")]
    InvalidHash,
    #[error("the supplied body hash does not match the sealed publication body")]
    BodyHashMismatch,
    #[error("the saved draft does not match its sealed publication body")]
    DraftChanged,
    #[error("publication attempt {0} does not exist or is no longer current")]
    AttemptNotCurrent(u64),
    #[error("publication attempt ID overflow")]
    AttemptOverflow,
    #[error("the supplied review was present before this publication attempt")]
    PreexistingReview,
}

/// SHA-256 of exact UTF-8 bytes, parsed identically from CLI and persisted JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct PublicationHash(String);

impl PublicationHash {
    pub fn new(value: impl Into<String>) -> Result<Self, PublicationStateError> {
        let value = value.into();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(PublicationStateError::InvalidHash);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn of(bytes: &[u8]) -> Self {
        let digest: [u8; 32] = Sha256::digest(bytes).into();
        let mut hex = String::with_capacity(64);
        for byte in digest {
            use std::fmt::Write;
            write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail");
        }
        Self(hex)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PublicationHash {
    type Error = PublicationStateError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl std::fmt::Display for PublicationHash {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedBody {
    body: String,
    hash: PublicationHash,
    artifact_hash: PublicationHash,
}

impl SealedBody {
    pub fn new(comments: &str, round_id: &str, artifact_hash: PublicationHash) -> Self {
        let body = format!(
            "{}\n\n_Agent review via Review Platform (round {})._",
            comments, round_id
        );
        let hash = PublicationHash::of(body.as_bytes());
        Self {
            body,
            hash,
            artifact_hash,
        }
    }

    pub fn body(&self) -> &str {
        &self.body
    }

    pub fn hash(&self) -> &PublicationHash {
        &self.hash
    }

    pub fn artifact_hash(&self) -> &PublicationHash {
        &self.artifact_hash
    }

    pub fn validate(&self) -> bool {
        !self.body.is_empty() && self.hash == PublicationHash::of(self.body.as_bytes())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttemptOutcome {
    Reserved,
    Dispatched,
    DefinitiveFailure,
    Unknown,
    Published,
    ConfirmedNotPosted,
}

impl std::fmt::Display for AttemptOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Reserved => "reserved",
            Self::Dispatched => "dispatched",
            Self::DefinitiveFailure => "definitive failure",
            Self::Unknown => "outcome unknown",
            Self::Published => "published",
            Self::ConfirmedNotPosted => "confirmed not posted",
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationAttempt {
    id: NonZeroU64,
    artifact_hash: PublicationHash,
    body_hash: PublicationHash,
    head_sha: HeadSha,
    revision: InputRevision,
    discussion: ObservedDiscussion,
    operator: String,
    expected_publisher_login: String,
    publisher_id: NonZeroU64,
    reserved_at: u64,
    dispatched_at: Option<u64>,
    finished_at: Option<u64>,
    preexisting_review_ids: Vec<NonZeroU64>,
    outcome: AttemptOutcome,
    #[serde(default)]
    manual_confirmation: Option<OperatorAssertion>,
    #[serde(default)]
    recovery_confirmation: Option<OperatorAssertion>,
}

impl PublicationAttempt {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        id: NonZeroU64,
        seal: &SealedBody,
        head_sha: HeadSha,
        revision: InputRevision,
        discussion: ObservedDiscussion,
        operator: String,
        expected_publisher_login: String,
        publisher_id: NonZeroU64,
        reserved_at: u64,
        mut preexisting_review_ids: Vec<NonZeroU64>,
    ) -> Self {
        preexisting_review_ids.sort_unstable();
        preexisting_review_ids.dedup();
        Self {
            id,
            artifact_hash: seal.artifact_hash.clone(),
            body_hash: seal.hash.clone(),
            head_sha,
            revision,
            discussion,
            operator,
            expected_publisher_login,
            publisher_id,
            reserved_at,
            dispatched_at: None,
            finished_at: None,
            preexisting_review_ids,
            outcome: AttemptOutcome::Reserved,
            manual_confirmation: None,
            recovery_confirmation: None,
        }
    }

    pub fn id(&self) -> u64 {
        self.id.get()
    }
    pub fn outcome(&self) -> AttemptOutcome {
        self.outcome
    }
    pub fn publisher_id(&self) -> u64 {
        self.publisher_id.get()
    }
    pub fn expected_publisher_login(&self) -> &str {
        &self.expected_publisher_login
    }
    pub fn is_preexisting(&self, review_id: u64) -> bool {
        self.preexisting_review_ids
            .iter()
            .any(|id| id.get() == review_id)
    }
    pub(crate) fn dispatch(&mut self, at: u64) {
        self.dispatched_at = Some(at);
        self.outcome = AttemptOutcome::Dispatched;
    }
    pub(crate) fn finish(&mut self, outcome: AttemptOutcome, at: u64) {
        self.outcome = outcome;
        self.finished_at = Some(at);
    }
    pub(crate) fn confirm_not_posted(&mut self, operator: String, at: u64) {
        self.manual_confirmation = Some(OperatorAssertion { operator, at });
        self.finish(AttemptOutcome::ConfirmedNotPosted, at);
    }
    pub(crate) fn confirm_recovery(&mut self, operator: String, at: u64) {
        self.recovery_confirmation = Some(OperatorAssertion { operator, at });
    }
    pub(crate) fn validate(
        &self,
        seal: &SealedBody,
        head: &HeadSha,
        revision: InputRevision,
        discussion: &ObservedDiscussion,
    ) -> bool {
        self.reserved_at > 0
            && !self.operator.is_empty()
            && !self.operator.chars().any(char::is_control)
            && valid_github_login(&self.expected_publisher_login)
            && &self.head_sha == head
            && self.revision == revision
            && &self.discussion == discussion
            && self.artifact_hash == seal.artifact_hash
            && self.body_hash == seal.hash
            && (matches!(
                self.outcome,
                AttemptOutcome::Reserved | AttemptOutcome::DefinitiveFailure
            ) || self.dispatched_at.is_some())
            && (self.outcome != AttemptOutcome::Reserved || self.dispatched_at.is_none())
            && (matches!(
                self.outcome,
                AttemptOutcome::Reserved | AttemptOutcome::Dispatched
            )) == self.finished_at.is_none()
            && self
                .preexisting_review_ids
                .windows(2)
                .all(|pair| pair[0] < pair[1])
            && (self.outcome == AttemptOutcome::ConfirmedNotPosted)
                == self.manual_confirmation.is_some()
            && self.manual_confirmation.as_ref().is_none_or(|assertion| {
                !assertion.operator.is_empty()
                    && !assertion.operator.chars().any(char::is_control)
                    && assertion.at > 0
            })
            && self.recovery_confirmation.as_ref().is_none_or(|assertion| {
                self.outcome == AttemptOutcome::Published
                    && !assertion.operator.is_empty()
                    && !assertion.operator.chars().any(char::is_control)
                    && assertion.at > 0
            })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorAssertion {
    operator: String,
    at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationReceipt {
    review_id: NonZeroU64,
    url: String,
    publisher_login: String,
    publisher_id: NonZeroU64,
    commit_id: HeadSha,
    published_at: String,
}

impl PublicationReceipt {
    pub fn new(
        review_id: NonZeroU64,
        url: String,
        publisher_login: String,
        publisher_id: NonZeroU64,
        commit_id: HeadSha,
        published_at: String,
    ) -> Self {
        Self {
            review_id,
            url,
            publisher_login,
            publisher_id,
            commit_id,
            published_at,
        }
    }
    pub fn review_id(&self) -> u64 {
        self.review_id.get()
    }
    pub fn url(&self) -> &str {
        &self.url
    }
    pub fn publisher_login(&self) -> &str {
        &self.publisher_login
    }
    pub fn publisher_id(&self) -> u64 {
        self.publisher_id.get()
    }
    pub fn commit_id(&self) -> &HeadSha {
        &self.commit_id
    }
    pub fn published_at(&self) -> &str {
        &self.published_at
    }
    pub(crate) fn validate(
        &self,
        key: &PrKey,
        head: &HeadSha,
        attempt: &PublicationAttempt,
    ) -> bool {
        let expected_url = format!(
            "https://github.com/{}/pull/{}#pullrequestreview-{}",
            key.repository(),
            key.number(),
            self.review_id
        );
        self.url.eq_ignore_ascii_case(&expected_url)
            && valid_github_login(&self.publisher_login)
            && self.publisher_id.get() == attempt.publisher_id()
            && &self.commit_id == head
            && !self.published_at.is_empty()
            && self.published_at.len() <= 64
            && self.published_at.is_ascii()
            && !self
                .published_at
                .bytes()
                .any(|byte| byte.is_ascii_control())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RejectionDecision {
    operator: String,
    at: u64,
}

impl RejectionDecision {
    pub(crate) fn new(operator: String, at: u64) -> Self {
        Self { operator, at }
    }
    pub fn operator(&self) -> &str {
        &self.operator
    }
    pub fn at(&self) -> u64 {
        self.at
    }
    pub(crate) fn validate(&self) -> bool {
        !self.operator.is_empty() && !self.operator.chars().any(char::is_control) && self.at > 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_preserves_agent_comment_bytes_and_hashes_the_complete_body() {
        let comments = "Finding with a trailing newline\n";
        let seal = SealedBody::new(comments, "r-test", PublicationHash::of(b"artifact"));
        assert_eq!(
            seal.body(),
            "Finding with a trailing newline\n\n\n_Agent review via Review Platform (round r-test)._"
        );
        assert_eq!(seal.hash(), &PublicationHash::of(seal.body().as_bytes()));
        assert!(seal.validate());
    }
}
