//! PR identifiers, authorization states, and domain errors.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::num::NonZeroU64;
use thiserror::Error;

const MAX_OWNER_LEN: usize = 39;
const MAX_REPOSITORY_NAME_LEN: usize = 100;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StateError {
    #[error(
        "repository must be a valid GitHub owner/name (owner 1-39 characters, name 1-100 characters and not . or ..): {0}"
    )]
    InvalidRepository(String),
    #[error("PR number must be greater than zero")]
    InvalidPrNumber,
    #[error("head SHA must contain 40 or 64 hexadecimal characters")]
    InvalidHeadSha,
    #[error("input revision must be greater than zero")]
    InvalidRevision,
    #[error("input revision cannot advance beyond u64::MAX")]
    RevisionOverflow,
    #[error("comment SHA-256 must contain 64 hexadecimal characters")]
    InvalidCommentHash,
    #[error("duplicate {kind:?} comment ID {id} in stored discussion")]
    DuplicateComment { kind: CommentSource, id: u64 },
    #[error(
        "inspected head {inspected} is no longer the recorded head {recorded}; run status and review the current head before authorizing"
    )]
    HeadMismatch {
        inspected: HeadSha,
        recorded: HeadSha,
    },
    #[error(
        "inspected input revision {inspected} is no longer the recorded revision {recorded}; run status and review the current inputs before authorizing"
    )]
    RevisionMismatch {
        inspected: InputRevision,
        recorded: InputRevision,
    },
    #[error("review request not found: {0}")]
    RequestNotFound(PrKey),
    #[error("duplicate review request in state: {0}")]
    DuplicateRequest(PrKey),
    #[error("unsupported state schema version: {0}")]
    UnsupportedSchema(u32),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPrKey {
    repository: String,
    number: u64,
}

/// A repository and pull request number, parsed at construction and on load.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "RawPrKey")]
pub struct PrKey {
    repository: String,
    number: NonZeroU64,
}

impl PrKey {
    pub fn new(repository: impl Into<String>, number: u64) -> Result<Self, StateError> {
        let repository = repository.into().to_ascii_lowercase();
        validate_repository(&repository)?;
        let number = NonZeroU64::new(number).ok_or(StateError::InvalidPrNumber)?;
        Ok(Self { repository, number })
    }

    pub fn repository(&self) -> &str {
        &self.repository
    }

    pub fn number(&self) -> u64 {
        self.number.get()
    }
}

impl TryFrom<RawPrKey> for PrKey {
    type Error = StateError;

    fn try_from(value: RawPrKey) -> Result<Self, Self::Error> {
        Self::new(value.repository, value.number)
    }
}

impl fmt::Display for PrKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}#{}", self.repository, self.number)
    }
}

fn validate_repository(repository: &str) -> Result<(), StateError> {
    let Some((owner, name)) = repository.split_once('/') else {
        return Err(StateError::InvalidRepository(repository.to_owned()));
    };
    let valid_owner = !owner.is_empty()
        && owner.len() <= MAX_OWNER_LEN
        && !owner.starts_with('-')
        && !owner.ends_with('-')
        && owner
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'-');
    let valid_name = !name.is_empty()
        && name.len() <= MAX_REPOSITORY_NAME_LEN
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.'));
    if valid_owner && valid_name {
        Ok(())
    } else {
        Err(StateError::InvalidRepository(repository.to_owned()))
    }
}

/// A Git commit ID with 40 or 64 hexadecimal characters.
/// Accepts either case and stores lowercase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct HeadSha(String);

