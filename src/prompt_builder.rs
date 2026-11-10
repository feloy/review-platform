//! Compose trusted review instructions and typed per-round tasks without external I/O.

use crate::config::{AgentKind, PromptHash};
use crate::publication::PublicationReceipt;
use crate::review::OUTPUT_SCHEMA;
use crate::{HeadSha, PrKey};
use serde::{Deserialize, Serialize};
use std::num::NonZeroU64;

const SYSTEM_PROMPT: &str = include_str!("prompts/system.md");
const FULL_TASK: &str = include_str!("prompts/full.md");
const INCREMENTAL_TASK: &str = include_str!("prompts/incremental.md");
const TASK_CONTEXT: &str = "Review pull request {key} at commit {head} against base commit {base}. Read the checkout at {checkout}.\n";
const PREVIOUS_REVIEW: &str = "The previous confirmed published review is GitHub review {review_id}, reviewing commit {since}.\n";

/// The lead's scope preference; automatic scope uses confirmed publication history.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewMode {
    #[default]
    Auto,
    Full,
}

/// Fixed review scope selected by the daemon, never inferred by the agent from PR text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum ReviewScope {
    Full,
    Incremental {
        since: HeadSha,
        review_id: NonZeroU64,
    },
}

impl ReviewScope {
    pub fn since(&self) -> Option<&HeadSha> {
        match self {
            Self::Full => None,
            Self::Incremental { since, .. } => Some(since),
        }
    }
}

/// Inspected base and scope retained with the round for reproducible task instructions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewTask {
    base_sha: HeadSha,
    scope: ReviewScope,
}

impl ReviewTask {
    pub fn new(base_sha: HeadSha, scope: ReviewScope) -> Self {
        Self { base_sha, scope }
    }

    pub fn base_sha(&self) -> &HeadSha {
        &self.base_sha
    }

    pub fn scope(&self) -> &ReviewScope {
        &self.scope
    }

    pub fn prompt(&self, key: &PrKey, head: &HeadSha) -> String {
        let checkout = crate::sandbox_layout::CHECKOUT_TARGET;
        // Interpolate only validated identifiers and fixed paths, never project or PR text.
        let context = TASK_CONTEXT
            .replace("{key}", &key.to_string())
            .replace("{head}", head.as_str())
            .replace("{base}", self.base_sha.as_str())
            .replace("{checkout}", checkout);
        match &self.scope {
            ReviewScope::Full => format!("{context}{FULL_TASK}"),
            ReviewScope::Incremental { since, review_id } => {
                let previous = PREVIOUS_REVIEW
                    .replace("{review_id}", &review_id.to_string())
                    .replace("{since}", since.as_str());
                format!("{context}{previous}{INCREMENTAL_TASK}")
            }
        }
    }
}

/// Choose a candidate scope before the GitHub adapter verifies commit ancestry.
pub fn scope(mode: ReviewMode, previous: Option<&PublicationReceipt>) -> ReviewScope {
    match (mode, previous) {
        (ReviewMode::Auto, Some(receipt)) => ReviewScope::Incremental {
            since: receipt.commit_id().clone(),
            review_id: receipt.review_id_nonzero(),
        },
        (ReviewMode::Auto, None) | (ReviewMode::Full, _) => ReviewScope::Full,
    }
}

/// Exact composed system instructions; contents stay out of Debug and public snapshots.
pub struct BuiltPrompt {
    text: String,
    hash: PromptHash,
}

impl BuiltPrompt {
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn hash(&self) -> &PromptHash {
        &self.hash
    }
}

pub fn build(kind: AgentKind, project: &str) -> BuiltPrompt {
    let text = compose(
        project,
        crate::agent::adapter(kind).tool_guidance(),
        SYSTEM_PROMPT,
    );
    let hash = PromptHash::of(&text);
    BuiltPrompt { text, hash }
}

fn compose(project: &str, agent: &str, system: &str) -> String {
    format!("Project guidance:\n{project}\n\nAgent guidance:\n{agent}\n\nSystem rules:\n{system}")
}

/// Version all compiled instructions, schemas, and rendering with model validation.
pub fn rules_hash(kind: AgentKind) -> PromptHash {
    let text = compose(
        "",
        crate::agent::adapter(kind).tool_guidance(),
        SYSTEM_PROMPT,
    );
    // Length-prefix each part so section boundaries cannot produce ambiguous hashes.
    let mut rules = String::new();
    for part in [
        text.as_str(),
        crate::sandbox_layout::CHECKOUT_TARGET,
        TASK_CONTEXT,
        PREVIOUS_REVIEW,
        FULL_TASK,
        INCREMENTAL_TASK,
        OUTPUT_SCHEMA,
    ] {
        use std::fmt::Write;
        // Writing to a String cannot fail.
        let _ = write!(rules, "{}:{part}", part.len());
    }
    PromptHash::of(&rules)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn composition_keeps_exact_project_text_and_selects_only_one_agent() {
        let project = "PRIVATE_PROJECT\r\n\nReview persistence.\n";
        let claude = build(AgentKind::Claude, project);
        assert!(
            claude
                .text()
                .starts_with(&format!("Project guidance:\n{project}\n\n"))
        );
        assert!(claude.text().ends_with(SYSTEM_PROMPT));
        assert!(claude.text().contains("dontAsk"));
        let codex = build(AgentKind::Codex, project);
        assert!(!codex.text().contains("dontAsk"));
        assert_ne!(claude.hash(), codex.hash());
        assert_ne!(claude.hash(), build(AgentKind::Claude, "Changed.").hash());
        assert_ne!(rules_hash(AgentKind::Claude), rules_hash(AgentKind::Codex));
        assert_eq!(claude.hash(), &PromptHash::of(claude.text()));
    }

    #[test]
    fn full_and_incremental_tasks_keep_scope_under_daemon_control() {
        let key = PrKey::new("owner/repo", 42).unwrap();
        let head = HeadSha::new("a".repeat(40)).unwrap();
        let base = HeadSha::new("b".repeat(40)).unwrap();
        let full = ReviewTask::new(base.clone(), ReviewScope::Full).prompt(&key, &head);
        assert!(full.contains("full review"));
        assert!(!full.contains("previous confirmed"));
        let task = ReviewTask::new(
            base,
            ReviewScope::Incremental {
                since: head.clone(),
                review_id: NonZeroU64::new(17).unwrap(),
            },
        );
        let incremental = task.prompt(&key, &head);
        assert!(incremental.contains("incremental review"));
        assert!(incremental.contains("GitHub review 17"));
        assert!(incremental.contains("no code changes"));
        for prompt in [full, incremental] {
            assert!(prompt.contains(&head.to_string()));
            assert!(prompt.contains("owner/repo#42"));
            assert!(prompt.contains("untrusted"));
        }
        assert_eq!(scope(ReviewMode::Auto, None), ReviewScope::Full);
    }
}
