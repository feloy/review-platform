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
    pub(super) fn record(&self, path: &Path, key: &PrKey, context: &RequestContext) -> Result<()> {
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
        }
        Ok(())
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
    use super::super::request_list;
    use super::*;
    use crate::request_commands;
    use review_platform::State;

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
        let context = RequestContext {
            title: "Fix login redirect".into(),
            author: "alice".into(),
            state: PullRequestState::Open,
            draft: false,
            head_sha: head.clone(),
            revision,
            checked_at: Some(123),
            checks: Some(CheckSummary::Green),
            head_changed: false,
            discussion_changed: true,
        };
        let contexts = Contexts::default();
        let original = std::fs::read(&path).unwrap();
        contexts.record(&path, &key, &context).unwrap();
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
        contexts.record(&path, &key, &context).unwrap();
        assert!(contexts.0.lock().unwrap().is_empty());

        request_commands::unmanage(&path, &key).unwrap();
        contexts.record(&path, &key, &context).unwrap();
        assert!(contexts.0.lock().unwrap().is_empty());
        let mut state = storage::load(&path).unwrap();
        state.record_request(key.clone(), head).unwrap();
        storage::save(&path, &state).unwrap();
        assert!(state.get(&key).unwrap().revision().get() > revision.get());
        contexts.record(&path, &key, &context).unwrap();
        assert!(contexts.0.lock().unwrap().is_empty());
    }
}
