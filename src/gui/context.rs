//! Display-only GitHub observations scoped to the GUI launch and recorded PR inputs.

use super::RequestList;
use crate::storage;
use anyhow::Result;
use review_platform::{
    HeadSha, InputRevision, PrKey, RefreshOutcome,
    github::{PullRequest, PullRequestState, checks::CheckSummary},
};
use serde::Serialize;
use std::{
    collections::HashMap,
    path::Path,
    sync::Mutex,
    time::{SystemTime, UNIX_EPOCH},
};

/// Display-only GitHub context, bound to the inputs observed in this server launch.
#[derive(Clone, Serialize)]
pub(super) struct RequestContext {
    title: String,
    author: String,
    state: PullRequestState,
    draft: bool,
    head_sha: HeadSha,
    revision: InputRevision,
    checked_at: Option<u64>,
    checks: Option<CheckSummary>,
    head_changed: bool,
    discussion_changed: bool,
}

impl RequestContext {
    pub(super) fn new(
        pull: &PullRequest,
        revision: InputRevision,
        checked_at: Option<u64>,
        checks: Option<CheckSummary>,
        changes: RefreshOutcome,
    ) -> Self {
        let (head_changed, discussion_changed) = match changes {
            RefreshOutcome::Unchanged => (false, false),
            RefreshOutcome::Updated {
                head_changed,
                discussion_changed,
                ..
            } => (head_changed, discussion_changed),
        };
        Self {
            title: pull.title().to_owned(),
            author: pull.author().to_owned(),
            state: pull.state(),
            draft: pull.draft(),
            head_sha: pull.head_sha().clone(),
            revision,
            checked_at,
            checks,
            head_changed,
            discussion_changed,
        }
    }
}

#[derive(Default)]
pub(super) struct Contexts(Mutex<HashMap<PrKey, RequestContext>>);

impl Contexts {
    pub(super) fn record(
        &self,
        path: &Path,
        key: &PrKey,
        context: RequestContext,
    ) -> Option<RequestContext> {
        match self.try_record(path, key, &context) {
            Ok(true) => Some(context),
            Ok(false) => None,
            Err(error) => {
                crate::gui_diagnostics::report("display context", key, None, error.as_ref());
                let _ = self.forget(key);
                None
            }
        }
    }

    fn try_record(&self, path: &Path, key: &PrKey, context: &RequestContext) -> Result<bool> {
        // Coordinate with removal and re-addition before retaining any PR identity.
        let _lock = storage::lock(path)?;
        let state = storage::load(path)?;
        if state.get(key).is_some_and(|request| {
            request.head_sha() == &context.head_sha && request.revision() == context.revision
        }) {
            self.0
                .lock()
                .map_err(|_| anyhow::anyhow!("could not save GUI display context"))?
                .insert(key.clone(), context.clone());
            return Ok(true);
        }
        Ok(false)
    }

    pub(super) fn forget(&self, key: &PrKey) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| anyhow::anyhow!("could not clear GUI display context"))?
            .remove(key);
        Ok(())
    }

    pub(super) fn apply(&self, list: &mut RequestList) -> Result<()> {
        let mut contexts = self
            .0
            .lock()
            .map_err(|_| anyhow::anyhow!("could not read GUI display context"))?;
        contexts.retain(|key, context| {
            list.requests.iter().any(|request| {
                request.repository == key.repository()
                    && request.number == key.number()
                    && request.head_sha == context.head_sha.as_str()
                    && request.revision == context.revision.get()
            })
        });
        for request in &mut list.requests {
            request.context = contexts
                .get(&PrKey::new(&request.repository, request.number)?)
                .cloned();
        }
        Ok(())
    }
}

pub(super) fn observation_time() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|time| time.as_secs())
}

#[cfg(test)]
mod tests {
    use super::super::{
        PreparationResponse, RefreshResponse, Session, request_list, router_with_preparation,
    };
    use super::*;
    use crate::{preparation_commands::InspectedRules, request_commands};
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use review_platform::{
        ObservedDiscussion, State,
        config::{ModelId, PromptHash},
        preparation::Readiness,
        round::ModelSelection,
    };
    use tower::ServiceExt;

    fn display_context(head_sha: HeadSha, revision: InputRevision) -> RequestContext {
        RequestContext {
            title: "Fix login redirect".into(),
            author: "alice".into(),
            state: PullRequestState::Open,
            draft: false,
            head_sha,
            revision,
            checked_at: Some(123),
            checks: Some(CheckSummary::Green),
            head_changed: false,
            discussion_changed: true,
        }
    }

    fn refresh_before_cache_failure(
        path: &Path,
        key: &PrKey,
    ) -> Result<RefreshResponse, request_commands::RefreshError> {
        let mut state = storage::load(path).unwrap();
        let head = HeadSha::new("b".repeat(40)).unwrap();
        let discussion =
            ObservedDiscussion::new(std::num::NonZeroU64::new(7).unwrap(), vec![]).unwrap();
        let outcome = state.refresh(key, &head, &discussion)?;
        storage::save(path, &state).unwrap();
        let context = display_context(head, state.get(key).unwrap().revision());
        // Fail only the display cache's later lock acquisition, after inputs were saved.
        std::fs::create_dir(path.with_extension("json.lock")).unwrap();
        let mut response = RefreshResponse::from_outcome(outcome);
        response.context = Some(context);
        Ok(response)
    }