impl HeadSha {
    pub fn new(value: impl Into<String>) -> Result<Self, StateError> {
        let value = value.into();
        if !matches!(value.len(), 40 | 64) || !value.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(StateError::InvalidHeadSha);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for HeadSha {
    type Error = StateError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Display for HeadSha {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A positive revision of the recorded PR head and author discussion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct InputRevision(NonZeroU64);

impl InputRevision {
    pub fn new(value: u64) -> Result<Self, StateError> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or(StateError::InvalidRevision)
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }

    pub(crate) fn next(self) -> Result<Self, StateError> {
        self.get()
            .checked_add(1)
            .ok_or(StateError::RevisionOverflow)
            .and_then(Self::new)
    }
}

impl TryFrom<u64> for InputRevision {
    type Error = StateError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl From<InputRevision> for u64 {
    fn from(value: InputRevision) -> Self {
        value.get()
    }
}

impl fmt::Display for InputRevision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.get().fmt(f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommentSource {
    IssueComment,
    ReviewComment,
    Review,
}

/// SHA-256 of the exact text returned by GitHub, stored without the text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct CommentHash(String);

impl CommentHash {
    pub fn new(value: impl Into<String>) -> Result<Self, StateError> {
        let value = value.into();
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(StateError::InvalidCommentHash);
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    pub fn from_digest(digest: [u8; 32]) -> Self {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        let mut value = String::with_capacity(64);
        for byte in digest {
            value.push(char::from(HEX[(byte >> 4) as usize]));
            value.push(char::from(HEX[(byte & 0x0f) as usize]));
        }
        Self(value)
    }
}

impl TryFrom<String> for CommentHash {
    type Error = StateError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CommentFingerprint {
    source: CommentSource,
    id: NonZeroU64,
    sha256: CommentHash,
}

impl CommentFingerprint {
    pub fn new(source: CommentSource, id: NonZeroU64, sha256: CommentHash) -> Self {
        Self { source, id, sha256 }
    }

    pub fn source(&self) -> CommentSource {
        self.source
    }

    pub fn id(&self) -> u64 {
        self.id.get()
    }

    pub fn hash(&self) -> &CommentHash {
        &self.sha256
    }
}

/// The author's observed discussion, including identity for stable filtering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "RawObservedDiscussion")]
pub struct ObservedDiscussion {
    author_id: NonZeroU64,
    comments: Vec<CommentFingerprint>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawObservedDiscussion {
    author_id: NonZeroU64,
    comments: Vec<CommentFingerprint>,
}

impl ObservedDiscussion {
    pub fn new(
        author_id: NonZeroU64,
        mut comments: Vec<CommentFingerprint>,
    ) -> Result<Self, StateError> {
        comments.sort_by_key(|comment| (comment.source, comment.id));
        for pair in comments.windows(2) {
            if pair[0].source == pair[1].source && pair[0].id == pair[1].id {
                return Err(StateError::DuplicateComment {
                    kind: pair[0].source,
                    id: pair[0].id.get(),
                });
            }
        }
        Ok(Self {
            author_id,
            comments,
        })
    }

    pub fn author_id(&self) -> u64 {
        self.author_id.get()
    }

    pub fn comments(&self) -> &[CommentFingerprint] {
        &self.comments
    }

    pub fn count(&self, source: CommentSource) -> usize {
        self.comments
            .iter()
            .filter(|comment| comment.source == source)
            .count()
    }
}

impl TryFrom<RawObservedDiscussion> for ObservedDiscussion {
    type Error = StateError;

    fn try_from(value: RawObservedDiscussion) -> Result<Self, Self::Error> {
        Self::new(value.author_id, value.comments)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authorization {
    AwaitingLead,
    OneRound,
    PrCycle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthorizationScope {
    OneRound,
    PrCycle,
}

impl From<AuthorizationScope> for Authorization {
    fn from(scope: AuthorizationScope) -> Self {
        match scope {
            AuthorizationScope::OneRound => Self::OneRound,
            AuthorizationScope::PrCycle => Self::PrCycle,
        }
    }
}

impl fmt::Display for Authorization {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::AwaitingLead => "awaiting lead authorization",
            Self::OneRound => "authorized for one round",
            Self::PrCycle => "authorized for the PR cycle",
        };
        f.write_str(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repository_names_reject_reserved_segments_and_length_overflow() {
        for name in [".", ".."] {
            assert!(matches!(
                PrKey::new(format!("owner/{name}"), 1),
                Err(StateError::InvalidRepository(_))
            ));
        }
        let error = serde_json::from_value::<PrKey>(
            serde_json::json!({"repository": "owner/..", "number": 1}),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("repository must be a valid GitHub owner/name"),
            "{error}"
        );
        assert!(matches!(
            PrKey::new(format!("{}/repo", "o".repeat(40)), 1),
            Err(StateError::InvalidRepository(_))
        ));
        assert!(matches!(
            PrKey::new(format!("owner/{}", "r".repeat(101)), 1),
            Err(StateError::InvalidRepository(_))
        ));

        assert!(PrKey::new(format!("{}/{}", "o".repeat(39), "r".repeat(100)), 1).is_ok());
        assert!(PrKey::new("owner/.github", 1).is_ok());
    }
}
