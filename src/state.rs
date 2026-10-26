//! Review requests and their authorization transitions.

use crate::model::{Authorization, AuthorizationScope, HeadSha, PrKey, StateError};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

const SCHEMA_VERSION: u32 = 1;

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
    head_sha: HeadSha,
    authorization: Authorization,
}

impl ReviewRequest {
    pub fn key(&self) -> &PrKey {
        &self.key
    }

    pub fn head_sha(&self) -> &HeadSha {
        &self.head_sha
    }

    pub fn authorization(&self) -> Authorization {
        self.authorization
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

    pub fn record_request(&mut self, key: PrKey, head_sha: HeadSha) -> RecordOutcome {
        if let Some(request) = self.requests.iter_mut().find(|request| request.key == key) {
            if request.head_sha == head_sha {
                return RecordOutcome::Unchanged;
            }
            request.head_sha = head_sha;
            return match request.authorization {
                Authorization::OneRound => {
                    request.authorization = Authorization::AwaitingLead;
                    RecordOutcome::HeadChangedAuthorizationReset
                }
                Authorization::AwaitingLead | Authorization::PrCycle => {
                    RecordOutcome::HeadChangedAuthorizationPreserved
                }
            };
        }
        self.requests.push(ReviewRequest {
            key,
            head_sha,
            authorization: Authorization::AwaitingLead,
        });
        RecordOutcome::New
    }

    pub fn authorize(
        &mut self,
        key: &PrKey,
        inspected_head: &HeadSha,
        scope: AuthorizationScope,
    ) -> Result<bool, StateError> {
        let request = self.request_mut(key)?;
        if &request.head_sha != inspected_head {
            return Err(StateError::HeadMismatch {
                inspected: inspected_head.clone(),
                recorded: request.head_sha.clone(),
            });
        }
        let authorization = Authorization::from(scope);
        let changed = request.authorization != authorization;
        request.authorization = authorization;
        Ok(changed)
    }

    pub fn revoke(&mut self, key: &PrKey) -> Result<bool, StateError> {
        let request = self.request_mut(key)?;
        let changed = request.authorization != Authorization::AwaitingLead;
        request.authorization = Authorization::AwaitingLead;
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

    fn head(value: impl Into<String>) -> HeadSha {
        HeadSha::new(value).unwrap()
    }

    #[test]
    fn one_round_authorization_expires_when_head_changes() {
        let mut state = State::default();
        state.record_request(key(), head(HEAD_A));
        state
            .authorize(&key(), &head(HEAD_A), AuthorizationScope::OneRound)
            .unwrap();

        assert_eq!(
            state.record_request(key(), head(HEAD_B)),
            RecordOutcome::HeadChangedAuthorizationReset
        );
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::AwaitingLead
        );
    }

    #[test]
    fn authorization_rejects_a_head_changed_since_the_lead_checked_status() {
        let mut state = State::default();
        state.record_request(key(), head(HEAD_A));
        let seen_head = state.get(&key()).unwrap().head_sha().clone();
        state.record_request(key(), head(HEAD_B));

        assert_eq!(
            state.authorize(&key(), &seen_head, AuthorizationScope::OneRound),
            Err(StateError::HeadMismatch {
                inspected: head(HEAD_A),
                recorded: head(HEAD_B),
            })
        );
        let request = state.get(&key()).unwrap();
        assert_eq!(request.head_sha(), &head(HEAD_B));
        assert_eq!(request.authorization(), Authorization::AwaitingLead);

        assert!(
            state
                .authorize(&key(), &head(HEAD_B), AuthorizationScope::OneRound)
                .unwrap()
        );
    }

    #[test]
    fn cycle_authorization_survives_head_change_and_can_be_revoked() {
        let mut state = State::default();
        state.record_request(key(), head(HEAD_A));
        state
            .authorize(&key(), &head(HEAD_A), AuthorizationScope::PrCycle)
            .unwrap();

        assert_eq!(
            state.record_request(key(), head(HEAD_B)),
            RecordOutcome::HeadChangedAuthorizationPreserved
        );
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::PrCycle
        );
        state.revoke(&key()).unwrap();
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::AwaitingLead
        );
    }

    #[test]
    fn invalid_identifiers_fail_at_construction_and_deserialization() {
        assert_eq!(HeadSha::new("not-a-sha"), Err(StateError::InvalidHeadSha));
        assert_eq!(
            HeadSha::new("g".repeat(40)),
            Err(StateError::InvalidHeadSha)
        );
        assert_eq!(
            HeadSha::new("A".repeat(64)).unwrap().as_str(),
            "a".repeat(64)
        );
        assert!(PrKey::new("owner/repo/extra", 42).is_err());
        assert_eq!(
            PrKey::new("owner/repo", 0),
            Err(StateError::InvalidPrNumber)
        );

        for raw_key in [
            serde_json::json!({"repository": "owner/repo", "number": 0}),
            serde_json::json!({"repository": "owner/repo/extra", "number": 42}),
        ] {
            let invalid_state = serde_json::json!({
                "schema_version": 1,
                "requests": [{"key": raw_key, "head_sha": HEAD_A, "authorization": "awaiting_lead"}]
            });
            assert!(serde_json::from_value::<State>(invalid_state).is_err());
        }
        let invalid_head = serde_json::json!({
            "schema_version": 1,
            "requests": [
                {"key": {"repository": "owner/repo", "number": 42}, "head_sha": "not-a-sha", "authorization": "awaiting_lead"}
            ]
        });
        assert!(serde_json::from_value::<State>(invalid_head).is_err());
    }

    #[test]
    fn repository_and_sha_case_cannot_create_a_second_request() {
        let mut state = State::default();
        let mixed_case_key = PrKey::new("Owner/Repo", 42).unwrap();
        let uppercase_head = head(HEAD_A.to_ascii_uppercase());
        state.record_request(mixed_case_key, uppercase_head.clone());
        state
            .authorize(&key(), &uppercase_head, AuthorizationScope::OneRound)
            .unwrap();

        assert_eq!(
            state.record_request(key(), head(HEAD_A)),
            RecordOutcome::Unchanged
        );
        assert_eq!(state.requests().len(), 1);
        assert_eq!(
            state.get(&key()).unwrap().authorization(),
            Authorization::OneRound
        );
    }

    #[test]
    fn state_json_keeps_its_shape_and_normalizes_identifiers() {
        let raw = serde_json::json!({
            "schema_version": 1,
            "requests": [
                {
                    "key": {"repository": "Owner/Repo", "number": 42},
                    "head_sha": HEAD_A.to_ascii_uppercase(),
                    "authorization": "awaiting_lead"
                }
            ]
        });
        let state: State = serde_json::from_value(raw).unwrap();
        state.validate().unwrap();
        assert_eq!(state.get(&key()).unwrap().head_sha(), &head(HEAD_A));

        let serialized = serde_json::to_value(&state).unwrap();
        assert_eq!(serialized["requests"][0]["key"]["repository"], "owner/repo");
        assert_eq!(serialized["requests"][0]["key"]["number"], 42);
        assert_eq!(serialized["requests"][0]["head_sha"], HEAD_A);
        assert_eq!(serialized["requests"][0]["authorization"], "awaiting_lead");
    }

    #[test]
    fn corrupt_or_future_state_is_rejected() {
        let duplicate = serde_json::json!({
            "schema_version": 1,
            "requests": [
                {"key": {"repository": "owner/repo", "number": 42}, "head_sha": HEAD_A, "authorization": "one_round"},
                {"key": {"repository": "Owner/Repo", "number": 42}, "head_sha": HEAD_B, "authorization": "pr_cycle"}
            ]
        });
        let state: State = serde_json::from_value(duplicate).unwrap();
        assert_eq!(state.validate(), Err(StateError::DuplicateRequest(key())));

        let future: State = serde_json::from_str(r#"{"schema_version":2,"requests":[]}"#).unwrap();
        assert_eq!(future.validate(), Err(StateError::UnsupportedSchema(2)));
    }
}