    fn prepare_before_cache_failure(
        path: &Path,
        _: &Path,
        key: &PrKey,
        _: Option<&ModelSelection>,
        _: Option<&InspectedRules>,
    ) -> Result<PreparationResponse, crate::preparation_commands::PrepareError> {
        let refreshed = refresh_before_cache_failure(path, key).unwrap();
        let context = refreshed.context.unwrap();
        Ok(PreparationResponse {
            head_sha: context.head_sha.clone(),
            revision: context.revision,
            authorization: review_platform::Authorization::AwaitingLead,
            configuration_sha256: PromptHash::new("a".repeat(64)).unwrap(),
            checks: CheckSummary::Green,
            readiness: Readiness::AwaitingAuthorization,
            readiness_message: Readiness::AwaitingAuthorization.to_string(),
            proposed_model: ModelId::new("model-alpha").unwrap(),
            proposal_matches_coauthor: false,
            validated_model: None,
            resolved_model: ModelId::new("model-alpha").unwrap(),
            models: vec![],
            context: Some(context),
        })
    }

    #[test]
    fn display_cache_failure_preserves_successful_refresh_and_preparation_responses() {
        for route in ["/api/requests/refresh", "/api/requests/prepare"] {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("state.json");
            let key = PrKey::new("owner/repo", 15).unwrap();
            let mut state = State::default();
            state
                .record_request(key.clone(), HeadSha::new("a".repeat(40)).unwrap())
                .unwrap();
            storage::save(&path, &state).unwrap();
            let app = router_with_preparation(
                path.clone(),
                directory.path().join("config.json"),
                Session {
                    bootstrap: None,
                    token: "test-token".into(),
                },
                refresh_before_cache_failure,
                prepare_before_cache_failure,
            );
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async {
                    let response = app
                        .oneshot(
                            Request::builder()
                                .method("POST")
                                .uri(route)
                                .header("host", "localhost:8080")
                                .header("origin", "http://localhost:8080")
                                .header("authorization", "Bearer test-token")
                                .header("content-type", "application/json")
                                .body(Body::from(r#"{"repository":"owner/repo","number":15}"#))
                                .unwrap(),
                        )
                        .await
                        .unwrap();
                    assert_eq!(response.status(), StatusCode::OK, "{route}");
                    let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
                    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                    assert!(body["context"].is_null());
                    assert!(body.get("error").is_none());
                    if route.ends_with("refresh") {
                        assert_eq!(body["changed"], true);
                    } else {
                        assert_eq!(body["head_sha"], "b".repeat(40));
                    }
                });
            let saved = storage::load(&path).unwrap();
            let saved_request = saved.get(&key).unwrap();
            assert_eq!(saved_request.head_sha().as_str(), "b".repeat(40));
            assert!(saved_request.discussion().is_some());
        }
    }

    #[test]
    fn github_display_context_is_bound_to_inputs_and_never_reused_after_removal() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let key = PrKey::new("owner/repo", 15).unwrap();
        let head = HeadSha::new("a".repeat(40)).unwrap();
        let mut state = State::default();
        state.record_request(key.clone(), head.clone()).unwrap();
        storage::save(&path, &state).unwrap();
        let revision = state.get(&key).unwrap().revision();
        let context = display_context(head.clone(), revision);
        let contexts = Contexts::default();
        let original = std::fs::read(&path).unwrap();
        contexts.record(&path, &key, context.clone()).unwrap();
        let mut list = request_list(&path).unwrap();
        contexts.apply(&mut list).unwrap();
        let displayed = list.requests[0].context.as_ref().unwrap();
        assert_eq!(displayed.title, "Fix login redirect");
        assert_eq!(displayed.checked_at, Some(123));
        assert_eq!(std::fs::read(&path).unwrap(), original);

        state
            .record_request(key.clone(), HeadSha::new("b".repeat(40)).unwrap())
            .unwrap();
        storage::save(&path, &state).unwrap();
        let mut list = request_list(&path).unwrap();
        contexts.apply(&mut list).unwrap();
        assert!(list.requests[0].context.is_none());
        assert!(contexts.record(&path, &key, context.clone()).is_none());
        assert!(contexts.0.lock().unwrap().is_empty());

        request_commands::unmanage(&path, &key).unwrap();
        assert!(contexts.record(&path, &key, context.clone()).is_none());
        assert!(contexts.0.lock().unwrap().is_empty());
        let mut state = storage::load(&path).unwrap();
        state.record_request(key.clone(), head).unwrap();
        storage::save(&path, &state).unwrap();
        assert!(state.get(&key).unwrap().revision().get() > revision.get());
        assert!(contexts.record(&path, &key, context).is_none());
        assert!(contexts.0.lock().unwrap().is_empty());
    }
}
