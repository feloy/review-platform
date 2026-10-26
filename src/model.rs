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
    #[error(
        "inspected head {inspected} is no longer the recorded head {recorded}; run status and review the current head before authorizing"
    )]
    HeadMismatch {
        inspected: HeadSha,
        recorded: HeadSha,
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
            assert!(PrKey::new(format!("owner/{name}"), 1).is_err());
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
        assert!(PrKey::new(format!("{}/repo", "o".repeat(40)), 1).is_err());
        assert!(PrKey::new(format!("owner/{}", "r".repeat(101)), 1).is_err());

        assert!(PrKey::new(format!("{}/{}", "o".repeat(39), "r".repeat(100)), 1).is_ok());
        assert!(PrKey::new("owner/.github", 1).is_ok());
    }
}
