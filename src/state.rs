//! Review requests and their authorization transitions.

use crate::model::{Authorization, PrKey, StateError};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const SCHEMA_VERSION: u32 = 1;

fn validate_head_sha(head_sha: &str) -> Result<(), StateError> {
    if matches!(head_sha.len(), 40 | 64) && head_sha.bytes().all(|c| c.is_ascii_hexdigit()) {
        Ok(())
    } else {
        Err(StateError::InvalidHeadSha)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecordOutcome {
    New,
    Unchanged,
    HeadChangedAuthorizationReset,
    HeadChangedAuthorizationPreserved,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewRequest {
    key: PrKey,
    head_sha: String,
    authorization: Authorization,
}

impl ReviewRequest {
    pub fn key(&self) -> &PrKey {
        &self.key
    }

    pub fn head_sha(&self) -> &str {
        &self.head_sha
    }

    pub fn authorization(&self) -> Authorization {
        self.authorization
    }

    fn validate(&self) -> Result<(), StateError> {
        self.key.validate()?;
        validate_head_sha(&self.head_sha)?;
        if self.head_sha != self.head_sha.to_ascii_lowercase() {
            return Err(StateError::InvalidHeadSha);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct State {
    schema_version: u32,
    requests: Vec<ReviewRequest>,
}

impl Default for State {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            requests: Vec::new(),
        }
    }
}

impl State {
    pub fn validate(&self) -> Result<(), StateError> {
        if self.schema_version != SCHEMA_VERSION {
            return Err(StateError::UnsupportedSchema(self.schema_version));
        }
        let mut seen = HashSet::new();
        for request in &self.requests {
            request.validate()?;
            if !seen.insert(&request.key) {
                return Err(StateError::DuplicateRequest(request.key.clone()));
            }
        }
        Ok(())
    }

    pub fn requests(&self) -> &[ReviewRequest] {
        &self.requests
    }

    pub fn get(&self, key: &PrKey) -> Option<&ReviewRequest> {
        self.requests.iter().find(|request| request.key == *key)
    }

    pub fn record_request(
        &mut self,
        key: PrKey,
        head_sha: impl Into<String>,
    ) -> Result<RecordOutcome, StateError> {
        key.validate()?;
        let head_sha = head_sha.into().to_ascii_lowercase();
        validate_head_sha(&head_sha)?;
        if let Some(request) = self.requests.iter_mut().find(|request| request.key == key) {
            if request.head_sha == head_sha {
                return Ok(RecordOutcome::Unchanged);
            }
            request.head_sha = head_sha;
            return match request.authorization {
                Authorization::OneRound => {
                    request.authorization = Authorization::AwaitingLead;
                    Ok(RecordOutcome::HeadChangedAuthorizationReset)
                }
                Authorization::AwaitingLead | Authorization::PrCycle => {
                    Ok(RecordOutcome::HeadChangedAuthorizationPreserved)
                }
            };
        }
        self.requests.push(ReviewRequest {
            key,
            head_sha,
            authorization: Authorization::AwaitingLead,
        });
        Ok(RecordOutcome::New)
    }

    pub fn set_authorization(
        &mut self,
        key: &PrKey,
        authorization: Authorization,
    ) -> Result<bool, StateError> {
        key.validate()?;
        let request = self.request_mut(key)?;
        let changed = request.authorization != authorization;
        request.authorization = authorization;
        Ok(changed)
    }

    fn request_mut(&mut self, key: &PrKey) -> Result<&mut ReviewRequest, StateError> {
        self.requests
            .iter_mut()
            .find(|request| request.key == *key)
            .ok_or_else(|| StateError::RequestNotFound(key.clone()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const HEAD_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn key() -> PrKey {
        PrKey::new("owner/repo", 42).unwrap()
    }

    #[test]
    fn one_round_authorization_expires_when_head_changes() {
        let mut state = State::default();
        state.record_request(key(), HEAD_A).unwrap();
        state
            .set_authorization(&key(), Authorization::OneRound)
            .unwrap();

        assert_eq!(
            state.record_request(key(), HEAD_B).unwrap(),
            RecordOutcome::HeadChangedAuthorizationReset
        );
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::AwaitingLead
        );
    }

    #[test]
    fn cycle_authorization_survives_head_change_and_can_be_revoked() {
        let mut state = State::default();
        state.record_request(key(), HEAD_A).unwrap();
        state
            .set_authorization(&key(), Authorization::PrCycle)
            .unwrap();

        assert_eq!(
            state.record_request(key(), HEAD_B).unwrap(),
            RecordOutcome::HeadChangedAuthorizationPreserved
        );
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::PrCycle
        );
        state
            .set_authorization(&key(), Authorization::AwaitingLead)
            .unwrap();
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::AwaitingLead
        );
    }

    #[test]
    fn invalid_input_does_not_change_state() {
        let mut state = State::default();
        state.record_request(key(), HEAD_A).unwrap();
        assert_eq!(
            state.record_request(key(), "not-a-sha"),
            Err(StateError::InvalidHeadSha)
        );
        assert_eq!(state.get(&key()).unwrap().head_sha(), HEAD_A);
        assert!(PrKey::new("owner/repo/extra", 42).is_err());
        assert_eq!(
            PrKey::new("owner/repo", 0),
            Err(StateError::InvalidPrNumber)
        );
    }

    #[test]
    fn repository_and_sha_case_cannot_create_a_second_request() {
        let mut state = State::default();
        let mixed_case_key = PrKey::new("Owner/Repo", 42).unwrap();
        state
            .record_request(mixed_case_key, HEAD_A.to_ascii_uppercase())
            .unwrap();
        state
            .set_authorization(&key(), Authorization::OneRound)
            .unwrap();

        assert_eq!(
            state.record_request(key(), HEAD_A).unwrap(),
            RecordOutcome::Unchanged
        );
        assert_eq!(state.requests().len(), 1);
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::OneRound
        );
    }

    #[test]
    fn corrupt_or_future_state_is_rejected() {
        let duplicate = serde_json::json!({
            "schema_version": 1,
            "requests": [
                {"key": {"repository": "owner/repo", "number": 42}, "head_sha": HEAD_A, "authorization": "one_round"},
                {"key": {"repository": "owner/repo", "number": 42}, "head_sha": HEAD_B, "authorization": "pr_cycle"}
            ]
        });
        let state: State = serde_json::from_value(duplicate).unwrap();
        assert_eq!(state.validate(), Err(StateError::DuplicateRequest(key())));

        let future: State = serde_json::from_str(r#"{"schema_version":2,"requests":[]}"#).unwrap();
        assert_eq!(future.validate(), Err(StateError::UnsupportedSchema(2)));
    }
}
